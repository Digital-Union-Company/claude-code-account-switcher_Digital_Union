//! Native Windows `claude.exe` entry-point dispatch and account routing.
//!
//! The shim makes only routing decisions. `ClaudeProcess` remains the single
//! executable resolver, environment boundary, spawn path, and exit-code path.

#[cfg(windows)]
use crate::claude_process::{ClaudeProcess, LaunchError};
#[cfg(any(windows, test))]
use crate::claude_process::ClaudeProfile;
#[cfg(any(windows, test))]
use crate::config::AppConfig;
#[cfg(any(windows, test))]
use std::ffi::OsStr;
#[cfg(windows)]
use std::ffi::OsString;
#[cfg(any(windows, test))]
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(any(windows, test))]
pub(crate) enum InvocationMode {
    Manager,
    Shim,
}

/// Decide entry-point mode solely from the executable basename. Arguments do
/// not participate, so a manager command can never accidentally become a
/// shim invocation.
#[cfg(any(windows, test))]
pub(crate) fn invocation_mode(executable: &Path) -> InvocationMode {
    match executable.file_name().and_then(OsStr::to_str) {
        Some(name) if name.eq_ignore_ascii_case("claude.exe") => InvocationMode::Shim,
        _ => InvocationMode::Manager,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(windows, test))]
struct ShimTarget {
    profile: ClaudeProfile,
    label: String,
    resume_dir: Option<PathBuf>,
}

/// Apply the existing cwd resolver and the same missing-account fallback as
/// shell activation: a valid named account selects its managed directory;
/// `default`, no mapping, or a stale mapping selects upstream default state.
#[cfg(any(windows, test))]
fn target_for_cwd(config: &AppConfig, cwd: &Path) -> ShimTarget {
    match crate::resolve::resolve_account(config, cwd) {
        Some(name) if name != crate::sessions::DEFAULT_LABEL && config.account_exists(&name) => {
            let dir = config.account_path(&name);
            ShimTarget {
                profile: ClaudeProfile::Named(dir.clone()),
                label: name,
                resume_dir: Some(dir),
            }
        }
        _ => ShimTarget {
            profile: ClaudeProfile::Default,
            label: crate::sessions::DEFAULT_LABEL.to_string(),
            resume_dir: crate::identity::standard_token_dir(),
        },
    }
}

/// Whether the forwarded argv contains a concrete resume id/name. A bare
/// `--resume` is Claude's picker and deliberately does not qualify.
#[cfg(any(windows, test))]
fn has_concrete_resume(args: &[String]) -> bool {
    for (index, arg) in args.iter().enumerate() {
        if let Some(value) = arg.strip_prefix("--resume=") {
            return !value.is_empty();
        }
        if arg == "--resume" || arg == "-r" {
            return args
                .get(index + 1)
                .is_some_and(|value| !value.starts_with('-'));
        }
    }
    false
}

/// Keep prompts away from scripts and pipes. `hook_enabled` already combines
/// the persisted `resume_hook` setting with `CLAUDE_ACC_NO_RESUME_HOOK`.
#[cfg(any(windows, test))]
fn should_run_preflight(
    args: &[String],
    hook_enabled: bool,
    stdin_terminal: bool,
    stdout_terminal: bool,
) -> bool {
    hook_enabled && stdin_terminal && stdout_terminal && has_concrete_resume(args)
}

#[cfg(windows)]
pub(crate) fn run() -> i32 {
    use std::io::{IsTerminal, stdin, stdout};

    let config = AppConfig::new();
    config
        .init()
        .expect("Failed to initialize config directory");
    let i18n = crate::i18n::I18n::new();
    let cwd = crate::claude_process::current_dir();
    let target = target_for_cwd(&config, &cwd);
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    // Windows command-line arguments are Unicode, but retain the original
    // OsStrings for launch. This view exists only so the existing resume
    // preflight can inspect flags without owning the process boundary.
    let resume_args: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if should_run_preflight(
        &resume_args,
        crate::commands::session::hook_enabled(&config),
        stdin().is_terminal(),
        stdout().is_terminal(),
    ) && let Some(dir) = target.resume_dir.as_deref()
    {
        crate::commands::session::preflight_resume(
            &config,
            &i18n,
            &resume_args,
            &target.label,
            dir,
        );
    }

    let process = ClaudeProcess::new(
        args,
        target.profile,
        cwd,
        crate::claude_process::manager_bin_dir(&config.base_dir),
    );
    report_launch(process.spawn(), &i18n)
}

#[cfg(windows)]
fn report_launch(result: Result<i32, LaunchError>, i18n: &crate::i18n::I18n) -> i32 {
    use crate::i18n::Msg;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> (PathBuf, AppConfig) {
        let root = std::env::temp_dir().join(format!(
            "cc-windows-shim-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let config = AppConfig {
            base_dir: root.join("manager"),
        };
        config.init().unwrap();
        (root, config)
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn executable_basename_selects_only_native_claude_shim() {
        assert_eq!(
            invocation_mode(Path::new("claude.exe")),
            InvocationMode::Shim
        );
        assert_eq!(
            invocation_mode(Path::new("CLAUDE.EXE")),
            InvocationMode::Shim
        );
        assert_eq!(
            invocation_mode(Path::new("claude-acc.exe")),
            InvocationMode::Manager
        );
        assert_eq!(
            invocation_mode(Path::new("other.exe")),
            InvocationMode::Manager
        );
    }

    #[test]
    fn linked_named_account_builds_the_managed_profile_and_resume_target() {
        let (root, config) = scratch("named");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(config.account_path("work")).unwrap();
        config.set_link(repo.to_str().unwrap(), "work").unwrap();

        let target = target_for_cwd(&config, &repo.join("nested"));
        assert_eq!(
            target.profile,
            ClaudeProfile::Named(config.account_path("work"))
        );
        assert_eq!(target.label, "work");
        assert_eq!(target.resume_dir, Some(config.account_path("work")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn default_unlinked_and_stale_mapping_use_default_profile() {
        let (root, config) = scratch("defaults");
        let explicit = root.join("explicit");
        let stale = root.join("stale");
        let unlinked = root.join("unlinked");
        for dir in [&explicit, &stale, &unlinked] {
            fs::create_dir_all(dir).unwrap();
        }
        config
            .set_link(explicit.to_str().unwrap(), crate::sessions::DEFAULT_LABEL)
            .unwrap();
        config
            .set_link(stale.to_str().unwrap(), "missing-account")
            .unwrap();

        for dir in [&explicit, &stale, &unlinked] {
            let target = target_for_cwd(&config, dir);
            assert_eq!(target.profile, ClaudeProfile::Default, "{dir:?}");
            assert_eq!(target.label, crate::sessions::DEFAULT_LABEL);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn configured_named_default_is_selected_for_an_unlinked_directory() {
        let (root, config) = scratch("configured-default");
        let unlinked = root.join("unlinked");
        fs::create_dir_all(&unlinked).unwrap();
        fs::create_dir_all(config.account_path("work")).unwrap();
        config.set_default("work").unwrap();

        let target = target_for_cwd(&config, &unlinked);
        assert_eq!(
            target.profile,
            ClaudeProfile::Named(config.account_path("work"))
        );
        assert_eq!(target.label, "work");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concrete_resume_forms_are_recognized_but_bare_picker_is_not() {
        assert!(has_concrete_resume(&strings(&["--resume", "abc"])));
        assert!(has_concrete_resume(&strings(&["--resume=abc"])));
        assert!(has_concrete_resume(&strings(&["-r", "abc"])));
        assert!(!has_concrete_resume(&strings(&["--resume"])));
        assert!(!has_concrete_resume(&strings(&["--resume", "--verbose"])));
        assert!(!has_concrete_resume(&strings(&["--resume="])));
    }

    #[test]
    fn preflight_requires_terminals_hook_and_concrete_resume() {
        let resume = strings(&["--resume", "abc"]);
        assert!(should_run_preflight(&resume, true, true, true));
        assert!(!should_run_preflight(&resume, true, false, true));
        assert!(!should_run_preflight(&resume, true, true, false));
        assert!(!should_run_preflight(&resume, false, true, true));
        assert!(!should_run_preflight(
            &strings(&["--resume"]),
            true,
            true,
            true
        ));
    }

    #[test]
    fn disabled_hook_value_covers_persisted_and_environment_opt_outs() {
        // Both `resume_hook=off` and CLAUDE_ACC_NO_RESUME_HOOK make the
        // existing hook_enabled helper return false; neither may prompt.
        let resume = strings(&["-r", "abc"]);
        assert!(!should_run_preflight(&resume, false, true, true));
    }
}
