//! Windows executable resolution and the compatibility boundary for unknown
//! `.cmd`/`.bat` Claude launchers.
//!
//! Resolution searches absolute PATH entries only, never the working
//! directory, and returns one concrete path. Native `.exe`/`.com` files are
//! spawned directly by `claude_process`. A recognized npm installation runs
//! its exact `node_modules/@anthropic-ai/claude-code/cli.js` through a concrete
//! Node executable. Only an unknown batch launcher reaches cmd.exe.

#![cfg_attr(not(windows), allow(dead_code))]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedClaude {
    Native(PathBuf),
    Npm { node: PathBuf, cli: PathBuf },
    Batch(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
pub enum InvocationError {
    UnsupportedArg(String),
    InvalidComSpec,
}

pub struct WindowsCommandInvocation {
    pub command: PathBuf,
    pub args: Vec<OsString>,
}

/// Resolve Claude once from trusted PATH entries. Relative and empty PATH
/// entries are ignored because Windows interprets them relative to cwd. The
/// manager-owned shim directory is excluded to prevent recursion now and when
/// the Windows shim is introduced in a later stage.
pub fn resolve_claude(
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
    excluded_dir: Option<&Path>,
) -> Option<ResolvedClaude> {
    let executable = find_executable("claude", path, pathext, excluded_dir)?;
    let extension = executable
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "exe" | "com" => Some(ResolvedClaude::Native(executable)),
        "cmd" | "bat" => recognize_npm_installation(&executable, path, excluded_dir)
            .unwrap_or(ResolvedClaude::Batch(executable)),
        _ => None,
    }
}

/// Recognize only npm's deterministic global layout beside its generated
/// `claude.cmd`: `<shim-dir>/node_modules/@anthropic-ai/claude-code/cli.js`.
/// No package paths are inferred from the shim's text.
fn recognize_npm_installation(
    shim: &Path,
    path: Option<&OsStr>,
    excluded_dir: Option<&Path>,
) -> Option<ResolvedClaude> {
    let cli = shim
        .parent()?
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code")
        .join("cli.js");
    if !cli.is_file() {
        return None;
    }

    let adjacent_node = shim.parent()?.join("node.exe");
    let node = if adjacent_node.is_file() {
        adjacent_node
    } else {
        find_native_executable("node", path, excluded_dir)?
    };
    Some(ResolvedClaude::Npm { node, cli })
}

fn find_native_executable(
    name: &str,
    path: Option<&OsStr>,
    excluded_dir: Option<&Path>,
) -> Option<PathBuf> {
    find_executable(
        name,
        path,
        Some(OsStr::new(".EXE;.COM")),
        excluded_dir,
    )
}

/// Search PATH in order without consulting cwd. Only absolute entries are
/// accepted, and the returned path is made absolute/canonical where possible.
pub fn find_executable(
    name: &str,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
    excluded_dir: Option<&Path>,
) -> Option<PathBuf> {
    let extensions: Vec<OsString> = pathext
        .unwrap_or_else(|| OsStr::new(".COM;.EXE;.BAT;.CMD"))
        .to_string_lossy()
        .split(';')
        .filter(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                ".com" | ".exe" | ".bat" | ".cmd"
            )
        })
        .map(OsString::from)
        .collect();

    for dir in path.into_iter().flat_map(std::env::split_paths) {
        if !dir.is_absolute() || same_directory(&dir, excluded_dir) {
            continue;
        }
        for extension in &extensions {
            let mut file_name = OsString::from(name);
            file_name.push(extension);
            let candidate = dir.join(file_name);
            if candidate.is_file() {
                return Some(absolute_path(candidate));
            }
        }
    }
    None
}

fn absolute_path(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

fn same_directory(candidate: &Path, excluded: Option<&Path>) -> bool {
    let Some(excluded) = excluded else {
        return false;
    };
    let candidate = fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
    let excluded = fs::canonicalize(excluded).unwrap_or_else(|_| excluded.to_path_buf());
    if cfg!(windows) {
        candidate
            .to_string_lossy()
            .eq_ignore_ascii_case(&excluded.to_string_lossy())
    } else {
        candidate == excluded
    }
}

/// Build the only cmd.exe command line retained by R1. This is exclusively for
/// unknown batch launchers. Tokens that cmd.exe cannot carry safely are
/// rejected explicitly rather than being silently changed.
pub fn build_batch_invocation(
    script: &Path,
    args: &[OsString],
    comspec: Option<&OsStr>,
) -> Result<WindowsCommandInvocation, InvocationError> {
    let comspec = comspec
        .map(PathBuf::from)
        .filter(|path| {
            path.is_absolute()
                && path.is_file()
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe"))
        })
        .ok_or(InvocationError::InvalidComSpec)?;

    let mut tokens = Vec::with_capacity(args.len() + 1);
    tokens.push(quote_safe_batch_token(script.as_os_str())?);
    for arg in args {
        tokens.push(quote_safe_batch_token(arg)?);
    }
    let command_line = tokens.join(" ");
    Ok(WindowsCommandInvocation {
        command: comspec,
        args: vec![
            OsString::from("/d"),
            OsString::from("/v:off"),
            OsString::from("/s"),
            OsString::from("/c"),
            OsString::from(format!("\"{command_line}\"")),
        ],
    })
}

fn quote_safe_batch_token(value: &OsStr) -> Result<String, InvocationError> {
    let value = value
        .to_str()
        .ok_or_else(|| InvocationError::UnsupportedArg(value.to_string_lossy().into_owned()))?;
    if value.chars().any(|c| matches!(c, '\r' | '\n' | '"')) {
        return Err(InvocationError::UnsupportedArg(value.to_string()));
    }
    let trailing_backslashes = value.chars().rev().take_while(|&c| c == '\\').count();
    let mut crt_escaped = value.to_string();
    crt_escaped.push_str(&"\\".repeat(trailing_backslashes));
    let percent_escaped = crt_escaped.replace('%', "\"^%\"");
    Ok(format!("\"{percent_escaped}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cc-r1-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn path_search_never_uses_cwd_or_relative_entries() {
        let root = scratch("no-cwd");
        let trusted = root.join("trusted");
        fs::create_dir_all(&trusted).unwrap();
        fs::write(root.join("claude.exe"), "decoy").unwrap();
        fs::write(trusted.join("claude.exe"), "intended").unwrap();
        let path = std::env::join_paths([Path::new("."), trusted.as_path()]).unwrap();
        let found = find_executable("claude", Some(&path), Some(OsStr::new(".exe")), None);
        assert_eq!(found, fs::canonicalize(trusted.join("claude.exe")).ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn manager_shim_directory_is_excluded() {
        let root = scratch("excluded");
        let shim = root.join("manager-bin");
        let real = root.join("real-bin");
        fs::create_dir_all(&shim).unwrap();
        fs::create_dir_all(&real).unwrap();
        fs::write(shim.join("claude.exe"), "shim").unwrap();
        fs::write(real.join("claude.exe"), "real").unwrap();
        let path = std::env::join_paths([shim.as_path(), real.as_path()]).unwrap();
        let found = find_executable(
            "claude",
            Some(&path),
            Some(OsStr::new(".exe")),
            Some(&shim),
        );
        assert_eq!(found, fs::canonicalize(real.join("claude.exe")).ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_extensions_resolve_for_direct_execution() {
        let root = scratch("native");
        fs::write(root.join("claude.exe"), "native").unwrap();
        let path = std::env::join_paths([root.as_path()]).unwrap();
        let resolved = resolve_claude(Some(&path), Some(OsStr::new(".exe;.cmd")), None);
        assert!(matches!(resolved, Some(ResolvedClaude::Native(_))));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deterministic_npm_layout_runs_cli_js_directly() {
        let root = scratch("npm");
        let cli = root
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("cli.js");
        fs::create_dir_all(cli.parent().unwrap()).unwrap();
        fs::write(root.join("claude.cmd"), "npm shim text is not parsed").unwrap();
        fs::write(root.join("node.exe"), "node").unwrap();
        fs::write(&cli, "cli").unwrap();
        let path = std::env::join_paths([root.as_path()]).unwrap();
        let resolved = resolve_claude(Some(&path), Some(OsStr::new(".cmd")), None);
        let Some(ResolvedClaude::Npm { node, cli: found_cli }) = resolved else {
            panic!("npm layout was not recognized");
        };
        assert_eq!(fs::canonicalize(node).unwrap(), fs::canonicalize(root.join("node.exe")).unwrap());
        assert_eq!(fs::canonicalize(found_cli).unwrap(), fs::canonicalize(cli).unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_batch_launcher_stays_a_compatibility_fallback() {
        let root = scratch("batch");
        fs::write(root.join("claude.cmd"), "unknown").unwrap();
        let path = std::env::join_paths([root.as_path()]).unwrap();
        let resolved = resolve_claude(Some(&path), Some(OsStr::new(".cmd")), None);
        assert!(matches!(resolved, Some(ResolvedClaude::Batch(_))));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unsafe_batch_arguments_fail_explicitly() {
        for arg in ["a\"b", "a\nb", "a\rb"] {
            assert!(matches!(
                quote_safe_batch_token(OsStr::new(arg)),
                Err(InvocationError::UnsupportedArg(_))
            ));
        }
    }

    #[test]
    fn safe_batch_arguments_are_quoted_without_value_changes() {
        assert_eq!(
            quote_safe_batch_token(OsStr::new("b c")).unwrap(),
            "\"b c\""
        );
        assert_eq!(
            quote_safe_batch_token(OsStr::new("C:\\path\\")).unwrap(),
            "\"C:\\path\\\\\""
        );
        assert_eq!(
            quote_safe_batch_token(OsStr::new("50% & safe")).unwrap(),
            "\"50\"^%\" & safe\""
        );
    }
}
