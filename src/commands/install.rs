use crate::config::AppConfig;
use crate::i18n::{I18n, Msg};
use crate::ide;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Filename the installed binary lives under inside `~/.claude-switch/bin/`.
/// Windows requires the `.exe` extension or the OS won't execute the file
/// even when the path is given explicitly.
pub(crate) fn binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "claude-acc.exe"
    } else {
        "claude-acc"
    }
}

pub fn run(config: &AppConfig, i18n: &I18n) {
    let bin_dir = config.base_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("Failed to create bin directory");

    let target = bin_dir.join(binary_name());
    let source = std::env::current_exe().expect("Cannot determine binary path");

    // On Windows, prior versions copied the binary as `claude-acc` (no
    // extension) which Windows can't execute. Clean that up so PATH-based
    // lookups stop hitting the broken file.
    if cfg!(target_os = "windows") {
        let stale = bin_dir.join("claude-acc");
        if stale.is_file() && stale != target {
            let _ = fs::remove_file(&stale);
        }
    }

    // Check version
    let current_version = env!("CARGO_PKG_VERSION");

    if target.exists() {
        // Run the installed binary with --version to get its version
        let output = std::process::Command::new(&target)
            .arg("--version")
            .output();

        if let Ok(output) = output {
            let installed_version = String::from_utf8_lossy(&output.stdout);
            // Format: "claude-acc X.Y.Z\n"
            let installed_version = installed_version.trim().replace("claude-acc ", "");
            if installed_version == current_version {
                i18n.print(Msg::InstallUpToDate(current_version.to_string()));
                ensure_ide_integration(config, &target, i18n);
                ensure_shell_integration(config, i18n);
                return;
            }
            i18n.print(Msg::InstallUpdating(
                installed_version.clone(),
                current_version.to_string(),
            ));
        }
    } else {
        i18n.print(Msg::InstallCopying(current_version.to_string()));
    }

    // Copy to a temp file in the same directory and rename over `target`
    // rather than overwriting it in place. In-place overwrite of an
    // existing executable can leave macOS's cached code-signing validation
    // for that inode stale, killing the next launch with SIGKILL; a rename
    // gives the replacement a fresh inode and is atomic on the same filesystem.
    let tmp = bin_dir.join(format!("{}.new", binary_name()));
    fs::copy(&source, &tmp).expect("Failed to copy binary");

    // Make executable on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).ok();
    }

    fs::rename(&tmp, &target).expect("Failed to install binary");

    i18n.print(Msg::InstallDone(target.to_str().unwrap_or("").to_string()));

    ensure_ide_integration(config, &target, i18n);
    ensure_shell_integration(config, i18n);
}

fn ensure_ide_integration(config: &AppConfig, claude_acc_bin: &Path, i18n: &I18n) {
    // The manager binary is already usable when this runs, but a failed shim
    // must be visible: silently leaving an older claude.exe on PATH would run
    // different routing code than the installed manager.
    if let Err(error) = ide::install_wrapper(config, claude_acc_bin) {
        i18n.print(Msg::InstallWrapperFailed(error.to_string()));
    }
    let _ = ide::refresh_all_account_symlinks(config);
    refresh_vscode_wrapper(config, claude_acc_bin);
}

/// The VS Code launcher embeds the path to this binary, exactly as the PATH
/// wrapper does, so `install` has to refresh it for the same reason `update`
/// does: a stale one points at wherever `claude-acc` used to be, and every
/// launch then falls back to the standard account with nothing to say why.
///
/// Only refreshes one that is already there — `install` is not the moment to
/// start writing into someone's editor config; that is `vscode install`.
#[cfg(not(windows))]
fn refresh_vscode_wrapper(config: &AppConfig, claude_acc_bin: &Path) {
    if crate::vscode::wrapper_path(&config.base_dir).exists() {
        let _ = crate::vscode::install_wrapper(&config.base_dir, claude_acc_bin);
    }
}

#[cfg(windows)]
fn refresh_vscode_wrapper(_config: &AppConfig, _claude_acc_bin: &Path) {}

fn ensure_shell_integration(config: &AppConfig, i18n: &I18n) {
    let bin_path = config.base_dir.join("bin").join(binary_name());
    let bin_str = bin_path.to_str().expect("Invalid bin path");

    let (shell, rc_path) = detect_shell_and_rc();

    let eval_line = match shell.as_str() {
        // PowerShell collects child-process output as `string[]` (one
        // element per line). Invoke-Expression only evaluates a scalar,
        // so without the `-join "`n"` it silently runs only the first
        // line of `init pwsh` (a comment) and the integration is dead.
        "pwsh" | "powershell" => format!(
            "Invoke-Expression ((& {} init pwsh) -join \"`n\")",
            crate::powershell::single_quoted_literal(bin_str)
        ),
        _ => format!("eval \"$('{0}' init {1})\"", bin_str, shell),
    };

    if let Some(rc) = rc_path {
        match update_shell_profile(&rc, &eval_line) {
            Ok(ShellProfileChange::Added) => {
                i18n.print(Msg::InstallShellAdded(rc.to_string_lossy().to_string()))
            }
            Ok(ShellProfileChange::AlreadyCurrent) => {
                i18n.print(Msg::InstallShellAlready(rc.to_string_lossy().to_string()))
            }
            Ok(ShellProfileChange::Updated) => {
                i18n.print(Msg::InstallShellUpdated(rc.to_string_lossy().to_string()))
            }
            Err(error) => i18n.print(Msg::InstallShellFailed(
                rc.to_string_lossy().to_string(),
                error.to_string(),
                eval_line.clone(),
            )),
        }
    } else {
        i18n.print(Msg::InstallShellManual(eval_line));
    }

    // The VS Code extension's native UI doesn't go through PATH, so the
    // wrapper above doesn't reach it. Say so — but don't write into
    // someone's editor config unasked; that's `vscode install`.
    super::vscode::print_install_hint(config, i18n);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellProfileChange {
    Added,
    AlreadyCurrent,
    Updated,
}

/// Update one shell profile without ever conflating "not found" with "could
/// not read". In particular, InvalidData from a UTF-16/non-UTF-8 PowerShell
/// profile is returned before any write occurs.
fn update_shell_profile(rc: &Path, eval_line: &str) -> std::io::Result<ShellProfileChange> {
    let content = match fs::read_to_string(rc) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };

    let init_line_count = content
        .lines()
        .filter(|line| is_claude_acc_init_line(line))
        .count();
    let has_exact_match = content
        .lines()
        .any(|line| line.trim() == eval_line.trim());
    if init_line_count == 1 && has_exact_match {
        return Ok(ShellProfileChange::AlreadyCurrent);
    }

    let (updated, change) = if init_line_count == 0 {
        let mut updated = content;
        if !updated.ends_with('\n') && !updated.is_empty() {
            updated.push('\n');
        }
        updated.push_str(&format!(
            "\n# Claude Code Account Switcher\n{}\n",
            eval_line
        ));
        (updated, ShellProfileChange::Added)
    } else {
        (
            update_eval_line(&content, eval_line),
            ShellProfileChange::Updated,
        )
    };

    if let Some(parent) = rc.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(rc, updated)?;
    Ok(change)
}

fn detect_shell_and_rc() -> (String, Option<PathBuf>) {
    if cfg!(target_os = "windows") {
        return detect_shell_windows();
    }
    detect_shell_unix()
}

fn detect_shell_unix() -> (String, Option<PathBuf>) {
    let home = dirs::home_dir().expect("Cannot determine home directory");

    let shell_env = std::env::var("SHELL").unwrap_or_default();
    if shell_env.contains("zsh") {
        return ("zsh".to_string(), Some(home.join(".zshrc")));
    }
    if shell_env.contains("bash") {
        let bashrc = home.join(".bashrc");
        let profile = home.join(".bash_profile");
        let rc = if bashrc.exists() { bashrc } else { profile };
        return ("bash".to_string(), Some(rc));
    }

    // Fallback: check common rc files
    if home.join(".zshrc").exists() {
        return ("zsh".to_string(), Some(home.join(".zshrc")));
    }
    if home.join(".bashrc").exists() {
        return ("bash".to_string(), Some(home.join(".bashrc")));
    }

    ("bash".to_string(), None)
}

/// Windows: PowerShell is the only target shell we support. Git Bash etc.
/// would appear with a unix-style $SHELL but on Windows we prefer pwsh,
/// because that's where IDEs and the standard terminal land.
///
/// Resolution order for the profile path:
///   1. `pwsh -NoProfile -Command "$PROFILE"` (PowerShell 7+)
///   2. `powershell -NoProfile -Command "$PROFILE"` (Windows PowerShell 5.x)
///   3. Hardcoded `~/Documents/PowerShell/Microsoft.PowerShell_profile.ps1`
///
/// The `$PROFILE` automatic variable in PowerShell is *not* exported to
/// child processes by default, so we can't read it via `std::env::var`.
fn detect_shell_windows() -> (String, Option<PathBuf>) {
    for shell_bin in ["pwsh", "powershell"] {
        if let Some(p) = pwsh_profile_path(shell_bin) {
            return ("pwsh".to_string(), Some(p));
        }
    }
    // Last-resort fallback: standard PSCore profile location, even if pwsh
    // isn't on PATH. The user can always source it manually.
    let home = dirs::home_dir().expect("Cannot determine home directory");
    let fallback = home
        .join("Documents")
        .join("PowerShell")
        .join("Microsoft.PowerShell_profile.ps1");
    ("pwsh".to_string(), Some(fallback))
}

fn pwsh_profile_path(shell_bin: &str) -> Option<PathBuf> {
    let out = Command::new(shell_bin)
        .args(["-NoProfile", "-NonInteractive", "-Command", "$PROFILE"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

fn is_claude_acc_init_line(line: &str) -> bool {
    line.contains("claude-acc")
        && line.contains("init")
        && (line.contains("eval") || line.contains("Invoke-Expression"))
}

const HEADER_COMMENT: &str = "# Claude Code Account Switcher";

fn update_eval_line(content: &str, new_eval: &str) -> String {
    // Replace the first matching init line; drop any subsequent duplicates
    // and their accompanying header comment (older versions of `install`
    // would append rather than dedupe).
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in content.lines() {
        if is_claude_acc_init_line(line) {
            if !replaced {
                out.push(new_eval.to_string());
                replaced = true;
            } else if out.last().map(|s| s.trim()) == Some(HEADER_COMMENT) {
                // Duplicate: drop both this eval line and its preceding
                // header comment so we don't leave an orphan.
                out.pop();
            }
        } else {
            out.push(line.to_string());
        }
    }
    let mut result = out.join("\n");
    result.push('\n');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "windows")]
    fn binary_name_has_exe_on_windows() {
        assert_eq!(binary_name(), "claude-acc.exe");
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn binary_name_has_no_extension_elsewhere() {
        assert_eq!(binary_name(), "claude-acc");
    }

    #[test]
    fn detects_quoted_path_eval_line() {
        assert!(is_claude_acc_init_line(
            r#"eval "$('/Users/me/.claude-switch/bin/claude-acc' init zsh)""#
        ));
    }

    #[test]
    fn detects_powershell_invocation() {
        // Current shape (with -join "`n").
        assert!(is_claude_acc_init_line(
            "Invoke-Expression ((& 'C:\\Users\\me\\.claude-switch\\bin\\claude-acc' init pwsh) -join \"`n\")"
        ));
        // Pre-0.6.1 shape — must still match so re-running `install`
        // upgrades existing users to the joined variant.
        assert!(is_claude_acc_init_line(
            "Invoke-Expression (& 'C:\\Users\\me\\.claude-switch\\bin\\claude-acc' init pwsh)"
        ));
    }

    #[test]
    fn powershell_install_line_quotes_apostrophe_in_binary_path() {
        let literal = crate::powershell::single_quoted_literal(
            r"C:\Users\O'Brien\.claude-switch\bin\claude-acc.exe",
        );
        let line = format!(
            "Invoke-Expression ((& {} init pwsh) -join \"`n\")",
            literal
        );
        assert_eq!(
            line,
            r#"Invoke-Expression ((& 'C:\Users\O''Brien\.claude-switch\bin\claude-acc.exe' init pwsh) -join "`n")"#
        );
    }

    #[test]
    fn invalid_utf8_profile_is_never_replaced() {
        let root = std::env::temp_dir().join(format!(
            "cc-install-invalid-profile-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let profile = root.join("Microsoft.PowerShell_profile.ps1");
        let original = vec![0xff, 0xfe, 0x00, 0x61, 0x00];
        fs::write(&profile, &original).unwrap();

        let result = update_shell_profile(&profile, "replacement");
        assert!(result.is_err());
        assert_eq!(fs::read(&profile).unwrap(), original);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn other_profile_read_errors_never_turn_into_a_replacement_file() {
        let root = std::env::temp_dir().join(format!(
            "cc-install-unreadable-profile-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        assert!(update_shell_profile(&root, "replacement").is_err());
        assert!(root.is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_and_existing_utf8_profiles_follow_explicit_paths() {
        let root = std::env::temp_dir().join(format!(
            "cc-install-profile-update-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let profile = root.join("nested/profile.ps1");
        let line = "Invoke-Expression ((& 'C:\\bin\\claude-acc.exe' init pwsh) -join \"`n\")";

        assert_eq!(
            update_shell_profile(&profile, line).unwrap(),
            ShellProfileChange::Added
        );
        assert_eq!(
            update_shell_profile(&profile, line).unwrap(),
            ShellProfileChange::AlreadyCurrent
        );
        assert_eq!(fs::read_to_string(&profile).unwrap().matches(line).count(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ignores_unrelated_lines() {
        assert!(!is_claude_acc_init_line("# Claude Code Account Switcher"));
        assert!(!is_claude_acc_init_line("alias claudey='claude --foo'"));
        assert!(!is_claude_acc_init_line("export PATH=/some/bin:$PATH"));
    }

    #[test]
    fn ignores_partial_match_without_eval() {
        // mentions claude-acc and "init" but not as an eval line
        assert!(!is_claude_acc_init_line(
            "# claude-acc init zsh runs on shell startup"
        ));
    }

    #[test]
    fn update_eval_line_replaces_single_match() {
        let input = "\
# unrelated
eval \"$('/old/path/claude-acc' init zsh)\"
# trailing
";
        let new_eval = "eval \"$('/new/path/claude-acc' init zsh)\"";
        let out = update_eval_line(input, new_eval);
        assert!(out.contains("/new/path/claude-acc"));
        assert!(!out.contains("/old/path/claude-acc"));
        assert!(out.contains("# unrelated"));
        assert!(out.contains("# trailing"));
    }

    #[test]
    fn update_eval_line_dedupes_multiple_matches_and_drops_orphan_headers() {
        let input = "\
# preamble

# Claude Code Account Switcher
eval \"$('/p1/claude-acc' init zsh)\"

# Claude Code Account Switcher
eval \"$('/p2/claude-acc' init zsh)\"

# Claude Code Account Switcher
eval \"$('/p3/claude-acc' init zsh)\"
";
        let new_eval = "eval \"$('/new/claude-acc' init zsh)\"";
        let out = update_eval_line(input, new_eval);

        // Exactly one eval line, exactly one header.
        assert_eq!(out.matches("eval \"$(").count(), 1, "got: {out}");
        assert_eq!(
            out.matches("# Claude Code Account Switcher").count(),
            1,
            "got: {out}"
        );
        assert!(out.contains("/new/claude-acc"));
    }
}
