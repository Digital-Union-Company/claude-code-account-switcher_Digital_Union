// IDE integration: wrapper script + per-account `ide/` symlinks.
//
// Problem: IDEs (PhpStorm, IntelliJ, VSCode) launch the `claude` binary
// without sourcing the user's shell config, so `CLAUDE_CONFIG_DIR` would
// not be set and the wrong account would be used. Additionally Claude
// Code writes IDE lock files to `$CLAUDE_CONFIG_DIR/ide/`, but IDE
// plugins always look in `~/.claude/ide/`.
//
// Fix:
// 1. Install a PATH wrapper under `~/.claude-switch/bin`: the shell script
//    `claude` on Unix, or the native manager entry point `claude.exe` on
//    Windows. Shell init prepends this bin dir to PATH.
// 2. On Unix, symlink `~/.claude-switch/accounts/<name>/ide → ~/.claude/ide`
//    for every account so both sides agree on lock file location. Windows
//    lock-directory sharing remains intentionally deferred.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::AppConfig;

#[cfg(not(windows))]
const WRAPPER_TEMPLATE: &str = include_str!("../shell/claude-wrapper.sh");
#[cfg(not(windows))]
const WRAPPER_PLACEHOLDER: &str = "__CLAUDE_ACC_BIN__";

/// `~/.claude/ide` — the canonical IDE lock-file directory.
pub fn shared_ide_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude/ide"))
}

/// Ensure `acc_dir/ide` is a symlink to `~/.claude/ide`. If it's a real
/// (possibly non-empty) directory, leave it alone — caller can decide
/// whether to migrate. Returns Ok if the link is in place after the call.
pub fn ensure_account_symlink(acc_dir: &Path) -> io::Result<()> {
    let Some(target) = shared_ide_dir() else {
        return Ok(());
    };
    fs::create_dir_all(&target)?;
    let link = acc_dir.join("ide");

    match fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(()),
        Ok(meta) if meta.is_dir() => {
            // Real directory: only auto-migrate if it's empty (just stale
            // lock files would normally be in there, but be conservative).
            let empty = fs::read_dir(&link)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false);
            if empty {
                fs::remove_dir(&link)?;
                symlink(&target, &link)
            } else {
                Ok(())
            }
        }
        Ok(_) => {
            fs::remove_file(&link)?;
            symlink(&target, &link)
        }
        Err(_) => symlink(&target, &link),
    }
}

/// Refresh `ide/` symlinks for all existing accounts. Used by `install`.
pub fn refresh_all_account_symlinks(config: &AppConfig) -> io::Result<()> {
    for acc in config.list_accounts()? {
        let acc_dir = config.account_path(&acc);
        ensure_account_symlink(&acc_dir).ok();
    }
    Ok(())
}

/// Write/update `~/.claude-switch/bin/claude` wrapper. Always overwrites
/// — the cost is one fs::write per `install` call. Returns the wrapper
/// path. Windows installs the native entry point in its cfg-specific function
/// below rather than trying to execute this shell template.
#[cfg(not(windows))]
pub fn install_wrapper(
    config: &AppConfig,
    claude_acc_bin: &Path,
) -> io::Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = config.base_dir.join("bin");
    fs::create_dir_all(&bin_dir)?;
    let wrapper = bin_dir.join("claude");
    let bin_str = claude_acc_bin.to_string_lossy();
    let content = WRAPPER_TEMPLATE.replace(WRAPPER_PLACEHOLDER, &bin_str);
    fs::write(&wrapper, content)?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
    Ok(wrapper)
}

/// Native Windows uses an `.exe`; Unix keeps the existing shell wrapper name.
pub fn wrapper_path(config: &AppConfig) -> PathBuf {
    config.base_dir.join("bin").join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    })
}

#[cfg(windows)]
pub fn install_wrapper(
    config: &AppConfig,
    claude_acc_bin: &Path,
) -> io::Result<std::path::PathBuf> {
    use std::fs::File;
    use std::io::{BufReader, Read};

    fn equal_bytes(left: &Path, right: &Path) -> io::Result<bool> {
        let left_meta = match fs::metadata(left) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let right_meta = fs::metadata(right)?;
        if !left_meta.is_file() || !right_meta.is_file() || left_meta.len() != right_meta.len() {
            return Ok(false);
        }

        let mut left = BufReader::new(File::open(left)?);
        let mut right = BufReader::new(File::open(right)?);
        let mut left_buf = [0_u8; 64 * 1024];
        let mut right_buf = [0_u8; 64 * 1024];
        loop {
            let left_len = left.read(&mut left_buf)?;
            let right_len = right.read(&mut right_buf)?;
            if left_len != right_len || left_buf[..left_len] != right_buf[..right_len] {
                return Ok(false);
            }
            if left_len == 0 {
                return Ok(true);
            }
        }
    }

    let wrapper = wrapper_path(config);
    let bin_dir = wrapper.parent().expect("wrapper always has a parent");
    fs::create_dir_all(bin_dir)?;
    if equal_bytes(&wrapper, claude_acc_bin)? {
        return Ok(wrapper);
    }

    // Build the complete replacement beside the destination before touching
    // the live shim. A locked/running shim fails at the first rename and is
    // left byte-for-byte intact.
    let staged = bin_dir.join("claude.exe.new");
    let backup = bin_dir.join("claude.exe.old");
    let _ = fs::remove_file(&staged);
    fs::copy(claude_acc_bin, &staged)?;

    let result = (|| -> io::Result<()> {
        if wrapper.exists() {
            if backup.exists() {
                fs::remove_file(&backup)?;
            }
            fs::rename(&wrapper, &backup)?;
            match fs::rename(&staged, &wrapper) {
                Ok(()) => {
                    let _ = fs::remove_file(&backup);
                    Ok(())
                }
                Err(error) => {
                    let rollback = fs::rename(&backup, &wrapper);
                    match rollback {
                        Ok(()) => Err(error),
                        Err(rollback_error) => Err(io::Error::new(
                            rollback_error.kind(),
                            format!(
                                "replacement failed ({error}); restoring the previous shim also failed ({rollback_error})"
                            ),
                        )),
                    }
                }
            }
        } else {
            fs::rename(&staged, &wrapper)
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result.map(|()| wrapper)
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    // Windows symlinks need elevated privileges by default. Native PATH
    // routing does not change that lock-directory constraint; R3A keeps it
    // deferred and skips silently as before.
    Ok(())
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    fn scratch(tag: &str) -> (PathBuf, AppConfig) {
        let root =
            std::env::temp_dir().join(format!("cc-shim-install-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let config = AppConfig {
            base_dir: root.join("manager"),
        };
        (root, config)
    }

    #[test]
    fn native_shim_install_is_idempotent_and_replaces_different_bytes() {
        let (root, config) = scratch("replace");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("claude-acc.exe");
        fs::write(&source, b"first manager binary").unwrap();

        let wrapper = install_wrapper(&config, &source).unwrap();
        assert_eq!(wrapper, config.base_dir.join("bin/claude.exe"));
        assert_eq!(fs::read(&wrapper).unwrap(), b"first manager binary");
        install_wrapper(&config, &source).unwrap();
        assert_eq!(fs::read(&wrapper).unwrap(), b"first manager binary");

        fs::write(&source, b"second manager binary").unwrap();
        install_wrapper(&config, &source).unwrap();
        assert_eq!(fs::read(&wrapper).unwrap(), b"second manager binary");
        assert!(!wrapper.with_file_name("claude.exe.new").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn locked_native_shim_reports_failure_without_changing_it() {
        let (root, config) = scratch("locked");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("claude-acc.exe");
        fs::write(&source, b"old manager binary").unwrap();
        let wrapper = install_wrapper(&config, &source).unwrap();
        fs::write(&source, b"new manager binary").unwrap();

        let _lock = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&wrapper)
            .unwrap();
        assert!(install_wrapper(&config, &source).is_err());
        assert_eq!(fs::read(&wrapper).unwrap(), b"old manager binary");
        drop(_lock);
        let _ = fs::remove_dir_all(root);
    }
}
