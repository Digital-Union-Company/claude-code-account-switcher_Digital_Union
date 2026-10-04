//! The single structured launch path for every Claude process started by the
//! account manager.

use crate::environment::strip_claude_auth_env;
#[cfg(windows)]
use crate::windows_invocation::InvocationError;
use crate::windows_invocation::ResolvedClaude;
#[cfg(test)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeProfile {
    Named(PathBuf),
    Default,
}

#[derive(Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum LaunchError {
    NotFound,
    UnsupportedArg(String),
    Spawn { program: OsString, error: io::Error },
}

/// A launch plan keeps executable resolution, argv, profile selection, cwd,
/// environment policy, process creation, and exit-code handling as explicit
/// stages. No native invocation is flattened into a shell command line.
pub struct ClaudeProcess {
    argv: Vec<OsString>,
    profile: ClaudeProfile,
    cwd: PathBuf,
    #[cfg_attr(not(windows), allow(dead_code))]
    manager_bin_dir: PathBuf,
}

impl ClaudeProcess {
    pub fn new<I, S>(
        argv: I,
        profile: ClaudeProfile,
        cwd: PathBuf,
        manager_bin_dir: PathBuf,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            profile,
            cwd,
            manager_bin_dir,
        }
    }

    #[cfg(test)]
    pub fn argv(&self) -> &[OsString] {
        &self.argv
    }

    #[cfg(test)]
    pub fn profile(&self) -> &ClaudeProfile {
        &self.profile
    }

    /// Resolve Claude exactly once, then turn the plan into a configured
    /// `Command`. Windows resolution never consults cwd.
    fn prepare(&self) -> Result<Command, LaunchError> {
        #[cfg(windows)]
        let resolved = crate::windows_invocation::resolve_claude(
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("PATHEXT").as_deref(),
            Some(&self.manager_bin_dir),
        )
        .ok_or(LaunchError::NotFound)?;

        #[cfg(not(windows))]
        let resolved = ResolvedClaude::Native(PathBuf::from("claude"));

        self.command_for_resolved(resolved)
    }

    /// Spawn the prepared child and preserve every ordinary numeric exit code.
    pub fn spawn(&self) -> Result<i32, LaunchError> {
        let mut command = self.prepare()?;
        let program = command.get_program().to_os_string();
        match command.status() {
            Ok(status) => Ok(status.code().unwrap_or(1)),
            Err(error) => Err(LaunchError::Spawn { program, error }),
        }
    }

    pub(crate) fn command_for_resolved(
        &self,
        resolved: ResolvedClaude,
    ) -> Result<Command, LaunchError> {
        let mut command = match resolved {
            ResolvedClaude::Native(executable) => {
                let mut command = Command::new(executable);
                command.args(&self.argv);
                command
            }
            ResolvedClaude::Npm { node, cli } => {
                let mut command = Command::new(node);
                command.arg(cli).args(&self.argv);
                command
            }
            ResolvedClaude::Batch(script) => self.batch_command(&script)?,
        };

        match &self.profile {
            ClaudeProfile::Named(config_dir) => {
                command.env("CLAUDE_CONFIG_DIR", config_dir);
                command.env_remove("CLAUDE_ACC_RUN_DEFAULT");
            }
            ClaudeProfile::Default => {
                command.env_remove("CLAUDE_CONFIG_DIR");
                command.env("CLAUDE_ACC_RUN_DEFAULT", "1");
            }
        }
        strip_claude_auth_env(&mut command);
        command.current_dir(&self.cwd);
        Ok(command)
    }

    #[cfg(windows)]
    fn batch_command(&self, script: &Path) -> Result<Command, LaunchError> {
        use std::os::windows::process::CommandExt;

        let invocation = crate::windows_invocation::build_batch_invocation(
            script,
            &self.argv,
            std::env::var_os("ComSpec").as_deref(),
        )
        .map_err(map_invocation_error)?;
        let mut command = Command::new(invocation.command);
        for arg in invocation.args {
            command.raw_arg(arg);
        }
        Ok(command)
    }

    #[cfg(not(windows))]
    fn batch_command(&self, _script: &Path) -> Result<Command, LaunchError> {
        Err(LaunchError::NotFound)
    }
}

#[cfg(windows)]
fn map_invocation_error(error: InvocationError) -> LaunchError {
    match error {
        InvocationError::UnsupportedArg(value) => LaunchError::UnsupportedArg(value),
        InvocationError::InvalidComSpec => LaunchError::Spawn {
            program: OsString::from("cmd.exe"),
            error: io::Error::new(io::ErrorKind::NotFound, "invalid ComSpec"),
        },
    }
}

pub fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn manager_bin_dir(base_dir: &Path) -> PathBuf {
    base_dir.join("bin")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::CLAUDE_AUTH_ENV_VARS;

    fn process(args: &[&str], profile: ClaudeProfile) -> ClaudeProcess {
        ClaudeProcess::new(
            args.iter().copied(),
            profile,
            PathBuf::from("working directory with spaces"),
            PathBuf::from("manager-bin"),
        )
    }

    fn env<'a>(command: &'a Command, name: &str) -> Option<Option<&'a OsStr>> {
        command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new(name))
            .map(|(_, value)| value)
    }

    #[test]
    fn native_command_keeps_argv_structured_and_exact() {
        let launch = process(
            &["a", "b c", "--flag=value", "quote\"percent%ampersand&"],
            ClaudeProfile::Default,
        );
        let command = launch
            .command_for_resolved(ResolvedClaude::Native(PathBuf::from("claude.exe")))
            .unwrap();
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("a"),
                OsStr::new("b c"),
                OsStr::new("--flag=value"),
                OsStr::new("quote\"percent%ampersand&")
            ]
        );
    }

    #[test]
    fn npm_command_puts_cli_before_unchanged_argv() {
        let launch = process(&["auth", "login"], ClaudeProfile::Default);
        let command = launch
            .command_for_resolved(ResolvedClaude::Npm {
                node: PathBuf::from("node.exe"),
                cli: PathBuf::from("cli.js"),
            })
            .unwrap();
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![OsStr::new("cli.js"), OsStr::new("auth"), OsStr::new("login")]
        );
    }

    #[test]
    fn named_profile_is_authoritative_and_scrubbed() {
        let launch = process(
            &["--dangerously-skip-permissions"],
            ClaudeProfile::Named(PathBuf::from("account path")),
        );
        let command = launch
            .command_for_resolved(ResolvedClaude::Native(PathBuf::from("claude.exe")))
            .unwrap();
        assert_eq!(env(&command, "CLAUDE_CONFIG_DIR"), Some(Some(OsStr::new("account path"))));
        assert_eq!(env(&command, "CLAUDE_ACC_RUN_DEFAULT"), Some(None));
        for variable in CLAUDE_AUTH_ENV_VARS {
            assert_eq!(env(&command, variable), Some(None), "{variable}");
        }
    }

    #[test]
    fn default_profile_removes_config_and_sets_wrapper_marker() {
        let launch = process(&[], ClaudeProfile::Default);
        let command = launch
            .command_for_resolved(ResolvedClaude::Native(PathBuf::from("claude.exe")))
            .unwrap();
        assert_eq!(env(&command, "CLAUDE_CONFIG_DIR"), Some(None));
        assert_eq!(env(&command, "CLAUDE_ACC_RUN_DEFAULT"), Some(Some(OsStr::new("1"))));
    }
}
