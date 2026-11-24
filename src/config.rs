use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a machine mutation waits to acquire the routing-store lock before
/// reporting it busy (`LINKS_STORE_LOCKED`). Human commands wait without a
/// bound (see [`AppConfig::lock_routing_blocking`]).
pub const ROUTING_LOCK_TRY_TIMEOUT: Duration = Duration::from_secs(5);

/// Monotonic suffix so two atomic writes from one process never choose the
/// same temp name. The exclusive create (`create_new`) is the real guard; this
/// only keeps the common path collision-free.
static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Replace `path` with `content` so a concurrent reader only ever sees the
/// complete old file or the complete new one — never a half-written store.
///
/// Writes a sibling temp file on the *same* directory (so the final step is a
/// same-filesystem rename, which is atomic on both Windows — `MoveFileExW`
/// with replace — and Unix `rename(2)`), fsyncs the data before the rename,
/// and removes the temp file on any failure. This guarantees **no torn or
/// partial store is ever visible** and that a crash between steps leaves the
/// previous store intact. It does **not** promise power-loss durability of the
/// rename itself: the containing directory is not fsynced (there is no portable
/// way to on Windows), so a power loss immediately after a successful rename
/// may, on some platforms, lose the just-committed file and keep the old one —
/// an old-or-new outcome, still never a torn one.
fn write_file_atomic(path: &Path, content: &str) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "store path has no parent directory",
        )
    })?;
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "store path has no file name")
    })?;
    let seq = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{file_name}.tmp.{}.{seq}", std::process::id()));

    // Exclusive create: never adopt or truncate an existing temp.
    let result = (|| -> io::Result<()> {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();

    if result.is_err() {
        // Best-effort cleanup; the original store is untouched either way.
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Account names: ASCII letters, digits, hyphens, underscores. Rejects path
/// separators, regex metacharacters, whitespace, and unicode — anything
/// unsafe in a filesystem path or shell expansion.
pub fn validate_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether `name` is the reserved "default" name — not a real managed
/// account directory, used across commands to mean "the standard ~/.claude
/// account". `add`, `remove`, and `clone-settings` reject it as a target
/// (there's nothing to add/remove/clone-into for the standard account).
pub fn is_reserved_name(name: &str) -> bool {
    name == "default"
}

pub struct AppConfig {
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLink {
    /// Original spelling retained in the links file for display.
    pub directory: String,
    pub account: String,
}

#[derive(Debug)]
pub enum LinkResolveError {
    Io(io::Error),
    Ambiguous {
        directory: String,
        mappings: Vec<(String, String)>,
    },
}

impl fmt::Display for LinkResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Ambiguous { directory, .. } => {
                write!(f, "ambiguous account links for {directory}")
            }
        }
    }
}

impl std::error::Error for LinkResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Ambiguous { .. } => None,
        }
    }
}

impl From<io::Error> for LinkResolveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn parse_link_line(line: &str) -> Option<(String, String)> {
    let (directory, account) = line.rsplit_once('=')?;
    let directory = directory.trim();
    let account = account.trim();
    if directory.is_empty() || account.is_empty() {
        return None;
    }
    Some((directory.to_string(), account.to_string()))
}

/// A line of the `links` file that `parse_link_line` could not make sense
/// of — 1-indexed so it matches what a human opening the file in an editor
/// sees, plus the exact raw text so the caller can show it without a second
/// file read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedLinkLine {
    pub line_number: usize,
    pub raw: String,
}

/// The outcome of reading the `links` file without the human command's
/// silent-skip behaviour (`all_links`/`parse_link_line`'s `filter_map`):
/// every line either parses, or the read as a whole reports exactly which
/// lines did not — never a partial map presented as complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictLinksRead {
    Ok(Vec<(String, String)>),
    Malformed(Vec<MalformedLinkLine>),
}

impl AppConfig {
    pub fn new() -> Self {
        let home = dirs::home_dir().expect("Cannot determine home directory");
        Self {
            base_dir: home.join(".claude-switch"),
        }
    }

    pub fn accounts_dir(&self) -> PathBuf {
        self.base_dir.join("accounts")
    }

    pub fn config_path(&self) -> PathBuf {
        self.base_dir.join("config")
    }

    pub fn links_path(&self) -> PathBuf {
        self.base_dir.join("links")
    }

    pub fn init(&self) -> io::Result<()> {
        fs::create_dir_all(self.accounts_dir())?;
        if !self.config_path().exists() {
            fs::write(self.config_path(), "default=\n")?;
        }
        if !self.links_path().exists() {
            fs::write(self.links_path(), "")?;
        }
        // Migration: repos → links
        let old_repos = self.base_dir.join("repos");
        if old_repos.exists()
            && fs::read_to_string(self.links_path())
                .map(|s| s.is_empty())
                .unwrap_or(true)
        {
            fs::rename(&old_repos, self.links_path())?;
        }
        Ok(())
    }

    pub fn account_path(&self, name: &str) -> PathBuf {
        self.accounts_dir().join(name)
    }

    pub fn account_exists(&self, name: &str) -> bool {
        self.account_path(name).is_dir()
    }

    pub fn list_accounts(&self) -> io::Result<Vec<String>> {
        let dir = self.accounts_dir();
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut accounts = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                accounts.push(name.to_string());
            }
        }
        accounts.sort();
        Ok(accounts)
    }

    // --- config file ---

    /// The `key=value` lines of the config file, in file order. Unparseable
    /// lines are dropped rather than preserved — the file is ours, and a
    /// stray line is more likely damage than something worth keeping.
    fn read_settings(&self) -> Vec<(String, String)> {
        let Ok(content) = fs::read_to_string(self.config_path()) else {
            return Vec::new();
        };
        content
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .filter(|(k, _)| !k.is_empty())
            .collect()
    }

    fn write_settings(&self, settings: &[(String, String)]) -> io::Result<()> {
        let content: String = settings
            .iter()
            .map(|(k, v)| format!("{}={}\n", k, v))
            .collect();
        write_file_atomic(&self.config_path(), &content)
    }

    /// The value of `key`, or `None` when it is absent or empty. Empty is
    /// treated as absent so `default=` keeps meaning "no default set".
    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.read_settings()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
            .filter(|v| !v.is_empty())
    }

    /// Set `key`, leaving every other setting in the file untouched. New keys
    /// are appended; existing ones are updated in place.
    pub fn set_setting(&self, key: &str, value: &str) -> io::Result<()> {
        // Hold the routing lock across the read-modify-write: `default=` lives
        // in this file and is routing-relevant, so a concurrent link change or
        // another setting write must not interleave.
        let _lock = self.lock_routing_blocking()?;
        let mut settings = self.read_settings();
        match settings.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value.to_string(),
            None => settings.push((key.to_string(), value.to_string())),
        }
        self.write_settings(&settings)
    }

    pub fn get_default(&self) -> io::Result<Option<String>> {
        Ok(self.get_setting("default"))
    }

    /// Like [`get_default`](Self::get_default), but a config file that exists yet
    /// cannot be read or UTF-8-decoded is an error, not silently "no default".
    ///
    /// The human path's [`read_settings`] deliberately treats an unreadable
    /// config as empty, and that stays unchanged. The machine mutation path,
    /// however, fingerprints and reports the default as part of the route it
    /// shows the user, so it must fail loudly (`CONFIG_STORE_UNREADABLE`) rather
    /// than quietly turn a corrupt config into "no default" and skew the
    /// before/after route and the state token. An absent file is still a clean
    /// `Ok(None)` — that genuinely means no default is configured.
    pub fn read_default_strict(&self) -> io::Result<Option<String>> {
        let content = match fs::read_to_string(self.config_path()) {
            Ok(content) => content,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(content
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.trim(), v.trim()))
            .find(|(k, _)| *k == "default")
            .map(|(_, v)| v.to_string())
            .filter(|v| !v.is_empty()))
    }

    pub fn set_default(&self, name: &str) -> io::Result<()> {
        self.set_setting("default", name)
    }

    pub fn clear_default(&self) -> io::Result<()> {
        self.set_setting("default", "")
    }

    // --- links file ---

    pub fn all_links(&self) -> io::Result<Vec<(String, String)>> {
        let content = fs::read_to_string(self.links_path())?;
        Ok(content.lines().filter_map(parse_link_line).collect())
    }

    /// `all_links`, but surfacing every unparsable line instead of silently
    /// dropping it (`links --json`'s `LINKS_STORE_INVALID` case) — the
    /// human `links` command keeps the permissive behaviour unchanged, this
    /// is a separate, stricter read for the machine contract only.
    pub fn read_links_strict(&self) -> io::Result<StrictLinksRead> {
        let content = fs::read_to_string(self.links_path())?;
        let mut links = Vec::new();
        let mut malformed = Vec::new();
        for (index, line) in content.lines().enumerate() {
            match parse_link_line(line) {
                Some(entry) => links.push(entry),
                None => malformed.push(MalformedLinkLine {
                    line_number: index + 1,
                    raw: line.to_string(),
                }),
            }
        }
        if malformed.is_empty() {
            Ok(StrictLinksRead::Ok(links))
        } else {
            Ok(StrictLinksRead::Malformed(malformed))
        }
    }

    pub fn find_link(&self, dir: &str) -> Result<Option<ResolvedLink>, LinkResolveError> {
        let links = self.all_links()?;
        let matches: Vec<(String, String)> = links
            .into_iter()
            .filter(|(stored, _)| crate::path_identity::equivalent(stored, dir))
            .collect();
        let Some((directory, account)) = matches.first() else {
            return Ok(None);
        };
        if matches.iter().any(|(_, candidate)| candidate != account) {
            return Err(LinkResolveError::Ambiguous {
                directory: dir.to_string(),
                mappings: matches,
            });
        }
        Ok(Some(ResolvedLink {
            directory: directory.clone(),
            account: account.clone(),
        }))
    }

    pub fn set_link(&self, dir: &str, account: &str) -> io::Result<()> {
        let _lock = self.lock_routing_blocking()?;
        let mut links = self.all_links()?;
        links.retain(|(stored, _)| !crate::path_identity::equivalent(stored, dir));
        links.push((dir.to_string(), account.to_string()));
        self.write_links(&links)
    }

    pub fn remove_link(&self, dir: &str) -> io::Result<bool> {
        let _lock = self.lock_routing_blocking()?;
        let mut links = self.all_links()?;
        let before = links.len();
        links.retain(|(stored, _)| !crate::path_identity::equivalent(stored, dir));
        if links.len() == before {
            return Ok(false);
        }
        self.write_links(&links)?;
        Ok(true)
    }

    pub fn remove_links_for_account(&self, account: &str) -> io::Result<()> {
        let _lock = self.lock_routing_blocking()?;
        let mut links = self.all_links()?;
        links.retain(|(_, a)| a != account);
        self.write_links(&links)
    }

    /// Serialize links into the store's `dir=account` line format. Pure, so a
    /// test can pin the exact bytes without touching the filesystem.
    pub(crate) fn serialize_links(links: &[(String, String)]) -> String {
        let content: String = links
            .iter()
            .map(|(d, a)| format!("{}={}", d, a))
            .collect::<Vec<_>>()
            .join("\n");
        if content.is_empty() {
            String::new()
        } else {
            content + "\n"
        }
    }

    /// Atomically persist `links`. **Does not take the routing lock** — the
    /// caller must already hold it (see [`AppConfig::lock_routing_blocking`] /
    /// [`AppConfig::try_lock_routing`]). Exposed to the machine mutation path so
    /// it can write under the same single lock it read and verified under,
    /// rather than re-locking and risking the store moving underneath it.
    pub(crate) fn write_links(&self, links: &[(String, String)]) -> io::Result<()> {
        write_file_atomic(&self.links_path(), &Self::serialize_links(links))
    }

    // --- routing-store lock ---
    //
    // One advisory lock serializes every writer of routing-relevant state —
    // the links store AND the config file's `default=` — so a link change and
    // a default change can never interleave their read-modify-write. It is an
    // OS advisory lock on a dedicated lock file (`flock`/`LockFileEx` via
    // `std::fs::File`), released automatically when the handle closes, so a
    // crashed holder leaves nothing stale and nothing is ever deleted by age.
    // It binds only cooperating writers in this binary: an older binary, a
    // direct hand-edit, or any process that ignores the lock is NOT excluded —
    // documented in docs/machine-api.md, never claimed otherwise.

    pub fn routing_lock_path(&self) -> PathBuf {
        self.base_dir.join(".routing.lock")
    }

    fn open_lock_file(&self) -> io::Result<File> {
        // The base dir must exist (init creates it); the lock file's content is
        // irrelevant, only the handle's advisory lock matters.
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.routing_lock_path())
    }

    /// Acquire the routing lock, waiting without a bound. Used by the human
    /// link/unlink/default/account writers, whose contract is "succeeds unless
    /// a real I/O error": the only holder they can meet is another cooperating
    /// writer finishing a sub-second critical section. The returned handle is
    /// the lock guard — the lock is released when it drops.
    pub fn lock_routing_blocking(&self) -> io::Result<File> {
        let f = self.open_lock_file()?;
        f.lock()?;
        Ok(f)
    }

    /// Acquire the routing lock within [`ROUTING_LOCK_TRY_TIMEOUT`]. `Ok(None)`
    /// means another writer holds it (the machine path maps this to
    /// `LINKS_STORE_LOCKED`). The returned handle is the lock guard.
    pub fn try_lock_routing(&self) -> io::Result<Option<File>> {
        self.try_lock_routing_for(ROUTING_LOCK_TRY_TIMEOUT)
    }

    /// [`try_lock_routing`](Self::try_lock_routing) with an explicit bound, so a
    /// test can exercise contention without waiting the production timeout.
    pub(crate) fn try_lock_routing_for(&self, timeout: Duration) -> io::Result<Option<File>> {
        let f = self.open_lock_file()?;
        let deadline = Instant::now() + timeout;
        loop {
            match f.try_lock() {
                Ok(()) => return Ok(Some(f)),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_alphanumeric() {
        assert!(validate_name("work"));
        assert!(validate_name("personal-2"));
        assert!(validate_name("a_b"));
        assert!(validate_name("WORK"));
        assert!(validate_name("123"));
        assert!(validate_name("a"));
    }

    #[test]
    fn validate_name_rejects_empty() {
        assert!(!validate_name(""));
    }

    #[test]
    fn validate_name_rejects_path_separators_and_traversal() {
        assert!(!validate_name("a/b"));
        assert!(!validate_name("../etc"));
        assert!(!validate_name(".."));
        assert!(!validate_name("."));
    }

    #[test]
    fn validate_name_rejects_whitespace() {
        assert!(!validate_name("a b"));
        assert!(!validate_name("a\tb"));
        assert!(!validate_name(" leading"));
    }

    #[test]
    fn validate_name_rejects_regex_metachars() {
        assert!(!validate_name("a.b"));
        assert!(!validate_name("a[b"));
        assert!(!validate_name("a+b"));
        assert!(!validate_name("a*b"));
    }

    #[test]
    fn validate_name_rejects_links_format_break() {
        // `=` would corrupt the path=name links file format.
        assert!(!validate_name("a=b"));
    }

    #[test]
    fn validate_name_rejects_unicode() {
        assert!(!validate_name("работа"));
        assert!(!validate_name("café"));
    }

    fn temp_config(tag: &str) -> AppConfig {
        let base = std::env::temp_dir().join(format!("cc-config-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let config = AppConfig { base_dir: base };
        config.init().unwrap();
        config
    }

    #[test]
    fn setting_round_trips() {
        let c = temp_config("round-trip");
        assert_eq!(c.get_setting("resume_hook"), None);
        c.set_setting("resume_hook", "off").unwrap();
        assert_eq!(c.get_setting("resume_hook").as_deref(), Some("off"));
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn setting_an_empty_value_reads_back_as_absent() {
        // `default=` is how "no default account" is stored, so an empty
        // value has to be indistinguishable from a missing key.
        let c = temp_config("empty");
        c.set_setting("default", "").unwrap();
        assert_eq!(c.get_setting("default"), None);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn setting_one_key_leaves_the_others_alone() {
        // Regression: set_default used to rewrite the whole file, which would
        // silently drop every other setting.
        let c = temp_config("preserve");
        c.set_setting("resume_hook", "off").unwrap();
        c.set_default("work").unwrap();
        assert_eq!(c.get_setting("resume_hook").as_deref(), Some("off"));
        assert_eq!(c.get_default().unwrap().as_deref(), Some("work"));

        c.clear_default().unwrap();
        assert_eq!(c.get_setting("resume_hook").as_deref(), Some("off"));
        assert_eq!(c.get_default().unwrap(), None);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn updating_a_key_does_not_append_a_duplicate() {
        let c = temp_config("no-dup");
        c.set_setting("resume_hook", "off").unwrap();
        c.set_setting("resume_hook", "on").unwrap();
        let raw = fs::read_to_string(c.config_path()).unwrap();
        assert_eq!(raw.matches("resume_hook=").count(), 1, "{}", raw);
        assert_eq!(c.get_setting("resume_hook").as_deref(), Some("on"));
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn link_parser_uses_the_rightmost_delimiter() {
        assert_eq!(
            parse_link_line(r"C:\repo=work"),
            Some((r"C:\repo".to_string(), "work".to_string()))
        );
        assert_eq!(
            parse_link_line(r"C:\repo=a=work"),
            Some((r"C:\repo=a".to_string(), "work".to_string()))
        );
        assert_eq!(
            parse_link_line(r"C:\a=b=c=personal1"),
            Some((r"C:\a=b=c".to_string(), "personal1".to_string()))
        );
    }

    #[test]
    fn link_parser_ignores_malformed_lines() {
        assert_eq!(parse_link_line("missing-delimiter"), None);
        assert_eq!(parse_link_line("=work"), None);
        assert_eq!(parse_link_line(r"C:\repo="), None);
    }

    #[test]
    fn equals_path_round_trips_without_rewriting() {
        let c = temp_config("equals-path");
        c.set_link(r"C:\repo=a=b", "work").unwrap();
        assert_eq!(
            c.all_links().unwrap(),
            vec![(r"C:\repo=a=b".to_string(), "work".to_string())]
        );
        let raw = fs::read_to_string(c.links_path()).unwrap();
        assert_eq!(raw, "C:\\repo=a=b=work\n");
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[cfg(windows)]
    #[test]
    fn equivalent_aliases_for_one_account_resolve_without_ambiguity() {
        let c = temp_config("same-account-aliases");
        fs::write(c.links_path(), "C:\\Work=personal1\nc:/work/=personal1\n").unwrap();
        let link = c.find_link(r"c:\WORK").unwrap().unwrap();
        assert_eq!(link.directory, r"C:\Work");
        assert_eq!(link.account, "personal1");
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[cfg(windows)]
    #[test]
    fn equivalent_aliases_for_different_accounts_are_ambiguous() {
        let c = temp_config("conflicting-aliases");
        fs::write(c.links_path(), "C:\\Work=personal1\nc:/work/=work\n").unwrap();
        match c.find_link(r"c:\WORK") {
            Err(LinkResolveError::Ambiguous { mappings, .. }) => {
                assert_eq!(mappings.len(), 2)
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[cfg(windows)]
    #[test]
    fn setting_a_link_heals_aliases_and_preserves_unrelated_entries() {
        let c = temp_config("heal-aliases");
        fs::write(
            c.links_path(),
            "C:\\Work=personal1\nc:/work/=work\nC:\\Work-child=other\n",
        )
        .unwrap();
        c.set_link(r"c:\WORK", "selected").unwrap();
        assert_eq!(
            c.all_links().unwrap(),
            vec![
                (r"C:\Work-child".to_string(), "other".to_string()),
                (r"c:\WORK".to_string(), "selected".to_string())
            ]
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[cfg(windows)]
    #[test]
    fn unlink_removes_all_equivalent_aliases_only() {
        let c = temp_config("unlink-aliases");
        fs::write(
            c.links_path(),
            "C:\\Work=personal1\nc:/work/=work\nC:\\Work-child=other\n",
        )
        .unwrap();
        assert!(c.remove_link(r"c:\WORK").unwrap());
        assert_eq!(
            c.all_links().unwrap(),
            vec![(r"C:\Work-child".to_string(), "other".to_string())]
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn removing_an_account_still_removes_every_stored_path() {
        let c = temp_config("remove-account-links");
        fs::write(
            c.links_path(),
            "/one=work\n/path=with=equals=work\n/other=personal\n",
        )
        .unwrap();
        c.remove_links_for_account("work").unwrap();
        assert_eq!(
            c.all_links().unwrap(),
            vec![("/other".to_string(), "personal".to_string())]
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn read_links_strict_ok_on_an_empty_store() {
        let c = temp_config("strict-empty");
        assert_eq!(c.read_links_strict().unwrap(), StrictLinksRead::Ok(vec![]));
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn read_links_strict_ok_on_well_formed_lines() {
        let c = temp_config("strict-ok");
        fs::write(c.links_path(), "/one=work\n/two=personal\n").unwrap();
        assert_eq!(
            c.read_links_strict().unwrap(),
            StrictLinksRead::Ok(vec![
                ("/one".to_string(), "work".to_string()),
                ("/two".to_string(), "personal".to_string()),
            ])
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn read_links_strict_reports_every_malformed_line_with_its_number() {
        let c = temp_config("strict-malformed");
        fs::write(
            c.links_path(),
            "/one=work\nmissing-delimiter\n/two=personal\n=no-directory\n",
        )
        .unwrap();
        match c.read_links_strict().unwrap() {
            StrictLinksRead::Malformed(bad) => {
                assert_eq!(
                    bad,
                    vec![
                        MalformedLinkLine {
                            line_number: 2,
                            raw: "missing-delimiter".to_string()
                        },
                        MalformedLinkLine {
                            line_number: 4,
                            raw: "=no-directory".to_string()
                        },
                    ]
                );
            }
            other => panic!("expected malformed lines, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn is_reserved_name_matches_only_default() {
        assert!(is_reserved_name("default"));
        assert!(!is_reserved_name("work"));
        assert!(!is_reserved_name("Default"));
        assert!(!is_reserved_name(""));
    }

    // --- atomic replacement ---

    #[test]
    fn write_file_atomic_replaces_content_and_leaves_no_temp_behind() {
        let c = temp_config("atomic-replace");
        let path = c.links_path();
        write_file_atomic(&path, "first\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\n");
        write_file_atomic(&path, "second\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
        // No .links.tmp.* sibling should survive a successful write.
        let leftovers: Vec<_> = fs::read_dir(c.base_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file may be left behind: {leftovers:?}"
        );
    }

    #[test]
    fn a_concurrent_reader_only_ever_sees_a_complete_store_never_a_torn_one() {
        // The whole point of the temp-file + rename: a reader opening the store
        // mid-rewrite sees the complete old or complete new file, never a
        // half-written one. A bare fs::write can be caught truncated here.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let c = Arc::new(temp_config("atomic-reader"));
        let state_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n".to_string();
        let state_b = "b\n".to_string();
        write_file_atomic(&c.links_path(), &state_a).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (c, stop, a, b) = (c.clone(), stop.clone(), state_a.clone(), state_b.clone());
            std::thread::spawn(move || {
                for i in 0..600 {
                    let content = if i % 2 == 0 { &a } else { &b };
                    write_file_atomic(&c.links_path(), content).unwrap();
                }
                stop.store(true, Ordering::Relaxed);
            })
        };
        let path = c.links_path();
        while !stop.load(Ordering::Relaxed) {
            if let Ok(seen) = fs::read_to_string(&path) {
                assert!(
                    seen == state_a || seen == state_b,
                    "reader saw a torn store: {seen:?}"
                );
            }
        }
        writer.join().unwrap();
    }

    // --- the routing lock ---

    #[test]
    fn the_routing_lock_is_exclusive_and_releases_on_drop() {
        let c = temp_config("lock-exclusive");
        let held = c.try_lock_routing().unwrap().expect("first acquirer wins");
        // A second acquirer, via its own handle, must not get the lock while it
        // is held — reported as busy within the (short) bound rather than hung.
        assert!(
            c.try_lock_routing_for(Duration::from_millis(80))
                .unwrap()
                .is_none(),
            "a held lock must block a second acquirer"
        );
        drop(held);
        assert!(
            c.try_lock_routing().unwrap().is_some(),
            "the lock must be available again once the guard drops"
        );
    }

    #[test]
    fn concurrent_writers_never_lose_each_others_links() {
        // Each thread links a different directory. Without the shared lock the
        // read-modify-write would race and one link would be lost; with it,
        // every thread's link survives.
        use std::sync::Arc;
        let c = Arc::new(temp_config("concurrent-writers"));
        let mut handles = Vec::new();
        for i in 0..8 {
            let c = c.clone();
            handles.push(std::thread::spawn(move || {
                c.set_link(&format!("/dir/{i}"), "work").unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let mut links = c.all_links().unwrap();
        links.sort();
        let mut expected: Vec<_> = (0..8)
            .map(|i| (format!("/dir/{i}"), "work".to_string()))
            .collect();
        expected.sort();
        assert_eq!(links, expected, "no writer's link may be lost");
    }

    #[test]
    fn a_blocking_human_writer_waits_for_a_held_lock_then_succeeds() {
        use std::sync::Arc;
        let c = Arc::new(temp_config("blocking-writer"));
        let held = c.try_lock_routing().unwrap().expect("acquire");
        let writer = {
            let c = c.clone();
            std::thread::spawn(move || {
                // Blocks until the lock below is released, then writes.
                c.set_link("/late", "work").unwrap();
            })
        };
        // Give the writer time to be genuinely blocked, then release.
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            c.all_links().unwrap().is_empty(),
            "the writer must not have written yet"
        );
        drop(held);
        writer.join().unwrap();
        assert_eq!(
            c.all_links().unwrap(),
            vec![("/late".to_string(), "work".to_string())]
        );
    }
}
