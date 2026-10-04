use crate::claude_process::{ClaudeProcess, ClaudeProfile, current_dir, manager_bin_dir};
use crate::config::{AppConfig, is_reserved_name, validate_name};
use crate::i18n::{I18n, Msg};
use crate::ide;
use crate::identity;
use crate::seed;
use std::fs;
use std::path::Path;

/// Build `claude auth login` for the new account. Resolution, structured argv,
/// profile environment, authentication scrub, cwd, and spawning are all owned
/// by the shared ClaudeProcess substrate.
fn build_login_process(config: &AppConfig, acc_dir: &Path) -> ClaudeProcess {
    ClaudeProcess::new(
        ["auth", "login"],
        ClaudeProfile::Named(acc_dir.to_path_buf()),
        current_dir(),
        manager_bin_dir(&config.base_dir),
    )
}

pub fn run(config: &AppConfig, i18n: &I18n, name: &str, seed_from_default: bool) {
    if is_reserved_name(name) {
        i18n.print(Msg::ReservedName(name.to_string()));
        std::process::exit(1);
    }

    if !validate_name(name) {
        i18n.print(Msg::NameInvalid);
        std::process::exit(1);
    }

    let acc_dir = config.account_path(name);
    if acc_dir.is_dir() {
        i18n.print(Msg::AddExists(name.to_string()));
        std::process::exit(1);
    }

    fs::create_dir_all(&acc_dir).expect("Failed to create account directory");
    ide::ensure_account_symlink(&acc_dir).ok();

    // Seed before printing AddCreated, so the copy report is logically
    // attached to "what the new account got". Errors here are non-fatal —
    // an empty account dir is still usable.
    if seed_from_default {
        match seed::copy_user_config(&acc_dir) {
            Ok(report) if report.is_empty() => {
                i18n.print(Msg::SeedNothingToCopy);
            }
            Ok(report) => {
                for entry in &report.copied {
                    i18n.print(Msg::SeedCopied(entry.clone()));
                }
            }
            Err(e) => eprintln!("seed: {}", e),
        }
    }

    i18n.print(Msg::AddCreated(name.to_string()));

    // `claude auth login` can write to the standard account's own Keychain
    // entries as a side effect, even though this login is scoped to
    // acc_dir's CLAUDE_CONFIG_DIR — snapshot/restore undoes that collateral.
    // See identity::snapshot_side_effect_keychain for why.
    let keychain_snapshot = identity::snapshot_side_effect_keychain();
    let code = super::spawn_claude(build_login_process(config, &acc_dir), i18n);
    identity::restore_side_effect_keychain(keychain_snapshot);

    // The account directory stays — it may already be seeded, and `login` is
    // the natural retry. What must not stay is the success banner: printing
    // "Done. Use:" over a login that never happened told a human the wrong
    // thing and told a script nothing at all, since the exit code was 0.
    if code != 0 {
        i18n.print(Msg::AddLoginFailed(name.to_string()));
        std::process::exit(code);
    }

    // Best-effort: hint if this login turned out to be the same identity as
    // an already-known account (a leftover from a prior doctor run's cache —
    // see identity::find_duplicate_account). Never blocks; just a heads-up.
    let known = super::known_account_cache_paths(config, name);
    if let Some(existing) = identity::find_duplicate_account(&acc_dir, &known) {
        i18n.print(Msg::DuplicateAccountWarning(name.to_string(), existing));
    }

    // Record which account this directory now belongs to, so a later
    // re-login as somebody else is reported rather than silently accepted.
    // Best-effort: the login is what mattered, and `claude-acc lock` can
    // always do this by hand.
    super::lock::write_after_login(config, name);

    println!();
    i18n.print(Msg::AddDone);
    i18n.print(Msg::AddHintDefault(name.to_string()));
    i18n.print(Msg::AddHintLink(name.to_string()));
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
    fn login_process_sets_named_profile() {
        let process = build_login_process(&config(), Path::new("account path"));
        assert_eq!(
            process.profile(),
            &ClaudeProfile::Named(PathBuf::from("account path"))
        );
    }

    #[test]
    fn login_process_uses_claude_auth_login_args() {
        let process = build_login_process(&config(), Path::new("account path"));
        assert_eq!(process.argv(), [OsStr::new("auth"), OsStr::new("login")]);
    }
}
