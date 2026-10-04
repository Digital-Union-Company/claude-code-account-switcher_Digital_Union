pub mod activate;
pub mod add;
pub mod clone_settings;
pub mod completions;
pub mod default;
pub mod desktop;
pub mod doctor;
pub mod import;
pub mod init;
pub mod install;
pub mod link;
pub mod links;
pub mod list;
pub mod lock;
pub mod login;
pub mod remove;
pub mod reset;
pub mod resume_hook;
pub mod run;
pub mod session;
pub mod sessions;
pub mod status;
pub mod statusline;
pub mod unlink;
pub mod update;
pub mod usage;
pub mod vscode;
pub mod whoami;

use crate::claude_process::{ClaudeProcess, LaunchError};
use crate::config::AppConfig;
use crate::i18n::{I18n, Msg};
use crate::identity;
use std::path::PathBuf;

/// Run a prepared `claude` invocation and return its exit code.
///
/// Neither failure here is a bug in this program: `claude` may simply not be
/// installed, and the explicit Windows batch-compatibility boundary may reject
/// a shell-significant argument. Native launches do not have that restriction.
fn spawn_claude(process: ClaudeProcess, i18n: &I18n) -> i32 {
    report_claude_result(process.spawn(), i18n)
}

fn report_claude_result(result: Result<i32, LaunchError>, i18n: &I18n) -> i32 {
    match result {
        Ok(code) => code,
        Err(LaunchError::NotFound) => {
            i18n.print(Msg::ClaudeNotFound);
            1
        }
        Err(LaunchError::UnsupportedArg(token)) => {
            i18n.print(Msg::ClaudeArgUnsupported(token));
            1
        }
        Err(LaunchError::Spawn { program, error })
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            i18n.print(Msg::SpawnProgramNotFound(
                program.to_string_lossy().into_owned(),
            ));
            1
        }
        Err(LaunchError::Spawn { error, .. }) => {
            i18n.print(Msg::ClaudeLaunchFailed(error.to_string()));
            1
        }
    }
}

/// `(label, cache_path)` pairs for every already-known account except the
/// one at `exclude_label` — every managed account plus the standard
/// `~/.claude` account (labeled `"~/.claude/"`). Feeds
/// `identity::find_duplicate_account`'s `known` argument, used by `add` and
/// `login` to warn when a freshly-authenticated account turns out to share
/// an identity with one that already exists.
fn known_account_cache_paths(config: &AppConfig, exclude_label: &str) -> Vec<(String, PathBuf)> {
    let mut known: Vec<(String, PathBuf)> = config
        .list_accounts()
        .unwrap_or_default()
        .into_iter()
        .filter(|acc| acc != exclude_label)
        .map(|acc| {
            let cache_path = config.account_path(&acc).join(".account-info.json");
            (acc, cache_path)
        })
        .collect();
    if exclude_label != "~/.claude/" {
        known.push((
            "~/.claude/".to_string(),
            identity::default_cache_path(&config.base_dir),
        ));
    }
    known
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i18n() -> I18n {
        I18n {
            lang: crate::i18n::Lang::En,
        }
    }

    #[test]
    fn an_unrepresentable_argument_is_reported_not_panicked_on() {
        assert_eq!(
            report_claude_result(
                Err(LaunchError::UnsupportedArg("a\"b".to_string())),
                &i18n()
            ),
            1
        );
    }

    #[test]
    fn a_program_that_will_not_start_is_reported_not_panicked_on() {
        assert_eq!(
            report_claude_result(
                Err(LaunchError::Spawn {
                    program: "no-such-binary-cc-test".into(),
                    error: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
                }),
                &i18n(),
            ),
            1
        );
    }

    #[test]
    fn the_exit_code_of_claude_is_passed_through() {
        assert_eq!(report_claude_result(Ok(3), &i18n()), 3);
    }
}
