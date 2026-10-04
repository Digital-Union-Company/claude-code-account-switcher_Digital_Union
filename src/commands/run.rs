use crate::claude_process::{ClaudeProcess, ClaudeProfile, current_dir, manager_bin_dir};
use crate::config::{AppConfig, validate_name};
use crate::i18n::{I18n, Msg};
use crate::sessions;
use std::path::Path;

/// Build the structured Claude launch plan for `run`. The explicit default
/// marker preserves the existing IDE-wrapper contract while the central
/// launcher makes CLAUDE_CONFIG_DIR and the authentication scrub authoritative.
fn build_process(config: &AppConfig, args: &[String], acc_dir: Option<&Path>) -> ClaudeProcess {
    let profile = acc_dir
        .map(|dir| ClaudeProfile::Named(dir.to_path_buf()))
        .unwrap_or(ClaudeProfile::Default);
    ClaudeProcess::new(
        args.iter().map(String::as_str),
        profile,
        current_dir(),
        manager_bin_dir(&config.base_dir),
    )
}

pub fn run(config: &AppConfig, i18n: &I18n, name: &str, args: &[String]) {
    if name == "default" {
        let dir = crate::identity::standard_token_dir();
        if let Some(dir) = dir.as_deref() {
            super::session::preflight_resume(config, i18n, args, sessions::DEFAULT_LABEL, dir);
        }
        std::process::exit(super::spawn_claude(
            build_process(config, args, None),
            i18n,
        ));
    }

    if !validate_name(name) {
        i18n.print(Msg::NameInvalid);
        std::process::exit(1);
    }

    if !config.account_exists(name) {
        i18n.print(Msg::LoginNotFound(name.to_string()));
        std::process::exit(1);
    }

    let acc_dir = config.account_path(name);
    super::session::preflight_resume(config, i18n, args, name, &acc_dir);
    std::process::exit(super::spawn_claude(
        build_process(config, args, Some(&acc_dir)),
        i18n,
    ));
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
    fn default_account_builds_default_profile() {
        let process = build_process(&config(), &[], None);
        assert_eq!(process.profile(), &ClaudeProfile::Default);
    }

    #[test]
    fn named_account_builds_exact_config_profile() {
        let dir = Path::new("account path");
        let process = build_process(&config(), &[], Some(dir));
        assert_eq!(
            process.profile(),
            &ClaudeProfile::Named(PathBuf::from("account path"))
        );
    }

    #[test]
    fn every_claude_argument_is_preserved_in_order() {
        let args = vec![
            "a".to_string(),
            "b c".to_string(),
            "--flag=value".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "quote\"relevant%content&".to_string(),
        ];
        let process = build_process(&config(), &args, None);
        assert_eq!(
            process.argv(),
            args.iter()
                .map(|arg| OsStr::new(arg.as_str()))
                .collect::<Vec<_>>()
        );
    }
}
