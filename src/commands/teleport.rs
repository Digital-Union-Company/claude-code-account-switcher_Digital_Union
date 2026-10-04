use crate::claude_process::{ClaudeProcess, ClaudeProfile, current_dir, manager_bin_dir};
use crate::config::AppConfig;
use crate::i18n::{I18n, Msg};

fn build_process(config: &AppConfig, session: &str, profile: ClaudeProfile) -> ClaudeProcess {
    ClaudeProcess::new(
        ["--teleport", session],
        profile,
        current_dir(),
        manager_bin_dir(&config.base_dir),
    )
}

fn session_is_nonempty(session: &str) -> bool {
    !session.trim().is_empty()
}

pub fn run(config: &AppConfig, i18n: &I18n, name: &str, session: &str) -> i32 {
    if !session_is_nonempty(session) {
        i18n.print(Msg::TeleportSessionEmpty);
        return 1;
    }
    let profile = match super::profile_for_account(config, name) {
        Ok(profile) => profile,
        Err(message) => {
            i18n.print(message);
            return 1;
        }
    };
    super::spawn_claude(build_process(config, session, profile), i18n)
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
    fn named_teleport_forwards_opaque_session_exactly() {
        let session = "session_Δ\"50%& exact";
        let process = build_process(
            &config(),
            session,
            ClaudeProfile::Named(PathBuf::from("account path")),
        );
        assert_eq!(
            process.profile(),
            &ClaudeProfile::Named(PathBuf::from("account path"))
        );
        assert_eq!(
            process.argv(),
            [OsStr::new("--teleport"), OsStr::new(session)]
        );
    }

    #[test]
    fn default_teleport_uses_default_profile() {
        let process = build_process(&config(), "session_123", ClaudeProfile::Default);
        assert_eq!(process.profile(), &ClaudeProfile::Default);
    }

    #[test]
    fn empty_session_is_rejected_before_spawn() {
        assert!(!session_is_nonempty(""));
        assert!(!session_is_nonempty(" \t\r\n"));
        assert!(session_is_nonempty(" session_opaque "));
    }
}
