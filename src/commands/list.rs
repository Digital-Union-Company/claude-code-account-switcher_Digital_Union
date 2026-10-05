use std::path::Path;

use serde_json::json;

use crate::chrome;
use crate::config::AppConfig;
use crate::i18n::{I18n, Msg};
use crate::identity::{self, CachedInfo};
use crate::machine;

pub fn run(config: &AppConfig, i18n: &I18n) {
    let default_acc = config.get_default().ok().flatten();
    let accounts = config.list_accounts().unwrap_or_default();

    // Standard ~/.claude/ shows up only if the user has actually logged into
    // the standard config — so empty installs don't get a phantom row.
    let standard = identity::standard_token_dir();
    let standard_logged_in = standard
        .as_deref()
        .and_then(identity::current_token_hash)
        .is_some()
        || identity::read_cache_at(&identity::default_cache_path(&config.base_dir)).is_some();

    if accounts.is_empty() && !standard_logged_in {
        i18n.print(Msg::ListEmpty);
        return;
    }

    i18n.print(Msg::ListHeader);

    for acc in &accounts {
        let acc_dir = config.account_path(acc);
        let cache = identity::read_cache(&acc_dir);
        let info_suffix = cache_suffix(
            cache.as_ref(),
            Some(acc_dir.as_path()),
            i18n,
            /* skip_label */ "",
        );
        if Some(acc.as_str()) == default_acc.as_deref() {
            println!("  ★ {}  {}{}", acc, i18n.msg(Msg::ListDefault), info_suffix);
        } else {
            println!("    {}{}", acc, info_suffix);
        }
    }

    if standard_logged_in {
        let cache_path = identity::default_cache_path(&config.base_dir);
        let cache = identity::read_cache_at(&cache_path);
        let suffix = cache_suffix(
            cache.as_ref(),
            standard.as_deref(),
            i18n,
            &format!("  {}", i18n.msg(Msg::ListStandard)),
        );
        println!("    ~/.claude/{}", suffix);
    }
}

/// `claude-acc list --json` — docs/machine-api.md's fast local account
/// inventory. Never performs a live identity/profile request: every field
/// comes from data claude-acc already keeps on disk (the account directory
/// listing, the `.account-info.json` cache, a local token-presence check,
/// and the Chrome-in-Claude flag), which is what keeps this cheap enough to
/// call on every GUI startup.
pub fn run_json(config: &AppConfig) -> i32 {
    match build_account_entries(config) {
        Ok(entries) => machine::success(json!({ "accounts": entries })),
        Err(e) => machine::error("CONFIG_STORE_UNREADABLE", &e.to_string(), json!({})),
    }
}

/// The decision half of `run_json`, kept separate from printing so it can be
/// asserted against directly rather than by capturing stdout.
fn build_account_entries(config: &AppConfig) -> std::io::Result<Vec<serde_json::Value>> {
    let accounts = config.list_accounts()?;
    let default_acc = config.get_default().ok().flatten();

    let mut entries: Vec<serde_json::Value> = accounts
        .iter()
        .map(|acc| {
            let acc_dir = config.account_path(acc);
            let cache = identity::read_cache(&acc_dir);
            account_entry(
                acc,
                &acc_dir,
                cache,
                Some(default_acc.as_deref() == Some(acc.as_str())),
                true,
            )
        })
        .collect();

    // Same visibility rule as the human command: the standard ~/.claude/ row
    // only appears once it has actually been used, so an empty install
    // never gets a phantom entry.
    let standard = identity::standard_token_dir();
    let standard_cache = identity::read_cache_at(&identity::default_cache_path(&config.base_dir));
    let standard_logged_in = standard
        .as_deref()
        .and_then(identity::current_token_hash)
        .is_some()
        || standard_cache.is_some();
    if standard_logged_in && let Some(dir) = standard.as_deref() {
        entries.push(account_entry(
            crate::sessions::DEFAULT_LABEL,
            dir,
            standard_cache,
            Some(false),
            false,
        ));
    }

    Ok(entries)
}

/// Build one `accounts[]` entry. `managed` distinguishes a real account
/// directory from the standard, unmanaged `~/.claude/` row. `cache` is
/// passed in rather than re-read from `config_dir`, since the standard
/// account's cache lives beside claude-acc's own state
/// (`identity::default_cache_path`), not inside `config_dir` itself.
fn account_entry(
    name: &str,
    config_dir: &Path,
    cache: Option<CachedInfo>,
    is_default: Option<bool>,
    managed: bool,
) -> serde_json::Value {
    let current_hash = identity::current_token_hash(config_dir);
    let auth_present = current_hash.is_some();
    let token_changed_since_audit = match (
        cache.as_ref().and_then(|c| c.token_hash.as_deref()),
        current_hash.as_deref(),
    ) {
        (Some(cached), Some(current)) => cached != current,
        _ => false,
    };

    json!({
        "name": name,
        "managed": managed,
        "default": is_default.unwrap_or(false),
        "auth_present": auth_present,
        "cached_email": cache.as_ref().and_then(|c| c.email.clone()),
        "cached_uuid": cache.as_ref().and_then(|c| c.uuid.clone()),
        "cached_plan": cache.as_ref().and_then(|c| c.plan.clone()),
        "token_changed_since_audit": token_changed_since_audit,
        "chrome_enabled": chrome::enabled(config_dir),
        "config_dir": config_dir.display().to_string(),
    })
}

/// Returns "  email  3d ago [skip_label]" or "  email  3d ago * [skip_label]"
/// if cache is present, else just `skip_label` if any.
///
/// `*` means current token at `token_dir` differs from the one cached at
/// last `doctor` run (see README — usually a routine OAuth refresh).
/// `skip_label` is appended after the time/marker, used by the standard row
/// to add `(standard)`.
fn cache_suffix(
    cache: Option<&CachedInfo>,
    token_dir: Option<&Path>,
    i18n: &I18n,
    skip_label: &str,
) -> String {
    let Some(cache) = cache else {
        return skip_label.to_string();
    };
    let Some(email) = cache.email.as_deref() else {
        return skip_label.to_string();
    };
    let when = cache
        .fetched_at
        .and_then(identity::seconds_since)
        .map(|secs| i18n.msg(Msg::RelativeTime(secs)))
        .unwrap_or_default();

    let drift = match (
        cache.token_hash.as_deref(),
        token_dir.and_then(identity::current_token_hash),
    ) {
        (Some(cached), Some(current)) if cached != current => " *",
        _ => "",
    };

    let plan = cache
        .plan
        .as_deref()
        .map(|p| format!("  {}", p))
        .unwrap_or_default();

    let body = if when.is_empty() {
        format!("  {}{}{}", email, plan, drift)
    } else {
        format!("  {}{}  {}{}", email, plan, when, drift)
    };
    format!("{}{}", body, skip_label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> AppConfig {
        let dir = std::env::temp_dir().join(format!("cc-list-json-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let config = AppConfig { base_dir: dir };
        config.init().unwrap();
        config
    }

    // The standard ~/.claude/ row, when present, always refers to the real
    // machine's home directory (`identity::standard_token_dir`) — there is
    // no scratch-dir equivalent for it. So these tests assert only about
    // the managed (`"managed": true`) entries they themselves created,
    // never about the total count or about whether a standard-account row
    // is present, which depends on whatever this machine happens to have.

    #[test]
    fn build_account_entries_has_no_managed_rows_when_none_exist() {
        let config = scratch("empty");
        let entries = build_account_entries(&config).unwrap();
        assert!(entries.iter().all(|e| e["managed"] == false), "{entries:?}");
        let _ = fs::remove_dir_all(&config.base_dir);
    }

    #[test]
    fn build_account_entries_lists_every_managed_account_with_the_right_default_flag() {
        let config = scratch("managed");
        fs::create_dir_all(config.account_path("work")).unwrap();
        fs::create_dir_all(config.account_path("personal")).unwrap();
        config.set_default("work").unwrap();

        let entries = build_account_entries(&config).unwrap();
        let managed: Vec<&serde_json::Value> =
            entries.iter().filter(|e| e["managed"] == true).collect();
        assert_eq!(managed.len(), 2, "{managed:?}");
        let work = managed.iter().find(|e| e["name"] == "work").unwrap();
        assert_eq!(work["managed"], true);
        assert_eq!(work["default"], true);
        assert_eq!(work["auth_present"], false);
        assert_eq!(work["cached_email"], serde_json::Value::Null);
        let personal = managed.iter().find(|e| e["name"] == "personal").unwrap();
        assert_eq!(personal["default"], false);
        let _ = fs::remove_dir_all(&config.base_dir);
    }

    #[test]
    fn account_entry_never_contains_a_token_token_hash_or_credential_field() {
        let dir = std::env::temp_dir().join(format!("cc-list-json-secret-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let cache = CachedInfo {
            email: Some("a@example.com".to_string()),
            uuid: Some("u-1".to_string()),
            org: None,
            fetched_at: Some(1_000),
            token_hash: Some("deadbeefdeadbeef".to_string()),
            plan: Some("Max 20x".to_string()),
        };
        let entry = account_entry("work", &dir, Some(cache), Some(false), true);
        let dump = entry.to_string();
        assert!(!dump.contains("deadbeef"), "{dump}");
        for forbidden in ["token", "credential", "access_token", "token_hash"] {
            assert!(
                !entry.as_object().unwrap().contains_key(forbidden),
                "entry must not expose a {forbidden:?} field: {entry:?}"
            );
        }
        assert_eq!(entry["cached_plan"], "Max 20x");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn account_entry_token_changed_since_audit_requires_both_sides_present_and_different() {
        let dir = std::env::temp_dir().join(format!("cc-list-json-drift-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // No cache at all: nothing to compare, never reported as changed.
        let entry = account_entry("work", &dir, None, Some(false), true);
        assert_eq!(entry["token_changed_since_audit"], false);

        // A cache with no token hash recorded: same — nothing to compare.
        let cache = CachedInfo {
            email: None,
            uuid: None,
            org: None,
            fetched_at: None,
            token_hash: None,
            plan: None,
        };
        let entry = account_entry("work", &dir, Some(cache), Some(false), true);
        assert_eq!(entry["token_changed_since_audit"], false);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn account_entry_reports_the_standard_accounts_stable_name_and_unmanaged_flag() {
        let dir =
            std::env::temp_dir().join(format!("cc-list-json-standard-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let entry = account_entry(
            crate::sessions::DEFAULT_LABEL,
            &dir,
            None,
            Some(false),
            false,
        );
        assert_eq!(entry["name"], "default");
        assert_eq!(entry["managed"], false);
        assert_eq!(entry["default"], false);
        let _ = fs::remove_dir_all(&dir);
    }

    // `auth_present` is grounded in a real, local token-presence check
    // (identity::current_token_hash) — exercised here via the plaintext
    // `.credentials.json` fallback, which works on every platform without
    // a keychain or network call.
    #[test]
    fn account_entry_auth_present_true_with_a_real_token_file_and_no_cache() {
        let dir = std::env::temp_dir().join(format!("cc-list-json-auth-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(".credentials.json"),
            serde_json::json!({"claudeAiOauth": {"accessToken": "fake-token"}}).to_string(),
        )
        .unwrap();

        let entry = account_entry("work", &dir, None, Some(false), true);
        assert_eq!(entry["auth_present"], true);
        assert_eq!(entry["cached_email"], serde_json::Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn account_entry_chrome_enabled_reflects_true_false_and_null() {
        let on =
            std::env::temp_dir().join(format!("cc-list-json-chrome-on-{}", std::process::id()));
        let off =
            std::env::temp_dir().join(format!("cc-list-json-chrome-off-{}", std::process::id()));
        let none =
            std::env::temp_dir().join(format!("cc-list-json-chrome-none-{}", std::process::id()));
        for dir in [&on, &off, &none] {
            let _ = fs::remove_dir_all(dir);
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            on.join(".claude.json"),
            r#"{"claudeInChromeDefaultEnabled": true}"#,
        )
        .unwrap();
        fs::write(
            off.join(".claude.json"),
            r#"{"claudeInChromeDefaultEnabled": false}"#,
        )
        .unwrap();
        // `none` has no .claude.json at all yet.

        assert_eq!(
            account_entry("on", &on, None, Some(false), true)["chrome_enabled"],
            true
        );
        assert_eq!(
            account_entry("off", &off, None, Some(false), true)["chrome_enabled"],
            false
        );
        assert_eq!(
            account_entry("none", &none, None, Some(false), true)["chrome_enabled"],
            serde_json::Value::Null
        );

        for dir in [&on, &off, &none] {
            let _ = fs::remove_dir_all(dir);
        }
    }
}
