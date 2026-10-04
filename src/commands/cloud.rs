use crate::claude_process::{ClaudeProcess, ClaudeProfile, current_dir, manager_bin_dir};
use crate::config::AppConfig;
use crate::i18n::{I18n, Msg};

#[derive(Debug, PartialEq, Eq)]
enum DescriptionError {
    Empty,
    ExistingSessionLocator,
}

fn validate_description(description: &str) -> Result<(), DescriptionError> {
    let value = description.trim();
    if value.is_empty() {
        return Err(DescriptionError::Empty);
    }
    if is_existing_session_locator(value) {
        return Err(DescriptionError::ExistingSessionLocator);
    }
    Ok(())
}

fn is_existing_session_locator(value: &str) -> bool {
    if value.chars().any(char::is_whitespace) {
        return false;
    }
    if is_cloud_session_id(value) {
        return true;
    }

    let lower = value.to_ascii_lowercase();
    let without_scheme = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let Some(rest) = without_scheme.strip_prefix("claude.ai/code/") else {
        return false;
    };
    let id = rest.split(['?', '#']).next().unwrap_or("");
    is_cloud_session_id(id)
}

fn is_cloud_session_id(value: &str) -> bool {
    ["session_", "cse_"]
        .iter()
        .copied()
        .any(|prefix| {
            value
                .strip_prefix(prefix)
                .is_some_and(|rest| !rest.is_empty())
        })
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
    if let Err(error) = validate_description(description) {
        i18n.print(match error {
            DescriptionError::Empty => Msg::CloudDescriptionEmpty,
            DescriptionError::ExistingSessionLocator => Msg::CloudExistingSessionLocator,
        });
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
    use std::ffi::OsStr;
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
        assert_eq!(validate_description(""), Err(DescriptionError::Empty));
        assert_eq!(validate_description(" \t\r\n"), Err(DescriptionError::Empty));
    }

    #[test]
    fn exact_existing_session_locators_are_rejected() {
        for value in [
            "session_012345",
            "cse_012345",
            "https://claude.ai/code/session_012345",
            "claude.ai/code/cse_012345?from=cli",
        ] {
            assert_eq!(
                validate_description(value),
                Err(DescriptionError::ExistingSessionLocator),
                "{value}"
            );
        }
    }

    #[test]
    fn prose_that_mentions_session_locators_is_allowed() {
        for value in [
            "Investigate session_012345 handling",
            "Explain cse_012345 and its caller",
            "Document https://claude.ai/code/session_012345 safely",
        ] {
            assert_eq!(validate_description(value), Ok(()), "{value}");
        }
    }
}
