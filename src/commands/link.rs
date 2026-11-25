use crate::config::{AppConfig, StrictLinksRead, validate_name};
use crate::i18n::{I18n, Msg};
use crate::machine;
use crate::path_identity;
use crate::resolve::{self, EffectiveRoute, RouteSource};
use crate::sessions::DEFAULT_LABEL;
use serde_json::json;
use sha2::{Digest, Sha256};

pub fn run(config: &AppConfig, i18n: &I18n, name: &str) {
    if name != "default" && !validate_name(name) {
        i18n.print(Msg::NameInvalid);
        std::process::exit(1);
    }

    let dir = std::env::current_dir().expect("Cannot get current directory");
    let dir_str = dir.to_str().expect("Invalid directory path");
    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or(dir_str);

    // Validate the account exists and write the link together under the routing
    // lock, so a concurrent `remove <name>` cannot be accepted between the check
    // and the write and leave a link to a now-deleted account.
    match config.link_checked(dir_str, name) {
        Ok(true) => {
            if name == "default" {
                i18n.print(Msg::LinkDoneDefault(dir_name.to_string()));
            } else {
                i18n.print(Msg::LinkDone(dir_name.to_string(), name.to_string()));
            }
        }
        Ok(false) => {
            i18n.print(Msg::LinkNotFound(name.to_string()));
            super::list::run(config, i18n);
            std::process::exit(1);
        }
        Err(_) => panic!("Failed to update links"),
    }
}

/// Fingerprint of every routing-relevant input the preview was computed
/// against: the requested directory and account, the full links store, the
/// configured default, the account inventory, the resolved before-route, the
/// complete set of `dir`'s own exact-equivalent entries, and `dir`'s own
/// filesystem identity. Returned opaquely as `expected_state` by `--dry-run`
/// and replayed by `--expect-state` so the commit proceeds only if nothing
/// routing-relevant moved in between.
///
/// It is a conservative whole-store STATE FINGERPRINT, deliberately not
/// authentication and not a transaction id: an identical restored state yields
/// an identical token, so it cannot distinguish "never changed" from "changed
/// and changed back" (ABA). It refuses on ANY routing-store change, including
/// ones unrelated to this directory — the safe direction. It carries no
/// credential or credential-derived value: its inputs are directory strings,
/// account names and the stored map, none of which are secrets.
#[allow(clippy::too_many_arguments)]
fn state_token(
    dir: &str,
    account: &str,
    links: &[(String, String)],
    default: Option<&str>,
    accounts: &[String],
    before: &EffectiveRoute,
    affected: &[(String, String)],
    dir_identity: Option<&str>,
) -> String {
    let mut h = Sha256::new();
    h.update(b"claude-acc-link-state-v3\0");
    h.update(dir.as_bytes());
    h.update([0]);
    h.update(account.as_bytes());
    h.update([0]);
    h.update(b"links\0");
    for (d, a) in links {
        h.update(d.as_bytes());
        h.update(b"=");
        h.update(a.as_bytes());
        h.update([0]);
    }
    h.update(b"default\0");
    h.update(default.unwrap_or("").as_bytes());
    h.update([0]);
    h.update(b"accounts\0");
    for acc in accounts {
        h.update(acc.as_bytes());
        h.update([0]);
    }
    // The actual routing INTERPRETATION, not only the textual store: `before`
    // is claude-acc's resolved effective route, `affected` is the COMPLETE
    // ordered set of dir's exact-equivalence entries the write will drop and
    // replace (not merely the first), and `dir_identity` is dir's own resolved
    // filesystem identity. All come from the resolver and `path_identity`,
    // which can consult live filesystem identity (`canonicalize`), so a
    // reparse/junction retarget that changes how dir is interpreted — or which
    // stored entries fall in its equivalence class — without changing any
    // stored string still flips the token and refuses the commit, rather than
    // acting on a stale interpretation or modifying a different set of entries
    // than the one previewed.
    h.update(b"before\0");
    h.update(before.resolved_account.as_bytes());
    h.update([0]);
    h.update(before.source.as_str().as_bytes());
    h.update([0]);
    h.update(before.owning_link_path.as_deref().unwrap_or("").as_bytes());
    h.update([0]);
    // Length-prefixed so the set boundary cannot be forged by run-together
    // entries, then every member in stored order.
    h.update(b"affected\0");
    h.update((affected.len() as u64).to_le_bytes());
    for (stored, acc) in affected {
        h.update(stored.as_bytes());
        h.update(b"=");
        h.update(acc.as_bytes());
        h.update([0]);
    }
    h.update(b"dir-identity\0");
    h.update(dir_identity.unwrap_or("<none>").as_bytes());
    h.update([0]);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `dir=account` survives a round-trip through the links-file format
/// unchanged. Rejects a directory that embeds a newline (which would split
/// across lines) or whitespace the `key=value` parser would trim, so the store
/// can never be corrupted by a path it cannot represent losslessly.
fn representable_in_store(dir: &str, account: &str) -> bool {
    if dir.contains('\n') || dir.contains('\r') {
        return false;
    }
    match crate::config::parse_link_line(&format!("{dir}={account}")) {
        Some((parsed_dir, parsed_account)) => parsed_dir == dir && parsed_account == account,
        None => false,
    }
}

fn route_json(route: &EffectiveRoute) -> serde_json::Value {
    json!({
        "resolved_account": route.resolved_account,
        "source": route.source.as_str(),
        "owning_link_path": route.owning_link_path,
    })
}

/// `claude-acc link --json --path <DIR> --account <ACCOUNT>` with exactly one of
/// `--dry-run` (preview) or `--expect-state <TOKEN>` (conditional commit).
///
/// The one machine-API mutation (docs/machine-api.md). It ensures only `<DIR>`'s
/// own exact-equivalent link names `<ACCOUNT>`, changing nothing else — never an
/// ancestor, descendant or unrelated mapping. The whole read → plan → write →
/// verify runs under a single routing-store lock; the commit is conditional on
/// the `--expect-state` token still matching. All routing and path-equivalence
/// decisions reuse the authoritative resolver and `path_identity`; this adds no
/// routing rule of its own.
pub fn run_json(
    config: &AppConfig,
    path: Option<&str>,
    account: Option<&str>,
    dry_run: bool,
    expect_state: Option<&str>,
) -> i32 {
    // Exactly one mode, and both machine inputs present. These are JSON
    // envelopes rather than clap usage errors so a consumer never has to parse
    // stderr to tell preview from commit from a bad call.
    if dry_run == expect_state.is_some() {
        return machine::error(
            "INVALID_ARGUMENTS",
            "exactly one of --dry-run or --expect-state <token> is required",
            json!({}),
        );
    }
    let (Some(path), Some(account)) = (path, account) else {
        return machine::error(
            "INVALID_ARGUMENTS",
            "--path and --account are required with --json",
            json!({}),
        );
    };

    // Account must be a valid managed name or the literal standard account.
    if account != DEFAULT_LABEL && !validate_name(account) {
        return machine::error(
            "ACCOUNT_NAME_INVALID",
            "account name is not a valid managed account name",
            json!({ "account": account }),
        );
    }

    // Resolve + validate the directory exactly as `status --json` does.
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => return machine::error("PATH_NOT_FOUND", &e.to_string(), json!({})),
    };
    let query_path = match crate::commands::status::resolve_query_path(Some(path), &cwd) {
        Ok(p) => p,
        Err((code, message)) => return machine::error(code, &message, json!({})),
    };
    let Some(dir) = query_path.to_str() else {
        return machine::error(
            "PATH_NOT_REPRESENTABLE",
            "directory path is not valid UTF-8 and cannot be stored",
            json!({ "query_path": query_path.to_string_lossy() }),
        );
    };
    if !representable_in_store(dir, account) {
        return machine::error(
            "PATH_NOT_REPRESENTABLE",
            "directory path cannot be represented in the links store without corruption",
            json!({ "query_path": dir }),
        );
    }

    // The machine path never initializes the store (main.rs skips init for it),
    // so an absent store is a structured refusal — never a baseline this command
    // (including a dry-run) manufactures as a side effect.
    if !config.links_path().exists() {
        return machine::error(
            "NOT_INITIALIZED",
            "claude-acc has no routing store yet; run a claude-acc command to initialize it first",
            json!({}),
        );
    }

    // One lock for the whole read → plan → write → verify. Bounded: a busy lock
    // is a cooperating writer mid-transaction, reported rather than waited on.
    let _lock = match config.try_lock_routing() {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            return machine::error(
                "LINKS_STORE_LOCKED",
                "another routing-store writer holds the lock; nothing was changed",
                json!({}),
            );
        }
        Err(e) => return machine::error("LINKS_STORE_UNREADABLE", &e.to_string(), json!({})),
    };

    // Strict read: a malformed store is refused, never silently rewritten.
    let links = match config.read_links_strict() {
        Ok(StrictLinksRead::Ok(links)) => links,
        Ok(StrictLinksRead::Malformed(lines)) => {
            let lines: Vec<serde_json::Value> = lines
                .iter()
                .map(|l| json!({ "line_number": l.line_number, "raw": l.raw }))
                .collect();
            return machine::error(
                "LINKS_STORE_INVALID",
                "the links store has malformed lines; nothing was changed",
                json!({ "lines": lines }),
            );
        }
        Err(e) => return machine::error("LINKS_STORE_UNREADABLE", &e.to_string(), json!({})),
    };

    // The target must name a real managed account or the standard account — a
    // stale link to a vanished account is refused, not written.
    if account != DEFAULT_LABEL && !config.account_exists(account) {
        return machine::error(
            "ACCOUNT_NOT_FOUND",
            "account is not a managed account in this installation",
            json!({ "account": account }),
        );
    }

    // Strict, unlike the human path: an unreadable config must fail loudly here
    // rather than silently become "no default" and skew the route and token.
    let default = match config.read_default_strict() {
        Ok(default) => default,
        Err(e) => return machine::error("CONFIG_STORE_UNREADABLE", &e.to_string(), json!({})),
    };
    let accounts = match config.list_accounts() {
        Ok(accounts) => accounts,
        Err(e) => return machine::error("CONFIG_STORE_UNREADABLE", &e.to_string(), json!({})),
    };

    // Authoritative current route. Ambiguity is refused — never healed.
    let before = match resolve::effective_route(config, &query_path) {
        Ok(route) => route,
        Err(crate::config::LinkResolveError::Ambiguous {
            directory,
            mappings,
        }) => {
            let mappings: Vec<serde_json::Value> = mappings
                .iter()
                .map(|(p, a)| json!({ "stored_path": p, "account": a }))
                .collect();
            return machine::error(
                "AMBIGUOUS_LINK",
                "routing is ambiguous for this directory; resolve it before adopting",
                json!({ "query_path": directory, "mappings": mappings }),
            );
        }
        Err(crate::config::LinkResolveError::Io(e)) => {
            return machine::error("LINKS_STORE_UNREADABLE", &e.to_string(), json!({}));
        }
    };

    // `exact_entry` is `dir`'s first equivalent stored entry — the one whose
    // stored spelling a no-op reports — and `affected` is the COMPLETE
    // equivalence class in stored order, exactly the set the write below drops
    // and replaces. Binding the whole set into the token (not just the first
    // entry) is what lets it refuse when a live-FS change alters *which* entries
    // the commit would modify.
    let exact_entry = AppConfig::exact_equivalent_entry(&links, dir);
    let affected = AppConfig::affected_equivalent_entries(&links, dir);
    // dir's own resolved filesystem identity, folded in so a retarget of dir
    // itself flips the token (see `state_token`); `None` where the platform's
    // path model establishes no separate local identity.
    let dir_identity = crate::path_identity::identity_fingerprint(dir);
    let already_exact = exact_entry.as_ref().map(|(_, a)| a.as_str()) == Some(account);
    let current_token = state_token(
        dir,
        account,
        &links,
        default.as_deref(),
        &accounts,
        &before,
        &affected,
        dir_identity.as_deref(),
    );

    // The path reported as the stored link: for a no-op it is the entry's own
    // spelling as actually stored (which may be a case/slash variant of the
    // query path on Windows); for a change it is the query path being written.
    let stored_link_path: &str = if already_exact {
        exact_entry
            .as_ref()
            .map(|(stored, _)| stored.as_str())
            .unwrap_or(dir)
    } else {
        dir
    };

    // The route an exact `dir=account` link would produce. The account is
    // already proven to exist (or be the standard account), so this is the
    // account itself, linked, owned by this directory.
    let planned_after = EffectiveRoute {
        resolved_account: account.to_string(),
        source: RouteSource::Linked,
        owning_link_path: Some(dir.to_string()),
    };

    if dry_run {
        return machine::success(json!({
            "dry_run": true,
            "would_change": !already_exact,
            "query_path": dir,
            "account": account,
            "before": route_json(&before),
            "after": route_json(if already_exact { &before } else { &planned_after }),
            "stored_link_path": stored_link_path,
            "expected_state": current_token,
        }));
    }

    // Commit: the state the owner confirmed must still hold.
    let token = expect_state.expect("commit mode requires --expect-state");
    if token != current_token {
        return machine::error(
            "STATE_CHANGED",
            "routing state changed since the preview; nothing was changed — re-check and decide again",
            json!({
                "query_path": dir,
                "account": account,
                "before": route_json(&before),
            }),
        );
    }

    if already_exact {
        return machine::success(json!({
            "dry_run": false,
            "changed": false,
            "query_path": dir,
            "account": account,
            "before": route_json(&before),
            "after": route_json(&before),
            "stored_link_path": stored_link_path,
        }));
    }

    // Conservative pre-replacement re-read. Cooperating writers are locked out,
    // but a NON-cooperating write (an older binary, a hand-edit, the shell
    // integration — none of which take this lock) could have landed since the
    // read above. If the links store changed under us, refuse rather than
    // clobber it. This NARROWS, and does not close, the window: a non-cooperating
    // write between here and the rename is still possible, which the post-write
    // verification below is the final guard against.
    match config.read_links_strict() {
        Ok(StrictLinksRead::Ok(current)) if current == links => {}
        _ => {
            return machine::error(
                "STATE_CHANGED",
                "the routing store changed during the operation; nothing was changed — re-check and decide again",
                json!({ "query_path": dir, "account": account }),
            );
        }
    }

    // Write only this directory's exact link: drop its equivalent entries, add
    // the one. Ancestor, descendant and unrelated mappings are untouched.
    let mut new_links = links.clone();
    new_links.retain(|(stored, _)| !path_identity::equivalent(stored, dir));
    new_links.push((dir.to_string(), account.to_string()));
    if let Err(e) = config.write_links(&new_links) {
        return machine::error(
            "LINKS_STORE_WRITE_FAILED",
            &format!("the links store could not be written: {e}"),
            json!({ "query_path": dir }),
        );
    }

    // Authoritative post-condition check against the store as it now is. On any
    // disagreement the outcome is uncertain — never reported as unchanged, and
    // never auto-rolled-back; the caller must reconcile read-only.
    match resolve::effective_route(config, &query_path) {
        Ok(after)
            if after.resolved_account == account
                && after.source == RouteSource::Linked
                && after.owning_link_path.as_deref() == Some(dir) =>
        {
            machine::success(json!({
                "dry_run": false,
                "changed": true,
                "query_path": dir,
                "account": account,
                "before": route_json(&before),
                "after": route_json(&after),
                "stored_link_path": stored_link_path,
            }))
        }
        Ok(after) => machine::error(
            "POSTCONDITION_FAILED",
            "the link was written but routing does not resolve as expected; re-check routing before retrying",
            json!({ "query_path": dir, "account": account, "after": route_json(&after) }),
        ),
        Err(_) => machine::error(
            "POSTCONDITION_FAILED",
            "the link was written but routing could not be re-verified; re-check routing before retrying",
            json!({ "query_path": dir, "account": account }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // A synthetic ~/.claude-switch plus a real project directory, so run_json's
    // resolve_query_path (which demands an existing directory) works against a
    // store that is never the machine's real one.
    fn scratch(tag: &str) -> (PathBuf, AppConfig) {
        let root = std::env::temp_dir().join(format!("cc-linkjson-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let config = AppConfig {
            base_dir: root.join("manager"),
        };
        config.init().unwrap();
        (root, config)
    }

    fn account(config: &AppConfig, name: &str) {
        fs::create_dir_all(config.account_path(name)).unwrap();
    }

    fn project(root: &std::path::Path, name: &str) -> String {
        let p = root.join(name);
        fs::create_dir_all(&p).unwrap();
        p.to_str().unwrap().to_string()
    }

    // The token for the store exactly as run_json would compute it right now,
    // so a test can drive the commit path without capturing preview stdout.
    fn token_now(config: &AppConfig, dir: &str, account: &str) -> String {
        let links = config.all_links().unwrap();
        let default = config.get_default().unwrap();
        let accounts = config.list_accounts().unwrap();
        let before = resolve::effective_route(config, std::path::Path::new(dir)).unwrap();
        let affected = AppConfig::affected_equivalent_entries(&links, dir);
        let dir_identity = crate::path_identity::identity_fingerprint(dir);
        state_token(
            dir,
            account,
            &links,
            default.as_deref(),
            &accounts,
            &before,
            &affected,
            dir_identity.as_deref(),
        )
    }

    fn standard_route() -> EffectiveRoute {
        EffectiveRoute {
            resolved_account: "default".to_string(),
            source: RouteSource::Standard,
            owning_link_path: None,
        }
    }

    // --- pure helpers ---

    #[test]
    fn state_token_is_stable_for_identical_inputs_and_moves_on_any_change() {
        let before = standard_route();
        let links = vec![("/p".to_string(), "work".to_string())];
        let accts = vec!["work".to_string()];
        let affected = vec![("/p".to_string(), "work".to_string())];
        let base = state_token(
            "/p",
            "work",
            &links,
            Some("work"),
            &accts,
            &before,
            &affected,
            Some("id:/p"),
        );
        assert_eq!(
            base,
            state_token(
                "/p",
                "work",
                &links,
                Some("work"),
                &accts,
                &before,
                &affected,
                Some("id:/p")
            ),
            "same inputs must hash the same"
        );
        // Every routing-relevant input flips the token: directory, account, the
        // stored links, the configured default, and the account inventory.
        assert_ne!(
            base,
            state_token(
                "/q",
                "work",
                &links,
                Some("work"),
                &accts,
                &before,
                &affected,
                Some("id:/p")
            )
        );
        assert_ne!(
            base,
            state_token(
                "/p",
                "home",
                &links,
                Some("work"),
                &accts,
                &before,
                &affected,
                Some("id:/p")
            )
        );
        assert_ne!(
            base,
            state_token(
                "/p",
                "work",
                &[("/p".to_string(), "home".to_string())],
                Some("work"),
                &accts,
                &before,
                &affected,
                Some("id:/p")
            )
        );
        assert_ne!(
            base,
            state_token(
                "/p",
                "work",
                &links,
                Some("home"),
                &accts,
                &before,
                &affected,
                Some("id:/p")
            )
        );
        assert_ne!(
            base,
            state_token(
                "/p",
                "work",
                &links,
                None,
                &accts,
                &before,
                &affected,
                Some("id:/p")
            )
        );
        assert_ne!(
            base,
            state_token(
                "/p",
                "work",
                &links,
                Some("work"),
                &["work".to_string(), "home".to_string()],
                &before,
                &affected,
                Some("id:/p")
            )
        );
        // And the bound routing INTERPRETATION flips it too: a different resolved
        // before-route, a different affected equivalence set, or a different
        // resolved directory identity.
        let other_route = EffectiveRoute {
            resolved_account: "work".to_string(),
            source: RouteSource::Linked,
            owning_link_path: Some("/p".to_string()),
        };
        assert_ne!(
            base,
            state_token(
                "/p",
                "work",
                &links,
                Some("work"),
                &accts,
                &other_route,
                &affected,
                Some("id:/p")
            )
        );
        // An empty affected set, and a larger affected set whose FIRST member is
        // identical, both flip the token — the whole set is bound, not the head.
        assert_ne!(
            base,
            state_token(
                "/p",
                "work",
                &links,
                Some("work"),
                &accts,
                &before,
                &[],
                Some("id:/p")
            )
        );
        assert_ne!(
            base,
            state_token(
                "/p",
                "work",
                &links,
                Some("work"),
                &accts,
                &before,
                &[
                    ("/p".to_string(), "work".to_string()),
                    ("/P".to_string(), "work".to_string()),
                ],
                Some("id:/p")
            ),
            "a changed affected set with the same first entry must flip the token"
        );
        // And the bound directory identity flips it: present vs absent, and two
        // different resolved identities.
        assert_ne!(
            base,
            state_token(
                "/p",
                "work",
                &links,
                Some("work"),
                &accts,
                &before,
                &affected,
                None
            )
        );
        assert_ne!(
            base,
            state_token(
                "/p",
                "work",
                &links,
                Some("work"),
                &accts,
                &before,
                &affected,
                Some("id:/other")
            )
        );
    }

    #[test]
    fn state_token_separators_prevent_field_run_together_collisions() {
        // "a"+"b" must not hash the same as "ab"+"" for any field pair.
        let r = standard_route();
        let a = state_token("a", "b", &[], None, &[], &r, &[], None);
        let b = state_token("ab", "", &[], None, &[], &r, &[], None);
        assert_ne!(a, b);
    }

    #[test]
    fn exact_equivalent_entry_finds_the_dirs_own_link_only() {
        assert_eq!(AppConfig::exact_equivalent_entry(&[], "/p"), None);
        assert_eq!(
            AppConfig::exact_equivalent_entry(&[("/p".to_string(), "work".to_string())], "/p"),
            Some(("/p".to_string(), "work".to_string()))
        );
        // A different directory's link is not this directory's exact link.
        assert_eq!(
            AppConfig::exact_equivalent_entry(&[("/other".to_string(), "work".to_string())], "/p"),
            None
        );
    }

    #[test]
    fn representable_rejects_newlines_and_trimmed_whitespace_but_accepts_equals_in_dir() {
        assert!(representable_in_store("/Users/a/work", "work"));
        assert!(representable_in_store(r"C:\repo=a=b", "work")); // rightmost '=' delimits
        assert!(!representable_in_store("/has\nnewline", "work"));
        assert!(!representable_in_store("/has\rreturn", "work"));
        assert!(!representable_in_store("/trailing ", "work")); // parser trims -> round-trip fails
        assert!(!representable_in_store(" /leading", "work"));
    }

    // --- run_json behaviour (exit code + authoritative store state) ---

    #[test]
    fn invalid_arguments_for_both_or_neither_mode() {
        let (root, config) = scratch("invalid-args");
        let p = project(&root, "proj");
        // neither
        assert_eq!(run_json(&config, Some(&p), Some("default"), false, None), 1);
        // both
        assert_eq!(
            run_json(&config, Some(&p), Some("default"), true, Some("x")),
            1
        );
        // missing path/account
        assert_eq!(run_json(&config, None, Some("default"), true, None), 1);
        assert_eq!(run_json(&config, Some(&p), None, true, None), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dry_run_never_writes_and_commit_with_the_matching_token_creates_the_exact_link() {
        let (root, config) = scratch("commit");
        account(&config, "work");
        let p = project(&root, "proj");

        // dry-run: exit 0, store still empty
        assert_eq!(run_json(&config, Some(&p), Some("work"), true, None), 0);
        assert!(
            config.all_links().unwrap().is_empty(),
            "preview must not write"
        );

        // commit with the matching token: exit 0, exact link present
        let token = token_now(&config, &p, "work");
        assert_eq!(
            run_json(&config, Some(&p), Some("work"), false, Some(&token)),
            0
        );
        assert_eq!(
            config.all_links().unwrap(),
            vec![(p.clone(), "work".to_string())]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn commit_with_a_stale_token_refuses_and_writes_nothing() {
        let (root, config) = scratch("stale-token");
        account(&config, "work");
        let p = project(&root, "proj");
        assert_eq!(
            run_json(&config, Some(&p), Some("work"), false, Some("deadbeef")),
            1
        );
        assert!(config.all_links().unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_already_exact_link_is_a_no_op_that_does_not_rewrite_the_store() {
        let (root, config) = scratch("noop");
        account(&config, "work");
        let p = project(&root, "proj");
        config.set_link(&p, "work").unwrap();
        let before_bytes = fs::read(config.links_path()).unwrap();

        let token = token_now(&config, &p, "work");
        assert_eq!(
            run_json(&config, Some(&p), Some("work"), false, Some(&token)),
            0
        );
        assert_eq!(
            fs::read(config.links_path()).unwrap(),
            before_bytes,
            "a no-op must not rewrite the store"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn replacing_a_different_exact_account_preserves_every_unrelated_mapping() {
        let (root, config) = scratch("replace");
        account(&config, "work");
        account(&config, "home");
        let p = project(&root, "proj");
        let other = project(&root, "other");
        config.set_link(&p, "home").unwrap();
        config.set_link(&other, "work").unwrap();

        let token = token_now(&config, &p, "work");
        assert_eq!(
            run_json(&config, Some(&p), Some("work"), false, Some(&token)),
            0
        );
        let mut links = config.all_links().unwrap();
        links.sort();
        let mut expected = vec![(p, "work".to_string()), (other, "work".to_string())];
        expected.sort();
        assert_eq!(links, expected, "the unrelated link must survive unchanged");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_inherited_route_to_the_account_still_writes_an_exact_pinning_link() {
        // docs/machine-api.md: inherited == requested is NOT a no-op; an exact
        // link is written, and the ancestor link is left intact.
        let (root, config) = scratch("inherit");
        account(&config, "work");
        let parent = root.join("parent");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();
        let parent_s = parent.to_str().unwrap().to_string();
        let child_s = child.to_str().unwrap().to_string();
        config.set_link(&parent_s, "work").unwrap();

        let token = token_now(&config, &child_s, "work");
        assert_eq!(
            run_json(&config, Some(&child_s), Some("work"), false, Some(&token)),
            0
        );
        let mut links = config.all_links().unwrap();
        links.sort();
        let mut expected = vec![
            (parent_s, "work".to_string()),
            (child_s, "work".to_string()),
        ];
        expected.sort();
        assert_eq!(links, expected);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn linking_to_the_standard_default_account_is_allowed() {
        let (root, config) = scratch("default");
        let p = project(&root, "proj");
        let token = token_now(&config, &p, "default");
        assert_eq!(
            run_json(&config, Some(&p), Some("default"), false, Some(&token)),
            0
        );
        assert_eq!(
            config.all_links().unwrap(),
            vec![(p, "default".to_string())]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_account_and_invalid_name_are_refused_without_writing() {
        let (root, config) = scratch("bad-account");
        let p = project(&root, "proj");
        assert_eq!(run_json(&config, Some(&p), Some("ghost"), true, None), 1); // ACCOUNT_NOT_FOUND
        assert_eq!(run_json(&config, Some(&p), Some("a/b"), true, None), 1); // ACCOUNT_NAME_INVALID
        assert!(config.all_links().unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_or_non_directory_path_is_refused() {
        let (root, config) = scratch("bad-path");
        account(&config, "work");
        let missing = root.join("nope");
        assert_eq!(
            run_json(
                &config,
                Some(missing.to_str().unwrap()),
                Some("work"),
                true,
                None
            ),
            1
        );
        let file = root.join("afile");
        fs::write(&file, "x").unwrap();
        assert_eq!(
            run_json(
                &config,
                Some(file.to_str().unwrap()),
                Some("work"),
                true,
                None
            ),
            1
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_ambiguous_store_is_refused_not_healed() {
        let (root, config) = scratch("ambiguous");
        account(&config, "work");
        let p = project(&root, "Work");
        // Two identically-spelled stored links naming different accounts — the
        // simplest ambiguity that holds on every platform.
        fs::write(
            config.links_path(),
            format!("{p}=personal1\n{p}=personal2\n"),
        )
        .unwrap();
        let before = fs::read(config.links_path()).unwrap();
        assert_eq!(run_json(&config, Some(&p), Some("work"), true, None), 1);
        assert_eq!(
            fs::read(config.links_path()).unwrap(),
            before,
            "an ambiguous store must never be rewritten by the machine path"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_unreadable_config_is_refused_by_the_machine_path_not_treated_as_no_default() {
        let (root, config) = scratch("bad-config");
        account(&config, "work");
        let p = project(&root, "proj");
        // Invalid UTF-8 in the config file: read_to_string fails, and the
        // machine path must report CONFIG_STORE_UNREADABLE rather than quietly
        // proceeding as if no default were set.
        fs::write(config.config_path(), [0xff_u8, 0xff]).unwrap();
        assert_eq!(run_json(&config, Some(&p), Some("work"), true, None), 1);
        assert!(config.all_links().unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_malformed_store_is_refused_not_silently_dropped() {
        let (root, config) = scratch("malformed");
        account(&config, "work");
        let p = project(&root, "proj");
        fs::write(config.links_path(), "this-line-has-no-delimiter\n").unwrap();
        let before = fs::read(config.links_path()).unwrap();
        assert_eq!(run_json(&config, Some(&p), Some("work"), true, None), 1);
        assert_eq!(fs::read(config.links_path()).unwrap(), before);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dry_run_on_an_uninitialized_store_refuses_without_creating_it() {
        // F2: the machine path never initializes. A dry-run against a store that
        // was never set up refuses with NOT_INITIALIZED and manufactures nothing.
        let root = std::env::temp_dir().join(format!("cc-linkjson-uninit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let config = AppConfig {
            base_dir: root.join("manager"),
        }; // deliberately NOT init()'d
        let p = project(&root, "proj");
        assert_eq!(run_json(&config, Some(&p), Some("default"), true, None), 1);
        assert!(
            !config.links_path().exists(),
            "the machine path must not create the store"
        );
        assert!(
            !config.routing_lock_path().exists(),
            "and must not even create the lock file for an uninitialized store"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_changed_default_between_preview_and_commit_is_a_state_change() {
        // F3: the token binds the resolved before-route, not just the stored
        // links. Changing the default moves a standard directory's route, so a
        // commit with the pre-change token is refused even though no link text
        // changed.
        let (root, config) = scratch("f3-default");
        account(&config, "work");
        account(&config, "home");
        let p = project(&root, "proj"); // no link, no default -> standard route
        let token = token_now(&config, &p, "work");
        config.set_default("home").unwrap(); // alters p's before-route
        assert_eq!(
            run_json(&config, Some(&p), Some("work"), false, Some(&token)),
            1
        );
        assert!(config.all_links().unwrap().is_empty(), "nothing written");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_non_cooperating_write_during_the_transaction_is_refused_not_clobbered() {
        // F5: the conservative pre-replacement re-read. Simulate a write that
        // landed after the initial read by changing the store, then committing
        // with a token computed before that change: STATE_CHANGED, the other
        // write survives.
        let (root, config) = scratch("f5-recheck");
        account(&config, "work");
        account(&config, "home");
        let p = project(&root, "proj");
        let other = project(&root, "other");
        let token = token_now(&config, &p, "work");
        // A concurrent (here, interleaved) non-cooperating write to an unrelated
        // directory changes the whole-store fingerprint.
        config.set_link(&other, "home").unwrap();
        assert_eq!(
            run_json(&config, Some(&p), Some("work"), false, Some(&token)),
            1
        );
        assert_eq!(
            config.all_links().unwrap(),
            vec![(other, "home".to_string())],
            "the other write must survive; ours must not have been applied"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_equivalent_spelling_is_treated_as_the_same_exact_link() {
        let (root, config) = scratch("win-equiv");
        account(&config, "work");
        let p = root.join("Proj");
        fs::create_dir_all(&p).unwrap();
        let canonical = p.to_str().unwrap().to_string();
        config.set_link(&canonical, "work").unwrap();
        // Commit for a case/slash-variant spelling of the same directory: it is
        // the same exact link, so it is a no-op and heals to one entry.
        let variant = canonical.to_lowercase().replace('\\', "/");
        let token = token_now(&config, &variant, "work");
        assert_eq!(
            run_json(&config, Some(&variant), Some("work"), false, Some(&token)),
            0
        );
        assert_eq!(config.all_links().unwrap().len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    fn make_junction(link: &std::path::Path, target: &std::path::Path) {
        let status = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &target.to_string_lossy(),
            ])
            .status()
            .expect("spawn mklink");
        assert!(status.success(), "mklink /J must create the junction");
    }

    #[cfg(windows)]
    #[test]
    fn a_changed_affected_set_with_an_unchanged_first_entry_and_route_is_a_state_change() {
        // The precise gap the v3 token closes: `exact_equivalent_entry` returns
        // only the FIRST equivalent entry, while the write drops EVERY equivalent
        // one. A junction retarget can pull a second stored entry into `<DIR>`'s
        // equivalence class with NO stored string, resolved route or first entry
        // changing — so a token bound to only the first entry would still match
        // and the commit would silently modify an entry the owner never previewed.
        // Binding the complete affected set refuses it instead.
        let (root, config) = scratch("affected-set-grows");
        account(&config, "work");
        let proj = root.join("proj");
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(&proj).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        let proj_s = proj.to_str().unwrap().to_string();
        let alias = root.join("alias");
        // alias -> elsewhere: NOT equivalent to proj yet.
        make_junction(&alias, &elsewhere);
        let alias_s = alias.to_str().unwrap().to_string();

        // Two stored entries, both naming `work`, so the class is never
        // ambiguous. Only `proj` is equivalent to proj right now.
        fs::write(
            config.links_path(),
            format!("{proj_s}=work\n{alias_s}=work\n"),
        )
        .unwrap();

        // Baseline interpretation the owner previews against.
        let links = config.all_links().unwrap();
        let first_before = AppConfig::exact_equivalent_entry(&links, &proj_s);
        let route_before = resolve::effective_route(&config, &proj).unwrap();
        let affected_before = AppConfig::affected_equivalent_entries(&links, &proj_s);
        assert_eq!(affected_before.len(), 1, "only proj is equivalent yet");
        let token = token_now(&config, &proj_s, "work");

        // Retarget the junction so alias now resolves to proj. No stored string
        // changes; the links file is byte-for-byte identical.
        fs::remove_dir(&alias).unwrap();
        make_junction(&alias, &proj);

        // The first equivalent entry, the resolved route, and proj's own
        // identity are all UNCHANGED — the only thing that moved is the full set.
        let links_after = config.all_links().unwrap();
        assert_eq!(
            AppConfig::exact_equivalent_entry(&links_after, &proj_s),
            first_before,
            "the first equivalent entry must be unchanged"
        );
        assert_eq!(
            resolve::effective_route(&config, &proj).unwrap(),
            route_before,
            "the resolved before-route must be unchanged"
        );
        assert_eq!(
            AppConfig::affected_equivalent_entries(&links_after, &proj_s).len(),
            2,
            "but the complete affected set has grown to include the alias"
        );

        // Committing with the pre-retarget token is refused: STATE_CHANGED.
        let before_bytes = fs::read(config.links_path()).unwrap();
        assert_eq!(
            run_json(&config, Some(&proj_s), Some("work"), false, Some(&token)),
            1,
            "a grown affected set must refuse the stale token"
        );
        assert_eq!(
            fs::read(config.links_path()).unwrap(),
            before_bytes,
            "a refused commit must not rewrite the store"
        );
        let _ = fs::remove_dir(&alias);
        let _ = fs::remove_dir_all(root);
    }
}
