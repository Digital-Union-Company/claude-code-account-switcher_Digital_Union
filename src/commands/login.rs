use crate::claude_process::{ClaudeProcess, ClaudeProfile, current_dir, manager_bin_dir};
use crate::config::{AppConfig, validate_name};
use crate::i18n::{I18n, Msg};
use crate::identity;
use std::path::Path;

/// Build `claude auth login`. `None` preserves the standard-account marker;
/// every other launch detail is owned by the shared ClaudeProcess substrate.
fn build_login_process(config: &AppConfig, acc_dir: Option<&Path>) -> ClaudeProcess {
    let profile = acc_dir
        .map(|dir| ClaudeProfile::Named(dir.to_path_buf()))
        .unwrap_or(ClaudeProfile::Default);
    ClaudeProcess::new(
        ["auth", "login"],
        profile,
        current_dir(),
        manager_bin_dir(&config.base_dir),
    )
}

pub fn run(config: &AppConfig, i18n: &I18n, name: &str) {
    if name == "default" {
        login_default(config, i18n);
        return;
    }

    if !validate_name(name) {
        i18n.print(Msg::NameInvalid);
        std::process::exit(1);
    }

    let acc_dir = config.account_path(name);
    if !acc_dir.is_dir() {
        i18n.print(Msg::LoginNotFound(name.to_string()));
        std::process::exit(1);
    }

    i18n.print(Msg::LoginStart(name.to_string()));
    // See identity::snapshot_side_effect_keychain: `claude auth login` can
    // clobber the standard account's own Keychain entries as a side effect
    // even when scoped to acc_dir's CLAUDE_CONFIG_DIR.
    let keychain_snapshot = identity::snapshot_side_effect_keychain();
    let code = super::spawn_claude(build_login_process(config, Some(&acc_dir)), i18n);
    identity::restore_side_effect_keychain(keychain_snapshot);
    if code != 0 {
        std::process::exit(code);
    }

    warn_if_duplicate(config, i18n, name, &acc_dir);
    // Pins only if this account has none yet. Re-logging in to a pinned
    // account must not move the pin — that swap is the drift it reports.
    super::lock::write_after_login(config, name);

    i18n.print(Msg::LoginDone);
}

/// Re-login the standard `~/.claude` account. No keychain snapshot/restore
/// here — unlike logging into a *different* account, this login is meant to
/// change the standard account's own credentials.
fn login_default(config: &AppConfig, i18n: &I18n) {
    i18n.print(Msg::LoginStart("default".to_string()));
    let code = super::spawn_claude(build_login_process(config, None), i18n);
    if code != 0 {
        std::process::exit(code);
    }

    if let Some(dir) = identity::standard_token_dir() {
        warn_if_duplicate(config, i18n, "~/.claude/", &dir);
    }
    super::lock::write_after_login(config, "default");

    i18n.print(Msg::LoginDone);
}

/// Best-effort: hint if `label`'s just-refreshed login turned out to be the
/// same identity as an already-known account (from a prior doctor run's
/// cache — see identity::find_duplicate_account). Never blocks; just a
/// heads-up.
fn warn_if_duplicate(config: &AppConfig, i18n: &I18n, label: &str, acc_dir: &std::path::Path) {
    let known = super::known_account_cache_paths(config, label);
    if let Some(existing) = identity::find_duplicate_account(acc_dir, &known) {
        i18n.print(Msg::DuplicateAccountWarning(label.to_string(), existing));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn config() -> AppConfig {
        AppConfig {
            base_dir: PathBuf::from("manager root"),
        }
    }

    #[test]
    fn named_account_builds_named_profile() {
        let process = build_login_process(&config(), Some(Path::new("account path")));
        assert_eq!(
            process.profile(),
            &ClaudeProfile::Named(PathBuf::from("account path"))
        );
    }

    #[test]
    fn default_account_builds_default_profile() {
        let process = build_login_process(&config(), None);
        assert_eq!(process.profile(), &ClaudeProfile::Default);
    }

    #[test]
    fn login_process_uses_claude_auth_login_args() {
        let process = build_login_process(&config(), None);
        assert_eq!(
            process.argv(),
            [OsStr::new("auth"), OsStr::new("login")]
        );
    }
}
