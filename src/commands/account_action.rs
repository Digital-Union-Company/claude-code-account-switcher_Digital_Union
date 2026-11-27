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
//! ## Truthful outcomes (F2)
//!
//! A clean login *exit* plus a local `.claude.json` identity is NOT proof the
//! account is authenticated now: that file is Claude Code's own cache and can
//! be stale from an earlier login. So `authenticated` is reported ONLY when an
//! authoritative live audit (token + profile fetch — the same substrate `add`,
//! `login` and `doctor` use) confirms it. Otherwise the login merely
//! *completed* and the consumer MUST reconcile:
//!
//! * `authenticated` — a live audit confirmed a current credential.
//! * `login_completed` — the login exited 0 and a local identity is present,
//!   but it was not authoritatively verified (offline / no token). Reconcile
//!   with `doctor`/`list` before treating the account as signed in.
//! * `login_completed_no_identity` — exited 0 yet no local identity landed.
//! * `login_failed` — the login exited non-zero. Upstream cannot tell a user
//!   cancellation from a failure, and for a *reauth* this does NOT mean the
//!   account lost whatever authentication it already had — reconcile.
//! * `spawn_failed` — the login child could not be started at all.
//!
//! Exactly one authoritative audit is performed per action, reused for both the
//! `authenticated` determination and the duplicate-account disclosure.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::claude_process::{ClaudeProcess, ClaudeProfile, current_dir, manager_bin_dir};
use crate::config::{AppConfig, is_reserved_name, validate_name};
use crate::identity::{self, Identity};
use crate::machine;
use crate::seed;

/// The account-action contract version. Independent of the shared envelope's
/// `schema_version`: this is the number a consumer pins to decide whether the
/// provider speaks the create/reauth action contract it needs. Still `1`: the
/// contract has never been released or merged, so this hardening revised the
/// pre-release shape in place rather than bumping a version nobody consumes.
pub const CONTRACT_VERSION: u64 = 1;

/// Monotonic suffix so two result-file publishes from one process never choose
/// the same temp name. The exclusive `create_new` is the real guard; this only
/// keeps the common path collision-free.
static PUBLISH_SEQ: AtomicU64 = AtomicU64::new(0);

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

/// Whether the account's authentication was *authoritatively* verified after
/// the login (a live token + profile audit), or could not be — the distinction
/// F2 turns on. `Unconfirmed` is not a failure; it is "the login completed but
/// we did not, or could not, prove a live credential".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    Confirmed,
    Unconfirmed,
}

/// The coarse, non-secret conclusion of an attempted authentication — the most
/// the supported upstream interface lets us report. See the module docs for
/// what each means and which require consumer reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Login exited 0 AND a live audit confirmed a current credential.
    Authenticated,
    /// Login exited 0 and a local identity is present, but it was not
    /// authoritatively verified (offline / no token). Reconcile before trusting.
    LoginCompleted,
    /// Login exited 0 yet no local identity landed at all.
    LoginCompletedNoIdentity,
    /// Login exited non-zero. A cancellation and a failure are indistinguishable
    /// upstream; for a reauth the account may still hold its prior credential.
    LoginFailed,
    /// The login child could not be started at all.
    SpawnFailed,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Authenticated => "authenticated",
            Outcome::LoginCompleted => "login_completed",
            Outcome::LoginCompletedNoIdentity => "login_completed_no_identity",
            Outcome::LoginFailed => "login_failed",
            Outcome::SpawnFailed => "spawn_failed",
        }
    }

    /// Whether the consumer must reconcile (run `doctor`/`list`) before trusting
    /// the account's state. True for everything except an authoritatively
    /// confirmed `authenticated` and a `spawn_failed` where nothing ran.
    fn requires_reconciliation(self) -> bool {
        matches!(
            self,
            Outcome::LoginCompleted | Outcome::LoginCompletedNoIdentity | Outcome::LoginFailed
        )
    }
}

/// A defined, non-secret conclusion of an action that actually ran a login
/// attempt. Serialized into the `ok:true` outcome document. (No `Eq`: it
/// carries an `Identity`, which is `PartialEq` but not `Eq`; tests assert on
/// individual fields rather than whole-report equality.)
#[derive(Debug, Clone)]
pub struct ActionReport {
    pub action: Action,
    pub account: String,
    pub existed_before: bool,
    pub created: bool,
    pub outcome: Outcome,
    pub authority: Authority,
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
    /// A syntactically valid name that is unsafe as a directory on some platform
    /// (a Windows reserved device name) — refused on every platform.
    NameUnsafe(String),
    /// Create refused because a managed account already exists — never
    /// silently replaced.
    AccountExists(String),
    /// Reauth refused because the target managed account does not exist.
    AccountNotFound(String),
    /// The target path is not a real account directory contained directly within
    /// the managed-account root (a symlink/junction/reparse redirect, or
    /// otherwise outside it). Never followed into another profile or the
    /// standard Claude configuration.
    ProfileNotContained(String),
    /// Another lifecycle operation for this account is already in flight.
    ActionInProgress(String),
    Io(String),
}

impl ActionError {
    fn code(&self) -> &'static str {
        match self {
            ActionError::ReservedName(_) => "RESERVED_NAME",
            ActionError::NameInvalid => "NAME_INVALID",
            ActionError::NameUnsafe(_) => "NAME_UNSAFE",
            ActionError::AccountExists(_) => "ACCOUNT_EXISTS",
            ActionError::AccountNotFound(_) => "ACCOUNT_NOT_FOUND",
            ActionError::ProfileNotContained(_) => "PROFILE_NOT_CONTAINED",
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
            ActionError::NameUnsafe(n) => {
                format!(
                    "'{n}' is a reserved device name and is unsafe as an account directory; choose another name"
                )
            }
            ActionError::AccountExists(n) => {
                format!("a managed account named '{n}' already exists; create never replaces one")
            }
            ActionError::AccountNotFound(n) => {
                format!("no managed account named '{n}' to reauthenticate")
            }
            ActionError::ProfileNotContained(n) => {
                format!(
                    "'{n}' does not resolve to a real account directory inside the managed-account root; it may be a symlink, junction or reparse point and is refused"
                )
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
            | ActionError::NameUnsafe(n)
            | ActionError::AccountExists(n)
            | ActionError::AccountNotFound(n)
            | ActionError::ProfileNotContained(n)
            | ActionError::ActionInProgress(n) => json!({ "account": n }),
            ActionError::NameInvalid | ActionError::Io(_) => json!({}),
        }
    }
}

/// Classify the coarse outcome from the login child's exit, whether a local
/// identity is present afterward, and whether an authoritative audit confirmed
/// it. Pure, so every branch is unit-tested without a filesystem or a child
/// process. `authority` only matters on a clean exit; a non-zero exit is
/// `LoginFailed` regardless, and a failed spawn is `SpawnFailed` regardless.
pub fn classify_outcome(exit: LoginExit, identity_present: bool, authority: Authority) -> Outcome {
    match exit {
        LoginExit::SpawnFailed => Outcome::SpawnFailed,
        LoginExit::Exited(0) => match authority {
            Authority::Confirmed => Outcome::Authenticated,
            Authority::Unconfirmed if identity_present => Outcome::LoginCompleted,
            Authority::Unconfirmed => Outcome::LoginCompletedNoIdentity,
        },
        LoginExit::Exited(_) => Outcome::LoginFailed,
    }
}

/// Whether `name`, though syntactically valid per [`validate_name`], is unsafe
/// as a directory on some platform: a Windows reserved device name (`CON`,
/// `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`), with or without an
/// extension. Refused on every platform so the contract behaves identically
/// everywhere and a store created on one OS stays usable on another. (Path
/// separators, traversal, whitespace, dots and unicode are already rejected by
/// [`validate_name`].)
fn is_platform_unsafe_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = stem.strip_prefix(prefix)
            && rest.len() == 1
            && rest.as_bytes()[0].is_ascii_digit()
        {
            return true;
        }
    }
    false
}

/// Verify that the managed account for `name` resolves to a real directory that
/// is a *direct child* of the managed-account root — never a symlink, junction
/// or other reparse point redirecting out of it (F5). Refuses to follow a
/// redirected profile into another account or the standard Claude
/// configuration. Called after a create's directory exists and before a
/// reauth's login runs.
fn verify_contained_account_dir(config: &AppConfig, name: &str) -> Result<(), ActionError> {
    let acc = config.account_path(name);
    let meta = fs::symlink_metadata(&acc).map_err(|e| ActionError::Io(e.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(ActionError::ProfileNotContained(name.to_string()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(ActionError::ProfileNotContained(name.to_string()));
        }
    }
    if !meta.file_type().is_dir() {
        return Err(ActionError::ProfileNotContained(name.to_string()));
    }
    // Canonicalize both and require a direct parent/child relationship, so an
    // ancestor reparse point redirecting the account elsewhere is caught even
    // when the leaf itself is an ordinary directory.
    let accounts_root = config.accounts_dir();
    let (Ok(canon_acc), Ok(canon_root)) =
        (fs::canonicalize(&acc), fs::canonicalize(&accounts_root))
    else {
        return Err(ActionError::Io(
            "could not canonicalize the account path for the containment check".to_string(),
        ));
    };
    if canon_acc.parent() != Some(canon_root.as_path()) {
        return Err(ActionError::ProfileNotContained(name.to_string()));
    }
    Ok(())
}

/// The whole action, with the interactive login abstracted behind `run_login`
/// so it is drivable by a controlled fake in tests. Returns a defined report
/// once a login was attempted, or a non-destructive operational error.
///
/// Safety properties enforced here: create never replaces an existing account;
/// nothing touches the configured default or any routing link; the identity
/// pin is only ever *added* when absent (never silently moved); the account
/// directory is never rolled back when authentication fails; the target must be
/// a contained account directory; and only one lifecycle operation per account
/// runs at a time (the per-account lock, which human `add`/`login`/`remove`
/// also take — never the global routing lock across the login).
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
    if is_platform_unsafe_name(name) {
        return Err(ActionError::NameUnsafe(name.to_string()));
    }

    // Per-account lifecycle lock, held across the whole action including the
    // login. Non-blocking: another cooperating lifecycle operation for this
    // account in flight is reported, not waited on across an interactive login.
    let _lifecycle = match config.try_lock_account_lifecycle(name) {
        Ok(Some(guard)) => guard,
        Ok(None) => return Err(ActionError::ActionInProgress(name.to_string())),
        Err(e) => return Err(ActionError::Io(e.to_string())),
    };

    let acc_dir = config.account_path(name);
    let (existed_before, created) = match action {
        Action::Create => {
            // Reserve the name under the routing lock — the existence check and
            // directory creation are one inventory write, so a concurrent
            // machine `link` sees a consistent inventory and two creates of the
            // same name cannot both pass. The interactive login below is
            // deliberately NOT under the routing lock.
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
            // The freshly created directory must be a real contained directory —
            // refuse (without logging in) if the path is a reparse redirect.
            verify_contained_account_dir(config, name)?;
            if seed_from_default {
                // Best-effort, exactly like `add`: an empty account dir is still
                // usable, and a seed failure must not abort the login.
                let _ = seed::copy_user_config(&acc_dir);
            }
            (false, true)
        }
        Action::Reauth => {
            match fs::symlink_metadata(&acc_dir) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(ActionError::AccountNotFound(name.to_string()));
                }
                Err(e) => return Err(ActionError::Io(e.to_string())),
                Ok(_) => {}
            }
            // Never reauthenticate *into* a symlink/junction that redirects out
            // of the managed-account root.
            verify_contained_account_dir(config, name)?;
            (true, false)
        }
    };

    let exit = run_login(&acc_dir);
    let child_exit_code = match exit {
        LoginExit::Exited(code) => Some(code),
        LoginExit::SpawnFailed => None,
    };
    let local = identity::local_identity(&acc_dir);

    // One authoritative audit (only on a clean exit) serves BOTH the
    // `authenticated` determination and the duplicate disclosure.
    let (authority, identity_for_report, duplicate_of) =
        reconcile(config, name, &acc_dir, exit, local);

    let identity_present = match authority {
        Authority::Confirmed => true,
        Authority::Unconfirmed => identity_for_report.is_some(),
    };
    let outcome = classify_outcome(exit, identity_present, authority);

    // Pin only when the login completed with an identity to record, and only if
    // the account has no pin yet (`write_after_login` never moves an existing
    // pin — that swap is exactly the drift the pin exists to report).
    let mut pinned = false;
    if matches!(outcome, Outcome::Authenticated | Outcome::LoginCompleted) {
        super::lock::write_after_login(config, name);
        pinned = super::lock::json_state(config, name).1.is_some();
    }

    Ok(ActionReport {
        action,
        account: name.to_string(),
        existed_before,
        created,
        outcome,
        authority,
        child_exit_code,
        identity: identity_for_report,
        duplicate_of,
        pinned,
    })
}

/// Reconcile the just-finished login: on a clean exit, perform exactly one live
/// audit and, if it authoritatively confirms a credential, report the audited
/// identity and any duplicate; otherwise fall back to the (unverified) local
/// identity with no duplicate disclosure. On any non-clean exit, no audit is
/// attempted. Returns `(authority, identity_to_report, duplicate_of)`.
fn reconcile(
    config: &AppConfig,
    name: &str,
    acc_dir: &Path,
    exit: LoginExit,
    local: Option<Identity>,
) -> (Authority, Option<Identity>, Option<String>) {
    if !matches!(exit, LoginExit::Exited(0)) {
        return (Authority::Unconfirmed, local, None);
    }
    match identity::audit_account(acc_dir) {
        identity::AuditResult::Ok(profile) => match profile.uuid.clone() {
            Some(uuid) => {
                let known = super::known_account_cache_paths(config, name);
                let duplicate = identity::duplicate_from_profile(&profile, &known);
                let id = Identity {
                    uuid,
                    email: profile.email.clone(),
                };
                (Authority::Confirmed, Some(id), duplicate)
            }
            // Audited but with no stable uuid: cannot authoritatively confirm.
            None => (Authority::Unconfirmed, local, None),
        },
        identity::AuditResult::NoToken | identity::AuditResult::Offline => {
            (Authority::Unconfirmed, local, None)
        }
    }
}

/// The account-action-specific fields of a `ok:true` outcome document (without
/// the shared `schema_version`/`ok` envelope keys). Pure.
fn report_fields(report: &ActionReport) -> Value {
    let identity = match &report.identity {
        Some(id) => json!({ "uuid": id.uuid, "email": id.email }),
        None => Value::Null,
    };
    let identity_source = match report.authority {
        Authority::Confirmed => "live_audit",
        Authority::Unconfirmed if report.identity.is_some() => "local_cache",
        Authority::Unconfirmed => "none",
    };
    json!({
        "contract": "account-action",
        "contract_version": CONTRACT_VERSION,
        "action": report.action.as_str(),
        "account": report.account,
        "account_existed_before": report.existed_before,
        "account_created": report.created,
        "outcome": report.outcome.as_str(),
        // True ONLY when a live audit confirmed a current credential.
        "authenticated": report.outcome == Outcome::Authenticated,
        "requires_reconciliation": report.outcome.requires_reconciliation(),
        // Where `identity` came from: an authoritative audit, the (unverified)
        // local cache, or nothing.
        "identity_source": identity_source,
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
        // What the caller must supply as --result-file.
        "result_file_requirements":
            "absolute path in a caller-owned directory outside the provider and standard Claude stores; must not already exist",
        // Honest capability disclosure (upstream `claude auth login` limits).
        "oauth_code_entry_feedback": "unavailable",
        "progress_granularity": "final_outcome_only",
        "distinguishes_cancellation_from_failure": false,
        // The full outcome vocabulary and the reconciliation rule (F2).
        "outcomes": [
            "authenticated",
            "login_completed",
            "login_completed_no_identity",
            "login_failed",
            "spawn_failed"
        ],
        "authenticated_requires_live_audit": true,
        "reconciliation":
            "run doctor/list after any outcome other than 'authenticated' before trusting account state",
        // --seed applies only to create.
        "seed_supported_actions": ["create"],
        "secret_safe": true,
    })
}

/// The full capability document (envelope + fields). Only the field builder
/// [`probe_fields`] is used in the running binary (via `machine::success`); this
/// whole-envelope form exists for the tests that assert on the document.
#[cfg(test)]
fn probe_doc() -> Value {
    machine::build_success(probe_fields())
}

/// A validated, caller-owned result-file destination the outcome document will
/// be published to. Established BEFORE any account mutation (F1).
#[derive(Debug)]
struct ResultChannel {
    /// The destination's parent directory (validated to exist and be a real
    /// directory outside the provider/standard stores).
    dir: PathBuf,
    /// The destination file's own name (used to build a sibling temp).
    file_name: String,
    /// The final destination path.
    dest: PathBuf,
}

/// Best-effort canonicalization: the real path when it resolves, else the input
/// unchanged, so containment checks degrade to a lexical comparison rather than
/// failing open.
fn canonical_or_lexical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_within(child: &Path, ancestor: &Path) -> bool {
    let c = canonical_or_lexical(child);
    let a = canonical_or_lexical(ancestor);
    c == a || c.starts_with(&a)
}

/// Whether a result directory/destination lands inside provider-owned state (the
/// switch store, which holds the account directories, routing store and locks)
/// or the standard Claude configuration (`~/.claude/` or `~/.claude.json`).
/// Writing a result there could corrupt provider state or be mistaken for it.
fn result_destination_forbidden(config: &AppConfig, dir: &Path, dest: &Path) -> bool {
    if path_within(dir, &config.base_dir) {
        return true;
    }
    if let Some(std_dir) = identity::standard_token_dir() {
        if path_within(dir, &std_dir) {
            return true;
        }
        // ~/.claude.json itself (lives directly in the home dir).
        if let Some(home_json) = identity::local_identity_path(&std_dir)
            && let Some(home) = home_json.parent()
            && path_within(dir, home)
            && canonical_or_lexical(dir) == canonical_or_lexical(home)
            && dest.file_name().and_then(|n| n.to_str()) == Some(".claude.json")
        {
            return true;
        }
    }
    false
}

/// Validate and establish the result channel before any profile creation or
/// authentication (F1): the path must be absolute, name a file in an existing
/// real directory outside the provider/standard stores, and NOT already exist;
/// and the directory must be writable (proven by an exclusive probe create).
/// On failure returns `(code, message, details)` for a STDOUT error — there is
/// no trustworthy result file to write to.
fn prepare_result_channel(
    config: &AppConfig,
    result_path: &Path,
) -> Result<ResultChannel, (&'static str, String, Value)> {
    if !result_path.is_absolute() {
        return Err((
            "RESULT_FILE_NOT_ABSOLUTE",
            "--result-file must be an absolute path".to_string(),
            json!({ "result_file": result_path.display().to_string() }),
        ));
    }
    let file_name = result_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            (
                "RESULT_FILE_NOT_ABSOLUTE",
                "--result-file must name a file".to_string(),
                json!({ "result_file": result_path.display().to_string() }),
            )
        })?
        .to_string();
    let dir = result_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            (
                "RESULT_FILE_NOT_ABSOLUTE",
                "--result-file has no parent directory".to_string(),
                json!({ "result_file": result_path.display().to_string() }),
            )
        })?;

    // The destination must NOT already exist, in any form (file, directory,
    // symlink, reparse point) — an old or pre-existing result is never treated
    // as this action's outcome, and we never clobber one.
    match fs::symlink_metadata(result_path) {
        Ok(_) => {
            return Err((
                "RESULT_FILE_EXISTS",
                "--result-file already exists; supply a fresh, unique destination per invocation"
                    .to_string(),
                json!({ "result_file": result_path.display().to_string() }),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err((
                "RESULT_FILE_NOT_WRITABLE",
                format!("could not stat --result-file: {e}"),
                json!({ "result_file": result_path.display().to_string() }),
            ));
        }
    }

    // The parent must be an existing, real directory (reject a symlinked parent
    // as unsafe ancestry — deeper ancestry reparse points are a documented
    // residual).
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => {
            return Err((
                "RESULT_DIR_NOT_A_DIRECTORY",
                "--result-file's parent is not an ordinary directory".to_string(),
                json!({ "result_dir": dir.display().to_string() }),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err((
                "RESULT_DIR_NOT_FOUND",
                "--result-file's parent directory does not exist".to_string(),
                json!({ "result_dir": dir.display().to_string() }),
            ));
        }
        Err(e) => {
            return Err((
                "RESULT_FILE_NOT_WRITABLE",
                format!("could not stat --result-file's parent directory: {e}"),
                json!({ "result_dir": dir.display().to_string() }),
            ));
        }
    }

    if result_destination_forbidden(config, &dir, result_path) {
        return Err((
            "RESULT_FILE_FORBIDDEN_LOCATION",
            "--result-file may not be inside the claude-acc store or the standard Claude configuration"
                .to_string(),
            json!({ "result_file": result_path.display().to_string() }),
        ));
    }

    // Prove writability now (before any mutation) with an exclusively created
    // probe temp we immediately remove — we never adopt a file we did not make.
    let probe = dir.join(format!(
        ".{file_name}.cm9probe.{}.{}",
        std::process::id(),
        PUBLISH_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            // Close the handle BEFORE removing — on Windows a file opened
            // without share-delete cannot be removed while a handle is open,
            // which would otherwise leave the probe file behind.
            drop(file);
            let _ = fs::remove_file(&probe);
        }
        Err(e) => {
            return Err((
                "RESULT_FILE_NOT_WRITABLE",
                format!("--result-file's directory is not writable: {e}"),
                json!({ "result_dir": dir.display().to_string() }),
            ));
        }
    }

    Ok(ResultChannel {
        dir,
        file_name,
        dest: result_path.to_path_buf(),
    })
}

/// Publish `doc` to the channel's destination atomically, without replacing an
/// existing destination (F1). Writes a uniquely named, exclusively created
/// sibling temp, fsyncs it, re-checks the destination is still absent, then
/// renames. Cleans up only its own temp. A residual filesystem race remains:
/// std has no portable atomic no-clobber rename, so a destination created in the
/// narrow window between the re-check and the rename could be replaced — the
/// re-check narrows but cannot close it (documented in docs/machine-api.md).
fn publish_result(channel: &ResultChannel, doc: &Value) -> io::Result<()> {
    let mut serialized = serde_json::to_string_pretty(doc).map_err(io::Error::other)?;
    serialized.push('\n');
    let tmp = channel.dir.join(format!(
        ".{}.cm9tmp.{}.{}",
        channel.file_name,
        std::process::id(),
        PUBLISH_SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    // Exclusive create: never adopt a temp we did not make.
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    let written = f
        .write_all(serialized.as_bytes())
        .and_then(|()| f.flush())
        .and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    // Re-check the destination is still absent right before publishing.
    match fs::symlink_metadata(&channel.dest) {
        Ok(_) => {
            let _ = fs::remove_file(&tmp);
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "result destination appeared before publish",
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    }

    if let Err(e) = fs::rename(&tmp, &channel.dest) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Whether the provider store has actually been initialized. A lifecycle action
/// requires it and must NOT manufacture it as a side effect (F4) — an
/// uninitialized store is a structured `NOT_INITIALIZED` refusal.
fn store_initialized(config: &AppConfig) -> bool {
    config.accounts_dir().is_dir() && config.config_path().exists() && config.links_path().exists()
}

/// The production interactive login: run `claude auth login` scoped to
/// `acc_dir`, bracketed by the standard-account keychain snapshot/restore (a
/// login can clobber the standard account's own Keychain entries as a side
/// effect, even scoped) AND serialized against every other cooperating login by
/// the global auth-side-effect lock (macOS only; a no-op elsewhere), so two
/// logins cannot interleave their snapshot/restore. Reuses the shared
/// `ClaudeProcess` substrate, so exe resolution, structured argv and the
/// auth-env scrub are identical to `add`/`login`. The terminal is inherited —
/// the caller is expected to have hosted one.
fn run_login_production(config: &AppConfig, acc_dir: &Path) -> LoginExit {
    let _auth = config.lock_auth_side_effect_blocking().ok().flatten();
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

/// Build the outcome (or operational-refusal) document for a create/reauth,
/// validating the action, the supported options and the store BEFORE any
/// mutation (F4). Returns the document and the process exit code; the caller
/// publishes it to the already-validated result channel.
fn build_outcome(
    config: &AppConfig,
    action_str: &str,
    name: &str,
    seed_from_default: bool,
) -> (Value, i32) {
    let action = match Action::parse(action_str) {
        Some(act) => act,
        None => {
            return (
                machine::build_error(
                    "ACTION_INVALID",
                    "--action must be 'create' or 'reauth'",
                    json!({ "action": action_str }),
                ),
                1,
            );
        }
    };

    // --seed applies only to create; never silently accepted for reauth (F4).
    if matches!(action, Action::Reauth) && seed_from_default {
        return (
            machine::build_error(
                "SEED_NOT_SUPPORTED",
                "--seed applies only to 'create'; 'reauth' never seeds a profile",
                json!({ "action": "reauth" }),
            ),
            1,
        );
    }

    // A lifecycle action requires an initialized store and never manufactures
    // one — an invalid or refused request cannot initialize routing state (F4).
    if !store_initialized(config) {
        return (
            machine::build_error(
                "NOT_INITIALIZED",
                "claude-acc has no initialized account store yet; run a claude-acc command to initialize it first",
                json!({}),
            ),
            1,
        );
    }

    let run_login = |acc_dir: &Path| run_login_production(config, acc_dir);
    match perform_action(config, action, name, seed_from_default, &run_login) {
        Ok(report) => (report_to_doc(&report), 0),
        Err(error) => (error_to_doc(&error), 1),
    }
}

/// CLI entry point for `account-action`. Machine-only: human use is `add` /
/// `login`. `--probe` writes the capability document to stdout; a create/reauth
/// writes its single outcome document to `--result-file` and keeps stdout clear
/// for the interactive login the caller's terminal is hosting.
///
/// Errors about the *invocation shape* (`JSON_REQUIRED`, `INVALID_ARGUMENTS`)
/// and about the *result channel itself* (`RESULT_FILE_*`, `RESULT_DIR_*`) go
/// to STDOUT, because there is no trustworthy result file to write them to.
/// Every outcome of a validated action — success or operational refusal — is
/// published to the validated result file. A publish failure *after* the action
/// ran is reported on stdout as `RESULT_FILE_PUBLISH_FAILED`: the account state
/// may have changed and the consumer must reconcile, never auto-retry.
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

    // Establish the result channel BEFORE any profile creation or
    // authentication (F1). A bad channel is a stdout error.
    let channel = match prepare_result_channel(config, Path::new(result_path)) {
        Ok(channel) => channel,
        Err((code, message, details)) => return machine::error(code, &message, details),
    };

    let (doc, exit_code) = build_outcome(config, action_str, name, seed_from_default);

    match publish_result(&channel, &doc) {
        Ok(()) => exit_code,
        Err(e) => machine::error(
            "RESULT_FILE_PUBLISH_FAILED",
            &format!(
                "the account action ran but its outcome could not be published to the result file ({e}); account state may have changed — reconcile with doctor/list and do not retry automatically"
            ),
            json!({ "result_file": channel.dest.display().to_string() }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SCRATCH_SEQ: AtomicU32 = AtomicU32::new(0);

    /// A fully initialized, isolated provider store (accounts dir + config +
    /// links), so `store_initialized` holds and `run_json` exercises the real
    /// paths without touching the real ~/.claude-switch.
    fn scratch(tag: &str) -> AppConfig {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "cc-acctaction-{}-{}-{}",
            tag,
            std::process::id(),
            seq
        ));
        let _ = fs::remove_dir_all(&base);
        let config = AppConfig { base_dir: base };
        config.init().unwrap();
        config
    }

    /// A fresh result-file destination in its OWN directory, deliberately
    /// OUTSIDE any provider store, that does not yet exist.
    fn result_path(tag: &str) -> PathBuf {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "cc-acctaction-RESULTS-{}-{}-{}",
            tag,
            std::process::id(),
            seq
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("result.json")
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

    // --- pure classification (F2) ---

    #[test]
    fn classify_covers_every_outcome_branch() {
        assert_eq!(
            classify_outcome(LoginExit::Exited(0), true, Authority::Confirmed),
            Outcome::Authenticated
        );
        // A clean exit with a local identity but NO authoritative confirmation
        // is login_completed, never authenticated — the F2 correctness fix.
        assert_eq!(
            classify_outcome(LoginExit::Exited(0), true, Authority::Unconfirmed),
            Outcome::LoginCompleted
        );
        assert_eq!(
            classify_outcome(LoginExit::Exited(0), false, Authority::Unconfirmed),
            Outcome::LoginCompletedNoIdentity
        );
        assert_eq!(
            classify_outcome(LoginExit::Exited(1), true, Authority::Unconfirmed),
            Outcome::LoginFailed
        );
        assert_eq!(
            classify_outcome(LoginExit::Exited(130), false, Authority::Unconfirmed),
            Outcome::LoginFailed
        );
        assert_eq!(
            classify_outcome(LoginExit::SpawnFailed, false, Authority::Unconfirmed),
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

    #[test]
    fn reserved_device_names_are_refused_as_unsafe() {
        for n in [
            "con", "CON", "nul", "Aux", "prn", "com1", "LPT9", "con.json",
        ] {
            assert!(is_platform_unsafe_name(n), "{n} must be unsafe");
        }
        for n in [
            "work",
            "console",
            "com",
            "com10",
            "lpt",
            "com0extra",
            "nulls",
        ] {
            assert!(!is_platform_unsafe_name(n), "{n} must be allowed");
        }
    }

    // --- create ---

    #[test]
    fn create_offline_is_login_completed_creates_pins_and_records_the_local_identity() {
        // Offline (no token, no network) a clean login is login_completed, not
        // authenticated: the credential was never authoritatively verified.
        let c = scratch("create-ok");
        let login = fake_login_success("u-new", Some("new@example.com"));
        let report = perform_action(&c, Action::Create, "fresh", false, &login).unwrap();

        assert_eq!(report.outcome, Outcome::LoginCompleted);
        assert_eq!(report.authority, Authority::Unconfirmed);
        assert!(report.created);
        assert!(!report.existed_before);
        assert_eq!(report.child_exit_code, Some(0));
        assert_eq!(report.identity.as_ref().unwrap().uuid, "u-new");
        assert!(report.pinned, "a freshly logged-in account is pinned");
        assert!(c.account_path("fresh").is_dir());
        let doc = report_to_doc(&report);
        assert_eq!(doc["authenticated"], false);
        assert_eq!(doc["requires_reconciliation"], true);
        assert_eq!(doc["identity_source"], "local_cache");
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
    fn create_rejects_reserved_invalid_and_unsafe_names_before_touching_the_store() {
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
        assert_eq!(
            perform_action(&c, Action::Create, "nul", false, &login).unwrap_err(),
            ActionError::NameUnsafe("nul".to_string())
        );
        assert!(
            c.list_accounts().unwrap().is_empty(),
            "no account dir created for a bad name"
        );
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn create_that_fails_authentication_keeps_the_directory_and_does_not_pin() {
        // Defined behavior: account created, login exited non-zero -> the dir
        // stays (no destructive rollback), login_failed, no pin.
        let c = scratch("create-authfail");
        let login = |_acc: &Path| LoginExit::Exited(1);
        let report = perform_action(&c, Action::Create, "halfborn", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::LoginFailed);
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
    fn create_success_but_no_local_identity_is_login_completed_no_identity() {
        let c = scratch("create-noident");
        let login = |_acc: &Path| LoginExit::Exited(0); // exits 0 but writes no identity
        let report = perform_action(&c, Action::Create, "ghost", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::LoginCompletedNoIdentity);
        assert!(!report.pinned);
        assert!(report.identity.is_none());
        let doc = report_to_doc(&report);
        assert_eq!(doc["identity_source"], "none");
        assert_eq!(doc["requires_reconciliation"], true);
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
        // spawn_failed does not demand reconciliation — nothing ran.
        assert_eq!(report_to_doc(&report)["requires_reconciliation"], false);
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
    fn reauth_of_an_existing_account_logs_in_without_recreating() {
        let c = scratch("reauth-ok");
        fs::create_dir_all(c.account_path("work")).unwrap();
        let login = fake_login_success("u-work", Some("work@example.com"));
        let report = perform_action(&c, Action::Reauth, "work", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::LoginCompleted);
        assert!(report.existed_before);
        assert!(!report.created);
        assert_eq!(report.identity.as_ref().unwrap().uuid, "u-work");
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn a_reauth_with_a_stale_cached_identity_and_a_clean_exit_is_not_authenticated() {
        // F2 repro: an OLD local identity remains, the login exits 0 but writes
        // no new authentication (offline, no token). This must be login_completed
        // with the stale identity surfaced as UNVERIFIED — never authenticated,
        // and the existing pin must not move.
        let c = scratch("reauth-stale");
        let acc = c.account_path("stale");
        fs::create_dir_all(&acc).unwrap();
        // Pre-existing (stale) local identity + a pin naming it.
        fs::write(
            acc.join(".claude.json"),
            json!({"oauthAccount": {"accountUuid": "u-old", "emailAddress": "old@example.com"}})
                .to_string(),
        )
        .unwrap();
        identity::write_lock_at(
            &acc.join(identity::LOCK_FILE),
            &Identity {
                uuid: "u-old".to_string(),
                email: Some("old@example.com".to_string()),
            },
        )
        .unwrap();

        // A login that changes nothing and exits 0.
        let login = |_acc: &Path| LoginExit::Exited(0);
        let report = perform_action(&c, Action::Reauth, "stale", false, &login).unwrap();

        assert_eq!(report.outcome, Outcome::LoginCompleted);
        assert_eq!(report.authority, Authority::Unconfirmed);
        let doc = report_to_doc(&report);
        assert_eq!(doc["authenticated"], false, "stale cache is NOT proof");
        assert_eq!(doc["requires_reconciliation"], true);
        assert_eq!(doc["identity_source"], "local_cache");
        assert_eq!(doc["identity"]["uuid"], "u-old");
        // The pre-existing pin still names the original identity.
        let raw = fs::read_to_string(acc.join(identity::LOCK_FILE)).unwrap();
        assert!(raw.contains("u-old"), "the pin must not move: {raw}");
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn reauth_does_not_move_an_existing_pin_even_when_signed_in_as_someone_else() {
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
        assert_eq!(report.outcome, Outcome::LoginCompleted);
        // The pin file still names the original identity.
        let raw = fs::read_to_string(acc.join(identity::LOCK_FILE)).unwrap();
        assert!(raw.contains("u-original"), "the pin must not move: {raw}");
        assert!(!raw.contains("u-different"));
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- F5 containment ---

    #[test]
    #[cfg(unix)]
    fn reauth_into_a_symlinked_account_directory_is_refused() {
        use std::os::unix::fs::symlink;
        let c = scratch("reauth-symlink");
        // An external directory the symlinked "account" redirects into.
        let outside = std::env::temp_dir().join(format!(
            "cc-acctaction-OUTSIDE-{}-{}",
            std::process::id(),
            SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, c.account_path("evil")).unwrap();

        let login = fake_login_success("u", None);
        assert_eq!(
            perform_action(&c, Action::Reauth, "evil", false, &login).unwrap_err(),
            ActionError::ProfileNotContained("evil".to_string())
        );
        // The login must NOT have run into the redirected directory.
        assert!(!outside.join(".claude.json").exists());
        let _ = fs::remove_dir_all(&outside);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- duplicate identity advisory ---

    #[test]
    fn the_duplicate_advisory_does_not_false_positive_without_a_live_audit() {
        // The duplicate-identity advisory is keyed on a LIVE audit of the new
        // account (token read + profile fetch). With no retrievable token (as
        // here, and in any offline unit test) authentication is Unconfirmed, so
        // there is no profile to compare and no "duplicate_of" is ever emitted.
        let c = scratch("dup");
        let existing = c.account_path("personal1");
        fs::create_dir_all(&existing).unwrap();
        fs::write(
            existing.join(".account-info.json"),
            json!({"uuid": "u-shared", "email": "shared@example.com"}).to_string(),
        )
        .unwrap();
        let login = fake_login_success("u-shared", Some("shared@example.com"));
        let report = perform_action(&c, Action::Create, "personal2", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::LoginCompleted);
        assert_eq!(report.duplicate_of, None);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- concurrency / lifecycle lock (F3) ---

    #[test]
    fn a_second_action_for_the_same_account_is_refused_while_one_is_in_flight() {
        let c = scratch("concurrent");
        let acc = "busy";
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
                fs::write(
                    acc_dir.join(".claude.json"),
                    json!({"oauthAccount": {"accountUuid": "u-busy"}}).to_string(),
                )
                .unwrap();
                LoginExit::Exited(0)
            }
        };
        let report = perform_action(&c, Action::Create, acc, false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::LoginCompleted);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn an_action_for_a_different_account_is_not_blocked() {
        let c = scratch("concurrent-distinct");
        let inner = AppConfig {
            base_dir: c.base_dir.clone(),
        };
        let login = move |acc_dir: &Path| {
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
        assert_eq!(report.outcome, Outcome::LoginCompleted);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn an_account_action_is_refused_while_a_human_lifecycle_lock_is_held() {
        // The per-account lifecycle lock is the SAME lock a human add/login/
        // remove takes, so a machine action for that account is refused while a
        // human operation holds it — and a different account is unaffected.
        //
        // Note: this simulates "human" and "machine" in ONE process with two
        // file descriptors on the lock file. `File::lock`'s same-process,
        // multi-descriptor behavior is UNSPECIFIED (it differs on macOS/BSD),
        // so we only assert the two things that matter and hold cross-platform:
        // (a) the same account is refused while the lock is held, and (b) a
        // different account is unaffected. Lock RELEASE on drop is proven
        // separately and robustly in config.rs (single-pattern, not interleaved)
        // — and in production the human and machine operations are separate
        // processes, where flock is fully specified.
        let c = scratch("human-conflict");
        fs::create_dir_all(c.account_path("busy")).unwrap();
        let human = c.lock_account_lifecycle_blocking("busy").unwrap();

        // (a) same account refused while the (human) lock is held.
        let login = fake_login_success("u", None);
        assert_eq!(
            perform_action(&c, Action::Reauth, "busy", false, &login).unwrap_err(),
            ActionError::ActionInProgress("busy".to_string())
        );

        // (b) a different account acquires its own lock freely.
        fs::create_dir_all(c.account_path("free")).unwrap();
        let free = fake_login_success("u-free", None);
        let report = perform_action(&c, Action::Reauth, "free", false, &free).unwrap();
        assert_eq!(report.outcome, Outcome::LoginCompleted);

        drop(human);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- preservation of unrelated state ---

    #[test]
    fn an_action_never_touches_the_default_or_links_or_other_accounts() {
        let c = scratch("preserve");
        fs::create_dir_all(c.account_path("sibling")).unwrap();
        c.set_default("sibling").unwrap();
        fs::write(c.links_path(), "/some/dir=sibling\n").unwrap();
        let before_links = fs::read_to_string(c.links_path()).unwrap();

        let login = fake_login_success("u-new", Some("new@example.com"));
        let report = perform_action(&c, Action::Create, "newcomer", false, &login).unwrap();
        assert_eq!(report.outcome, Outcome::LoginCompleted);

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
    fn the_authenticated_outcome_document_has_the_envelope_shape_and_no_secret_fields() {
        let report = ActionReport {
            action: Action::Create,
            account: "x".to_string(),
            existed_before: false,
            created: true,
            outcome: Outcome::Authenticated,
            authority: Authority::Confirmed,
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
        assert_eq!(doc["requires_reconciliation"], false);
        assert_eq!(doc["identity_source"], "live_audit");
        assert_eq!(doc["identity"]["uuid"], "u");
        assert_eq!(doc["identity"]["email"], "e@x.com");
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
        assert_eq!(doc["authenticated_requires_live_audit"], true);
        assert_eq!(doc["seed_supported_actions"], json!(["create"]));
        assert_eq!(doc["secret_safe"], true);
        assert_eq!(doc["provider_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            doc["outcomes"],
            json!([
                "authenticated",
                "login_completed",
                "login_completed_no_identity",
                "login_failed",
                "spawn_failed"
            ])
        );
    }

    // --- result channel (F1) ---

    #[test]
    fn prepare_result_channel_refuses_a_relative_path() {
        let c = scratch("rc-relative");
        let err = prepare_result_channel(&c, Path::new("result.json")).unwrap_err();
        assert_eq!(err.0, "RESULT_FILE_NOT_ABSOLUTE");
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn prepare_result_channel_refuses_an_existing_destination() {
        let c = scratch("rc-exists");
        let dest = result_path("rc-exists");
        fs::write(&dest, "old result").unwrap();
        let err = prepare_result_channel(&c, &dest).unwrap_err();
        assert_eq!(err.0, "RESULT_FILE_EXISTS");
        // The pre-existing file is left intact — never clobbered or adopted.
        assert_eq!(fs::read_to_string(&dest).unwrap(), "old result");
        let _ = fs::remove_dir_all(dest.parent().unwrap());
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn prepare_result_channel_refuses_a_missing_parent_directory() {
        let c = scratch("rc-noparent");
        let dest = std::env::temp_dir()
            .join(format!("cc-rc-missing-{}", std::process::id()))
            .join("nope")
            .join("result.json");
        let _ = fs::remove_dir_all(dest.parent().unwrap().parent().unwrap());
        let err = prepare_result_channel(&c, &dest).unwrap_err();
        assert_eq!(err.0, "RESULT_DIR_NOT_FOUND");
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn prepare_result_channel_refuses_a_destination_inside_the_provider_store() {
        let c = scratch("rc-forbidden");
        // Inside the provider store (base_dir) — forbidden.
        let dest = c.base_dir.join("result.json");
        let err = prepare_result_channel(&c, &dest).unwrap_err();
        assert_eq!(err.0, "RESULT_FILE_FORBIDDEN_LOCATION");
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn prepare_then_publish_writes_atomically_and_leaves_no_temp() {
        let c = scratch("rc-publish");
        let dest = result_path("rc-publish");
        let channel = prepare_result_channel(&c, &dest).unwrap();
        let doc = report_to_doc(&ActionReport {
            action: Action::Reauth,
            account: "x".to_string(),
            existed_before: true,
            created: false,
            outcome: Outcome::LoginFailed,
            authority: Authority::Unconfirmed,
            child_exit_code: Some(2),
            identity: None,
            duplicate_of: None,
            pinned: false,
        });
        publish_result(&channel, &doc).unwrap();
        let back: Value = serde_json::from_str(&fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(back["outcome"], "login_failed");
        let leftovers: Vec<_> = fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".cm9tmp."))
            .collect();
        assert!(leftovers.is_empty(), "no temp left behind: {leftovers:?}");
        let _ = fs::remove_dir_all(dest.parent().unwrap());
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn publish_refuses_a_destination_that_appeared_after_prepare() {
        // Models the narrow race: the destination is created between prepare and
        // publish. Publish must refuse rather than clobber it, and clean its temp.
        let c = scratch("rc-race");
        let dest = result_path("rc-race");
        let channel = prepare_result_channel(&c, &dest).unwrap();
        fs::write(&dest, "someone-else").unwrap();
        let doc = probe_doc();
        let err = publish_result(&channel, &doc).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            "someone-else",
            "the destination that appeared must not be clobbered"
        );
        let leftovers: Vec<_> = fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".cm9tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp cleaned on refusal: {leftovers:?}"
        );
        let _ = fs::remove_dir_all(dest.parent().unwrap());
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // --- run_json routing / validation (the thin CLI shell) ---

    #[test]
    fn run_json_without_the_json_flag_is_refused() {
        let c = scratch("no-json");
        assert_eq!(run_json(&c, false, false, None, None, None, false), 1);
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn run_json_probe_returns_zero_and_touches_nothing() {
        // Probe must not require or create a store (it runs before init is
        // skipped in main). An uninitialized base stays uninitialized.
        let base = std::env::temp_dir().join(format!(
            "cc-acctaction-PROBE-{}-{}",
            std::process::id(),
            SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        let c = AppConfig {
            base_dir: base.clone(),
        };
        assert_eq!(run_json(&c, true, true, None, None, None, false), 0);
        assert!(!base.exists(), "probe created nothing");
    }

    #[test]
    fn run_json_with_an_invalid_action_writes_an_error_document_to_the_result_file() {
        let c = scratch("bad-action");
        let out = result_path("bad-action");
        let code = run_json(
            &c,
            true,
            false,
            Some("delete"),
            Some("x"),
            Some(out.to_str().unwrap()),
            false,
        );
        assert_eq!(code, 1);
        let doc: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(doc["ok"], false);
        assert_eq!(doc["error"]["code"], "ACTION_INVALID");
        let _ = fs::remove_dir_all(out.parent().unwrap());
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn run_json_refuses_seed_for_reauth() {
        let c = scratch("seed-reauth");
        fs::create_dir_all(c.account_path("work")).unwrap();
        let out = result_path("seed-reauth");
        let code = run_json(
            &c,
            true,
            false,
            Some("reauth"),
            Some("work"),
            Some(out.to_str().unwrap()),
            true, // --seed
        );
        assert_eq!(code, 1);
        let doc: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(doc["error"]["code"], "SEED_NOT_SUPPORTED");
        let _ = fs::remove_dir_all(out.parent().unwrap());
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    #[test]
    fn run_json_refuses_an_action_against_an_uninitialized_store_and_creates_nothing() {
        // An uninitialized store: the action is refused with NOT_INITIALIZED and
        // NO routing/account state is manufactured (F4).
        let base = std::env::temp_dir().join(format!(
            "cc-acctaction-UNINIT-{}-{}",
            std::process::id(),
            SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap(); // base exists, but not initialized
        let c = AppConfig {
            base_dir: base.clone(),
        };
        let out = result_path("uninit");
        let code = run_json(
            &c,
            true,
            false,
            Some("create"),
            Some("fresh"),
            Some(out.to_str().unwrap()),
            false,
        );
        assert_eq!(code, 1);
        let doc: Value = serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(doc["error"]["code"], "NOT_INITIALIZED");
        assert!(!c.accounts_dir().exists(), "no accounts store manufactured");
        assert!(!c.config_path().exists(), "no config manufactured");
        assert!(!c.links_path().exists(), "no links store manufactured");
        let _ = fs::remove_dir_all(out.parent().unwrap());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn run_json_refuses_to_overwrite_an_existing_result_file_and_leaves_it_intact() {
        let c = scratch("rc-run-exists");
        let out = result_path("rc-run-exists");
        fs::write(&out, "prior result").unwrap();
        let code = run_json(
            &c,
            true,
            false,
            Some("create"),
            Some("fresh"),
            Some(out.to_str().unwrap()),
            false,
        );
        // Stdout error, nonzero; the pre-existing file is untouched and no
        // account was created.
        assert_eq!(code, 1);
        assert_eq!(fs::read_to_string(&out).unwrap(), "prior result");
        assert!(!c.account_path("fresh").exists(), "no account created");
        let _ = fs::remove_dir_all(out.parent().unwrap());
        let _ = fs::remove_dir_all(&c.base_dir);
    }

    // NOTE: the create/reauth success path through `run_json` is intentionally
    // NOT exercised end-to-end here. It would call `run_login_production`, which
    // spawns a real `claude auth login`; on a runner that happened to have
    // `claude` installed that would open an interactive browser login and hang.
    // `perform_action` is covered exhaustively above with an injected fake
    // login, and `publish_result` / `prepare_result_channel` are covered
    // directly — together they cover every branch `run_json` composes.
}
