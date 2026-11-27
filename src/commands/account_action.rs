//! CM9 machine-only account creation / reauthentication action contract.
//!
//! This is the provider half of the GUI account-management surface
//! (ClaudeManagerPS CM9). It exists so a GUI can *trigger* a managed-account
//! create or reauth without the operator hand-typing `claude-acc add` /
//! `claude-acc login`, while claude-acc stays the sole owner of identity,
//! credentials and the authentication interaction.
//!
//! ## Why a new verb instead of `--json` on `add`/`login`
//!
//! The supported Claude Code login (`claude auth login`) is strictly
//! interactive: its only options are `--claudeai/--console/--email/--sso`;
//! there is no `--json`, no headless/device-code mode, and no secret-safe
//! redirected code entry. So a GUI cannot observe or feed the OAuth code, and
//! structured per-keystroke progress is simply not available upstream. The
//! honest design, which this implements, is:
//!
//! * **the caller hosts the terminal** — the consumer launches
//!   `claude-acc account-action` in a real terminal it owns, and the
//!   interactive `claude auth login` inherits that terminal (exactly as the
//!   human `add`/`login` do). The OAuth code is entered there, in claude-acc's
//!   own authentication surface. ClaudeManagerPS never sees it.
//! * **the machine channel is a file** — because the terminal is busy hosting
//!   the login, the single structured outcome document goes to the
//!   caller-supplied `--result-file`, never mixed into the login's stdout. It
//!   carries only non-secret fields (the account name, the coarse outcome, the
//!   reconciled public identity email/uuid) — never a code, token, credential,
//!   cookie or authorization header.
//! * **discovery without authenticating** — `--probe` reports the contract
//!   version and exactly which capabilities exist (and which do not, e.g.
//!   structured code-entry feedback), performing no authentication.
//!
//! The outcome is coarse by necessity: `authenticated`, `not_authenticated`
//! (the login exited non-zero — upstream does not let us tell a user
//! cancellation apart from a failure), `uncertain` (exited success yet no
//! local identity landed), or `spawn_failed`. This limitation is documented in
//! the probe output and docs/machine-api.md rather than papered over.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::claude_process::{ClaudeProcess, ClaudeProfile, current_dir, manager_bin_dir};
use crate::config::{AppConfig, is_reserved_name, validate_name};
use crate::identity::{self, Identity};
use crate::machine;
use crate::seed;

/// The account-action contract version. Independent of the shared envelope's
/// `schema_version`: this is the number a consumer pins to decide whether the
/// provider speaks the create/reauth action contract it needs. Starts at 1;
/// bumped only when an existing field's meaning changes or a field is removed,
/// never for additive fields (same policy as the `--json` schemas).
pub const CONTRACT_VERSION: u64 = 1;

/// Which lifecycle action the caller asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Create,
    Reauth,
}

impl Action {
    pub fn parse(s: &str) -> Option<Action> {
        match s {
            "create" => Some(Action::Create),
            "reauth" => Some(Action::Reauth),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Action::Create => "create",
            Action::Reauth => "reauth",
        }
    }
}

/// The exit signal of the interactive login child, abstracted so the whole
/// action can be driven by a controlled fake in tests without ever running a
/// real OAuth flow. `Exited` carries the child's real numeric code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginExit {
    Exited(i32),
    SpawnFailed,
}

/// The coarse, non-secret conclusion of an attempted authentication — the most
/// the supported upstream interface lets us report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Login exited success and a local identity is now present.
    Authenticated,
    /// Login exited non-zero. Upstream does not distinguish a user
    /// cancellation from an authentication failure, so both land here; the raw
    /// child exit code is carried for whatever signal it holds.
    NotAuthenticated,
    /// Login exited success yet no local identity landed — an unexpected state
    /// the consumer should surface rather than treat as signed in.
    Uncertain,
    /// The login child could not be started at all.
    SpawnFailed,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Authenticated => "authenticated",
            Outcome::NotAuthenticated => "not_authenticated",
            Outcome::Uncertain => "uncertain",
            Outcome::SpawnFailed => "spawn_failed",
        }
    }
}

/// A defined, non-secret conclusion of an action that actually ran a login
/// attempt. Serialized into the `ok:true` outcome document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionReport {
    pub action: Action,
    pub account: String,
    pub existed_before: bool,
    pub created: bool,
    pub outcome: Outcome,
    pub child_exit_code: Option<i32>,
    pub identity: Option<Identity>,
    pub duplicate_of: Option<String>,
    pub pinned: bool,
}

/// An operational refusal that prevented a login attempt from running. Shaped
/// into the standard `ok:false` error envelope. None of these is destructive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    ReservedName(String),
    NameInvalid,
    /// Create refused because a managed account already exists — never
    /// silently replaced.
    AccountExists(String),
    /// Reauth refused because the target managed account does not exist.
    AccountNotFound(String),
    /// Another account action for this account is already in flight.
    ActionInProgress(String),
    Io(String),
}

impl ActionError {
    fn code(&self) -> &'static str {
        match self {
            ActionError::ReservedName(_) => "RESERVED_NAME",
            ActionError::NameInvalid => "NAME_INVALID",
            ActionError::AccountExists(_) => "ACCOUNT_EXISTS",
            ActionError::AccountNotFound(_) => "ACCOUNT_NOT_FOUND",
            ActionError::ActionInProgress(_) => "ACTION_IN_PROGRESS",
            ActionError::Io(_) => "IO_ERROR",
        }
    }

    fn message(&self) -> String {
        match self {
            ActionError::ReservedName(n) => {
                format!(
                    "'{n}' is the reserved standard-account name; it cannot be created or reauthenticated through this contract"
                )
            }
            ActionError::NameInvalid => {
                "account name must be ASCII letters, digits, hyphens or underscores".to_string()
            }
            ActionError::AccountExists(n) => {
                format!("a managed account named '{n}' already exists; create never replaces one")
            }
            ActionError::AccountNotFound(n) => {
                format!("no managed account named '{n}' to reauthenticate")
            }
            ActionError::ActionInProgress(n) => {
                format!("another account action for '{n}' is already in progress")
            }
            ActionError::Io(e) => e.clone(),
        }
    }

    fn details(&self) -> Value {
        match self {
            ActionError::ReservedName(n)
            | ActionError::AccountExists(n)
            | ActionError::AccountNotFound(n)
            | ActionError::ActionInProgress(n) => json!({ "account": n }),
            ActionError::NameInvalid | ActionError::Io(_) => json!({}),
        }
    }
}

/// Classify the coarse outcome from the login child's exit and whether a local
/// identity is present afterward. Pure, so every branch is unit-tested without
/// a filesystem or a child process.
pub fn classify_outcome(exit: LoginExit, identity_present: bool) -> Outcome {
    match exit {
        LoginExit::SpawnFailed => Outcome::SpawnFailed,
        LoginExit::Exited(0) if identity_present => Outcome::Authenticated,
        LoginExit::Exited(0) => Outcome::Uncertain,
        LoginExit::Exited(_) => Outcome::NotAuthenticated,
    }
}

/// The per-account action lock path. Separate from the routing lock (which
/// must never be held across an interactive login) and keyed by account name
/// so two actions on *different* accounts never block each other. The name is
/// already `validate_name`-checked before this is called, so it is safe in a
/// file name.
fn action_lock_path(config: &AppConfig, name: &str) -> PathBuf {
    config.base_dir.join(format!(".account-action.{name}.lock"))
}

/// Run `body` while holding the per-account action lock, mapping "already
/// held" to [`ActionError::ActionInProgress`] instead of blocking. The lock is
/// an OS advisory lock on a dedicated file, released when the guard drops — a
/// crashed holder leaves nothing stale. It is held across the whole action,
/// including the interactive login, so two overlapping create/reauths of the
/// same account cannot interleave.
fn with_action_lock<T>(
    config: &AppConfig,
    name: &str,
    body: impl FnOnce() -> Result<T, ActionError>,
) -> Result<T, ActionError> {
    let path = action_lock_path(config, name);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| ActionError::Io(e.to_string()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            return Err(ActionError::ActionInProgress(name.to_string()));
        }
        Err(fs::TryLockError::Error(e)) => return Err(ActionError::Io(e.to_string())),
    }
    let result = body();
    drop(file); // explicit: release the advisory lock only after the action ends
    result
}

/// The whole action, with the interactive login abstracted behind `run_login`
/// so it is drivable by a controlled fake in tests. Returns a defined report
/// once a login was attempted, or a non-destructive operational error.
///
/// Safety properties enforced here: create never replaces an existing account;
/// nothing touches the configured default or any routing link; the identity
/// pin is only ever *added* when absent (never silently moved); the account
/// directory is never rolled back when authentication fails; and only one
/// action per account runs at a time.
pub fn perform_action(
    config: &AppConfig,
    action: Action,
    name: &str,
    seed_from_default: bool,
    run_login: &dyn Fn(&Path) -> LoginExit,
) -> Result<ActionReport, ActionError> {
    if is_reserved_name(name) {
        return Err(ActionError::ReservedName(name.to_string()));
    }
    if !validate_name(name) {
        return Err(ActionError::NameInvalid);
    }

    with_action_lock(config, name, || {
        let acc_dir = config.account_path(name);
        let (existed_before, created) = match action {
            Action::Create => {
                // Reserve the name under the routing lock — the existence check
                // and directory creation are one inventory write, so a
                // concurrent machine `link` sees a consistent inventory and two
                // creates of the same name cannot both pass. The interactive
                // login below is deliberately NOT under the routing lock.
                let created = config
                    .with_routing_lock(|| {
                        if acc_dir.is_dir() {
                            return Ok(false);
                        }
                        fs::create_dir_all(&acc_dir)?;
                        Ok(true)
                    })
                    .map_err(|e| ActionError::Io(e.to_string()))?;
                if !created {
                    return Err(ActionError::AccountExists(name.to_string()));
                }
                if seed_from_default {
                    // Best-effort, exactly like `add`: an empty account dir is
                    // still usable, and a seed failure must not abort the login.
                    let _ = seed::copy_user_config(&acc_dir);
                }
                (false, true)
            }
            Action::Reauth => {
                if !acc_dir.is_dir() {
                    return Err(ActionError::AccountNotFound(name.to_string()));
                }
                (true, false)
            }
        };

        let exit = run_login(&acc_dir);
        let identity_after = identity::local_identity(&acc_dir);
        let outcome = classify_outcome(exit, identity_after.is_some());
        let child_exit_code = match exit {
            LoginExit::Exited(code) => Some(code),
            LoginExit::SpawnFailed => None,
        };

        let mut duplicate_of = None;
        let mut pinned = false;
        if outcome == Outcome::Authenticated {
            // Pins only if the account has no pin yet (never moves an existing
            // pin — that swap is exactly the drift the pin exists to report).
            super::lock::write_after_login(config, name);
            pinned = super::lock::json_state(config, name).1.is_some();
            let known = super::known_account_cache_paths(config, name);
            duplicate_of = identity::find_duplicate_account(&acc_dir, &known);
        }

        Ok(ActionReport {
            action,
            account: name.to_string(),
            existed_before,
            created,
            outcome,
            child_exit_code,
            identity: identity_after,
            duplicate_of,
            pinned,
        })
    })
}

/// The account-action-specific fields of a `ok:true` outcome document (without
/// the shared `schema_version`/`ok` envelope keys). Pure.
fn report_fields(report: &ActionReport) -> Value {
    let identity = match &report.identity {
        Some(id) => json!({ "uuid": id.uuid, "email": id.email }),
        None => Value::Null,
    };
    json!({
        "contract": "account-action",
        "contract_version": CONTRACT_VERSION,
        "action": report.action.as_str(),
        "account": report.account,
        "account_existed_before": report.existed_before,
        "account_created": report.created,
        "outcome": report.outcome.as_str(),
        "authenticated": report.outcome == Outcome::Authenticated,
        "child_exit_code": report.child_exit_code,
        "identity": identity,
        "duplicate_of": report.duplicate_of,
        "pinned": report.pinned,
    })
}

/// The full `ok:true` outcome document for a report. Pure.
pub fn report_to_doc(report: &ActionReport) -> Value {
    machine::build_success(report_fields(report))
}

/// The full `ok:false` error envelope for an operational refusal. Pure.
pub fn error_to_doc(error: &ActionError) -> Value {
    machine::build_error(error.code(), &error.message(), error.details())
}

/// The account-action-specific fields of the capability/version discovery
/// document. No authentication, no side effect. States plainly which
/// capabilities exist and which do not, so the consumer can gate its UI
/// truthfully.
fn probe_fields() -> Value {
    json!({
        "contract": "account-action",
        "contract_version": CONTRACT_VERSION,
        "provider_version": env!("CARGO_PKG_VERSION"),
        "actions": ["create", "reauth"],
        // The consumer hosts the terminal; claude-acc hosts the login within it.
        "interactive_hosting": "caller_terminal",
        // The single structured outcome goes to --result-file, never stdout.
        "result_channel": "result_file",
        // Honest capability disclosure (upstream `claude auth login` limits).
        "oauth_code_entry_feedback": "unavailable",
        "progress_granularity": "final_outcome_only",
        "distinguishes_cancellation_from_failure": false,
        "secret_safe": true,
    })
}

/// The full capability document (for tests and callers that want the envelope).
pub fn probe_doc() -> Value {
    machine::build_success(probe_fields())
}

/// Atomically write the result document to `path` (temp sibling + rename), so a
/// consumer polling the file never reads a torn document.
fn write_result_file(path: &Path, doc: &Value) -> io::Result<()> {
    let serialized = serde_json::to_string_pretty(doc).map_err(io::Error::other)?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "result path has no file name")
    })?;
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    fs::write(&tmp, serialized.as_bytes())?;
    let renamed = fs::rename(&tmp, path);
    if renamed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    renamed
}

/// The production interactive login: run `claude auth login` scoped to
/// `acc_dir`, bracketed by the standard-account keychain snapshot/restore (a
/// login can clobber the standard account's own Keychain entries as a side
/// effect, even scoped). Reuses the shared `ClaudeProcess` substrate, so exe
/// resolution, structured argv and the auth-env scrub are identical to
/// `add`/`login`. The terminal is inherited — the caller is expected to have
/// hosted one.
fn run_login_production(config: &AppConfig, acc_dir: &Path) -> LoginExit {
    let keychain = identity::snapshot_side_effect_keychain();
    let process = ClaudeProcess::new(
        ["auth", "login"],
        ClaudeProfile::Named(acc_dir.to_path_buf()),
        current_dir(),
        manager_bin_dir(&config.base_dir),
    );
    let result = process.spawn();
    identity::restore_side_effect_keychain(keychain);
    match result {
        Ok(code) => LoginExit::Exited(code),
        Err(_) => LoginExit::SpawnFailed,
    }
}

/// CLI entry point for `account-action`. Machine-only: human use is `add` /
/// `login`. `--probe` writes the capability document to stdout; a create/reauth
/// writes its single outcome document to `--result-file` and keeps stdout clear
/// for the interactive login the caller's terminal is hosting.
#[allow(clippy::too_many_arguments)]
pub fn run_json(
    config: &AppConfig,
    json_flag: bool,
    probe: bool,
    action: Option<&str>,
    account: Option<&str>,
    result_file: Option<&str>,
    seed_from_default: bool,
) -> i32 {
    if !json_flag {
        return machine::error(
            "JSON_REQUIRED",
            "account-action is a machine-only verb; pass --json (use 'add'/'login' for interactive human use)",
            json!({}),
        );
    }
    if probe {
        return machine::success(probe_fields());
    }

    // clap enforces these are present unless --probe; treat an unexpected
    // absence as a structured argument error rather than panicking.
    let (Some(action_str), Some(name), Some(result_path)) = (action, account, result_file) else {
        return machine::error(
            "INVALID_ARGUMENTS",
            "create/reauth require --action, --account and --result-file",
            json!({}),
        );
    };
    let result_path = PathBuf::from(result_path);

    let (doc, exit_code) = match Action::parse(action_str) {
        None => (
            machine::build_error(
                "ACTION_INVALID",
                "--action must be 'create' or 'reauth'",
                json!({ "action": action_str }),
            ),
            1,
        ),
        Some(act) => {
            let run_login = |acc_dir: &Path| run_login_production(config, acc_dir);
            match perform_action(config, act, name, seed_from_default, &run_login) {
                Ok(report) => (report_to_doc(&report), 0),
                Err(error) => (error_to_doc(&error), 1),
            }
        }
    };

    match write_result_file(&result_path, &doc) {
        Ok(()) => exit_code,
        Err(e) => machine::error(
            "RESULT_FILE_WRITE_FAILED",
            &e.to_string(),
            json!({ "result_file": result_path.display().to_string() }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SCRATCH_SEQ: AtomicU32 = AtomicU32::new(0);

    fn scratch(tag: &str) -> AppConfig {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "cc-acctaction-{}-{}-{}",
            tag,
            std::process::id(),
            seq
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("accounts")).unwrap();
        AppConfig { base_dir: base }
    }

    /// A fake login that writes a local identity (as a real successful login
    /// would) and returns exit 0.
    fn fake_login_success(
        uuid: &'static str,
        email: Option<&'static str>,
    ) -> impl Fn(&Path) -> LoginExit {
        move |acc_dir: &Path| {
            let oauth = match email {
                Some(e) => json!({"oauthAccount": {"accountUuid": uuid, "emailAddress": e}}),
                None => json!({"oauthAccount": {"accountUuid": uuid}}),
            };
            fs::write(acc_dir.join(".claude.json"), oauth.to_string()).unwrap();
            LoginExit::Exited(0)
        }
    }

    // --- pure classification ---

    #[test]
    fn classify_covers_every_outcome_branch() {
        assert_eq!(
            classify_outcome(LoginExit::Exited(0), true),
            Outcome::Authenticated
        );
        assert_eq!(
            classify_outcome(LoginExit::Exited(0), false),
            Outcome::Uncertain
        );
        assert_eq!(
            classify_outcome(LoginExit::Exited(1), true),
            Outcome::NotAuthenticated
        );
        assert_eq!(
            classify_outcome(LoginExit::Exited(130), false),
            Outcome::NotAuthenticated
        );
        assert_eq!(
            classify_outcome(LoginExit::SpawnFailed, false),
            Outcome::SpawnFailed
        );
    }

    #[test]
    fn action_parse_accepts_only_the_two_actions() {
        assert_eq!(Action::parse("create"), Some(Action::Create));
        assert_eq!(Action::parse("reauth"), Some(Action::Reauth));
        assert_eq!(Action::parse("delete"), None);
        assert_eq!(Action::parse("Create"), None);
        assert_eq!(Action::parse(""), None);
    }

    // --- create ---

    #[test]
    fn create_success_authenticates_creates_pins_and_reconciles_identity() {
        let c = scratch("create-ok");
        let login = fake_login_success("u-new", Some("new@example.com"));
        let report = perform_action(&c, Action::Create, "fresh", false, &login).unwrap();

        assert_eq!(report.outcome, Outcome::Authenticated);
        assert!(report.created);
        assert!(!report.existed_before);
        assert_eq!(report.child_exit_code, Some(0));
        assert_eq!(report.identity.as_ref().unwrap().uuid, "u-new");
        assert_eq!(
            report.identity.as_ref().unwrap().email.as_deref(),
            Some("new@example.com")
        );
        assert!(report.pinned, "a freshly authenticated account is pinned");
        assert!(c.account_path("fresh").is_dir());
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn create_refuses_an_existing_account_without_replacing_it() {
        let c = scratch("create-exists");
        fs::create_dir_all(c.account_path("taken")).unwrap();
        fs::write(c.account_path("taken").join("marker"), "keep me").unwrap();
        let login = fake_login_success("u-x", None);
        let err = perform_action(&c, Action::Create, "taken", false, &login).unwrap_err();
        assert_eq!(err, ActionError::AccountExists("taken".to_string()));
        // The pre-existing content is untouched — never silently replaced.
        assert_eq!(
            fs::read_to_string(c.account_path("taken").join("marker")).unwrap(),
            "keep me"
        );
        assert!(
            !c.account_path("taken").join(".claude.json").exists(),
            "login must not have run"
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn create_rejects_reserved_and_invalid_names_before_touching_the_store() {
        let c = scratch("create-badname");
        let login = fake_login_success("u", None);
        assert_eq!(
            perform_action(&c, Action::Create, "default", false, &login).unwrap_err(),
            ActionError::ReservedName("default".to_string())
        );
        assert_eq!(
            perform_action(&c, Action::Create, "bad/name", false, &login).unwrap_err(),
            ActionError::NameInvalid
        );
        assert!(
            c.list_accounts().unwrap().is_empty(),
            "no account dir created for a bad name"
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn create_that_fails_authentication_keeps_the_directory_and_does_not_pin() {
        // Defined behavior: account created, auth failed/cancelled -> the dir
        // stays (no destructive rollback), authenticated=false, no pin.
        let c = scratch("create-authfail");
        let login = |_acc: &Path| LoginExit::Exited(1);
        let report = perform_action(&c, Action::Create, "halfborn", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::NotAuthenticated);
        assert!(report.created);
        assert!(!report.pinned);
        assert!(report.identity.is_none());
        assert_eq!(report.child_exit_code, Some(1));
        assert!(
            c.account_path("halfborn").is_dir(),
            "the account dir is not rolled back"
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn create_success_but_no_local_identity_is_uncertain_not_authenticated() {
        let c = scratch("create-uncertain");
        let login = |_acc: &Path| LoginExit::Exited(0); // exits 0 but writes no identity
        let report = perform_action(&c, Action::Create, "ghost", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::Uncertain);
        assert!(!report.pinned);
        assert!(report.identity.is_none());
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn create_spawn_failure_is_reported_as_spawn_failed() {
        let c = scratch("create-spawnfail");
        let login = |_acc: &Path| LoginExit::SpawnFailed;
        let report = perform_action(&c, Action::Create, "nostart", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::SpawnFailed);
        assert_eq!(report.child_exit_code, None);
        assert!(
            report.created,
            "the dir was reserved before the spawn attempt"
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- reauth ---

    #[test]
    fn reauth_requires_an_existing_account() {
        let c = scratch("reauth-missing");
        let login = fake_login_success("u", None);
        assert_eq!(
            perform_action(&c, Action::Reauth, "ghost", false, &login).unwrap_err(),
            ActionError::AccountNotFound("ghost".to_string())
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn reauth_of_an_existing_account_authenticates_without_recreating() {
        let c = scratch("reauth-ok");
        fs::create_dir_all(c.account_path("work")).unwrap();
        let login = fake_login_success("u-work", Some("work@example.com"));
        let report = perform_action(&c, Action::Reauth, "work", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::Authenticated);
        assert!(report.existed_before);
        assert!(!report.created);
        assert_eq!(report.identity.as_ref().unwrap().uuid, "u-work");
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn reauth_does_not_move_an_existing_pin() {
        // No silent re-pin: a pinned account that is reauthenticated as a
        // different identity keeps its original pin (that swap is drift).
        let c = scratch("reauth-pin");
        let acc = c.account_path("pinned");
        fs::create_dir_all(&acc).unwrap();
        identity::write_lock_at(
            &acc.join(identity::LOCK_FILE),
            &Identity {
                uuid: "u-original".to_string(),
                email: None,
            },
        )
        .unwrap();
        let login = fake_login_success("u-different", Some("other@example.com"));
        let report = perform_action(&c, Action::Reauth, "pinned", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::Authenticated);
        // The pin file still names the original identity.
        let raw = fs::read_to_string(acc.join(identity::LOCK_FILE)).unwrap();
        assert!(raw.contains("u-original"), "the pin must not move: {raw}");
        assert!(!raw.contains("u-different"));
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- duplicate identity advisory ---

    #[test]
    fn a_duplicate_subscription_identity_is_advised() {
        let c = scratch("dup");
        // An existing account whose cached identity is u-shared.
        let existing = c.account_path("personal1");
        fs::create_dir_all(&existing).unwrap();
        fs::write(
            existing.join(".account-info.json"),
            json!({"uuid": "u-shared", "email": "shared@example.com"}).to_string(),
        )
        .unwrap();
        // Create a new account that authenticates as the SAME identity.
        let login = fake_login_success("u-shared", Some("shared@example.com"));
        let report = perform_action(&c, Action::Create, "personal2", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::Authenticated);
        assert_eq!(report.duplicate_of.as_deref(), Some("personal1"));
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- concurrency / action lock ---

    #[test]
    fn a_second_action_for_the_same_account_is_refused_while_one_is_in_flight() {
        let c = scratch("concurrent");
        let acc = "busy";
        // A login that, while "running", asserts a second action is refused.
        let inner = std::sync::Arc::new(AppConfig {
            base_dir: c.base_dir.clone(),
        });
        let login = {
            let inner = inner.clone();
            move |acc_dir: &Path| {
                let nested = perform_action(&inner, Action::Reauth, "busy", false, &|_: &Path| {
                    LoginExit::Exited(0)
                });
                assert_eq!(
                    nested.unwrap_err(),
                    ActionError::ActionInProgress("busy".to_string()),
                    "a second action for the same account must be refused while one holds the lock"
                );
                // Now simulate a real successful login for the outer action.
                fs::write(
                    acc_dir.join(".claude.json"),
                    json!({"oauthAccount": {"accountUuid": "u-busy"}}).to_string(),
                )
                .unwrap();
                LoginExit::Exited(0)
            }
        };
        let report = perform_action(&c, Action::Create, acc, false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::Authenticated);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn an_action_for_a_different_account_is_not_blocked() {
        let c = scratch("concurrent-distinct");
        let inner = AppConfig {
            base_dir: c.base_dir.clone(),
        };
        let login = move |acc_dir: &Path| {
            // A different account must acquire its own lock freely.
            let other = perform_action(&inner, Action::Reauth, "other", false, &|d: &Path| {
                fs::write(
                    d.join(".claude.json"),
                    json!({"oauthAccount": {"accountUuid": "u-other"}}).to_string(),
                )
                .unwrap();
                LoginExit::Exited(0)
            });
            assert!(
                other.is_ok(),
                "a different account must not be blocked: {other:?}"
            );
            fs::write(
                acc_dir.join(".claude.json"),
                json!({"oauthAccount": {"accountUuid": "u-main"}}).to_string(),
            )
            .unwrap();
            LoginExit::Exited(0)
        };
        fs::create_dir_all(c.account_path("other")).unwrap();
        let report = perform_action(&c, Action::Create, "main", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::Authenticated);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- preservation of unrelated state ---

    #[test]
    fn an_action_never_touches_the_default_or_links_or_other_accounts() {
        let c = scratch("preserve");
        // Seed unrelated routing state and a sibling account.
        c.init().unwrap();
        fs::create_dir_all(c.account_path("sibling")).unwrap();
        c.set_default("sibling").unwrap();
        fs::write(c.links_path(), "/some/dir=sibling\n").unwrap();
        let before_links = fs::read_to_string(c.links_path()).unwrap();

        let login = fake_login_success("u-new", Some("new@example.com"));
        let report = perform_action(&c, Action::Create, "newcomer", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::Authenticated);

        assert_eq!(
            c.get_default().unwrap().as_deref(),
            Some("sibling"),
            "default unchanged"
        );
        assert_eq!(
            fs::read_to_string(c.links_path()).unwrap(),
            before_links,
            "links unchanged"
        );
        assert!(c.account_path("sibling").is_dir(), "sibling preserved");
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- document shapes / secret safety ---

    #[test]
    fn the_outcome_document_has_the_envelope_shape_and_no_secret_fields() {
        let report = ActionReport {
            action: Action::Create,
            account: "x".to_string(),
            existed_before: false,
            created: true,
            outcome: Outcome::Authenticated,
            child_exit_code: Some(0),
            identity: Some(Identity {
                uuid: "u".to_string(),
                email: Some("e@x.com".to_string()),
            }),
            duplicate_of: None,
            pinned: true,
        };
        let doc = report_to_doc(&report);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["ok"], true);
        assert_eq!(doc["contract"], "account-action");
        assert_eq!(doc["contract_version"], 1);
        assert_eq!(doc["outcome"], "authenticated");
        assert_eq!(doc["authenticated"], true);
        assert_eq!(doc["identity"]["uuid"], "u");
        assert_eq!(doc["identity"]["email"], "e@x.com");
        // Exactly the fields a consumer needs — no token/code/credential key.
        let obj = doc.as_object().unwrap();
        for forbidden in [
            "token",
            "access_token",
            "refresh_token",
            "code",
            "credential",
            "credentials",
            "authorization",
            "password",
            "cookie",
        ] {
            assert!(
                !obj.contains_key(forbidden),
                "no secret field '{forbidden}'"
            );
        }
        let idobj = doc["identity"].as_object().unwrap();
        for forbidden in ["token", "access_token", "refresh_token", "credential"] {
            assert!(
                !idobj.contains_key(forbidden),
                "identity carries no secret '{forbidden}'"
            );
        }
    }

    #[test]
    fn a_synthetic_secret_sentinel_never_reaches_the_outcome_document() {
        // A login writes a credential-looking file; reconciliation reads only
        // the non-secret oauthAccount identity, never the sentinel.
        let c = scratch("sentinel");
        let sentinel = "SENTINEL_TOKEN_sk-DO-NOT-LEAK";
        let login = move |acc_dir: &Path| {
            fs::write(
                acc_dir.join(".credentials.json"),
                json!({"access_token": sentinel}).to_string(),
            )
            .unwrap();
            fs::write(
                acc_dir.join(".claude.json"),
                json!({"oauthAccount": {"accountUuid": "u-s", "emailAddress": "s@x.com"}})
                    .to_string(),
            )
            .unwrap();
            LoginExit::Exited(0)
        };
        let report = perform_action(&c, Action::Create, "sec", false, &login).unwrap();
        let doc = report_to_doc(&report);
        let serialized = serde_json::to_string(&doc).unwrap();
        assert!(
            !serialized.contains(sentinel),
            "the credential sentinel must never appear in the outcome document"
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn error_document_has_the_error_envelope_shape() {
        let doc = error_to_doc(&ActionError::AccountExists("x".to_string()));
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["ok"], false);
        assert_eq!(doc["error"]["code"], "ACCOUNT_EXISTS");
        assert_eq!(doc["error"]["details"]["account"], "x");
    }

    #[test]
    fn the_probe_document_discloses_capabilities_truthfully() {
        let doc = probe_doc();
        assert_eq!(doc["ok"], true);
        assert_eq!(doc["contract"], "account-action");
        assert_eq!(doc["contract_version"], 1);
        assert_eq!(doc["actions"], json!(["create", "reauth"]));
        assert_eq!(doc["oauth_code_entry_feedback"], "unavailable");
        assert_eq!(doc["distinguishes_cancellation_from_failure"], false);
        assert_eq!(doc["secret_safe"], true);
        assert_eq!(doc["provider_version"], env!("CARGO_PKG_VERSION"));
    }

    // --- result-file transport ---

    #[test]
    fn write_result_file_writes_atomically_and_leaves_no_temp() {
        let dir = scratch("resultfile");
        let path = dir.base_dir.join("out.json");
        let doc = report_to_doc(&ActionReport {
            action: Action::Reauth,
            account: "x".to_string(),
            existed_before: true,
            created: false,
            outcome: Outcome::NotAuthenticated,
            child_exit_code: Some(2),
            identity: None,
            duplicate_of: None,
            pinned: false,
        });
        write_result_file(&path, &doc).unwrap();
        let back: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back["outcome"], "not_authenticated");
        let leftovers: Vec<_> = fs::read_dir(&dir.base_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file left behind: {leftovers:?}"
        );
        let _ = fs::remove_dir_all(&dir.base_dir);
    }
}
