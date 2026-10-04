use crate::claude_process::{ClaudeProcess, ClaudeProfile, current_dir, manager_bin_dir};
use crate::config::AppConfig;
use crate::i18n::{I18n, Msg};

fn description_is_nonempty(description: &str) -> bool {
    !description.trim().is_empty()
}

fn build_process(config: &AppConfig, description: &str, profile: ClaudeProfile) -> ClaudeProcess {
    ClaudeProcess::new(
        ["--cloud", description, "--permission-mode", "auto"],
        profile,
        current_dir(),
        manager_bin_dir(&config.base_dir),
    )
}

pub fn run(config: &AppConfig, i18n: &I18n, name: &str, description: &str) -> i32 {
    if !description_is_nonempty(description) {
        i18n.print(Msg::CloudDescriptionEmpty);
        return 1;
    }

    let profile = match super::profile_for_account(config, name) {
        Ok(profile) => profile,
        Err(message) => {
            i18n.print(message);
            return 1;
        }
    };
    super::spawn_claude(build_process(config, description, profile), i18n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;

    fn config() -> AppConfig {
        AppConfig {
            base_dir: PathBuf::from("manager root"),
        }
    }

    #[test]
    fn named_cloud_uses_exact_profile_and_load_bearing_argv_order() {
        let description = "Continue Δ with \"quotes\", 50% & exact spacing";
        let process = build_process(
            &config(),
            description,
            ClaudeProfile::Named(PathBuf::from("account path")),
        );
        assert_eq!(
            process.profile(),
            &ClaudeProfile::Named(PathBuf::from("account path"))
        );
        assert_eq!(
            process.argv(),
            [
                OsStr::new("--cloud"),
                OsStr::new(description),
                OsStr::new("--permission-mode"),
                OsStr::new("auto"),
            ]
        );
    }

    #[test]
    fn default_cloud_uses_default_profile() {
        let process = build_process(&config(), "task", ClaudeProfile::Default);
        assert_eq!(process.profile(), &ClaudeProfile::Default);
    }

    #[test]
    fn empty_and_whitespace_only_descriptions_are_rejected() {
        assert!(!description_is_nonempty(""));
        assert!(!description_is_nonempty(" \t\r\n"));
    }

    #[test]
    fn locator_like_descriptions_are_forwarded_as_new_session_tasks() {
        for description in [
            "session_012345",
            "cse_012345",
            "https://claude.ai/code/session_012345",
            "claude.ai/code/cse_012345?from=cli",
            "Investigate session_012345 handling",
        ] {
            let process = build_process(&config(), description, ClaudeProfile::Default);
            assert_eq!(
                process.argv(),
                [
                    OsStr::new("--cloud"),
                    OsStr::new(description),
                    OsStr::new("--permission-mode"),
                    OsStr::new("auto"),
                ],
                "{description}"
            );
            assert!(!process.argv().contains(&OsString::from("-p")));
        }
    }
}
