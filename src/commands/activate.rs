use crate::config::AppConfig;
use crate::i18n::I18n;
use crate::resolve;
use std::path::Path;

#[derive(Clone, Copy)]
pub enum ShellSyntax {
    Posix,
    PowerShell,
}

pub fn run(config: &AppConfig, i18n: &I18n, shell: ShellSyntax) -> i32 {
    let cwd = std::env::current_dir().expect("Cannot get current directory");
    let account = match resolve::resolve_account(config, &cwd) {
        Ok(account) => account,
        Err(error) => {
            // stdout is eval'd by shell integration. Keep the diagnostic on
            // stderr and emit only a safe removal action on stdout.
            eprintln!("{}", resolve::error_message(i18n, &error));
            print_default_action(shell);
            return 1;
        }
    };

    match account.as_deref() {
        Some("default") | None => print_default_action(shell),
        Some(name) => {
            let path = config.account_path(name);
            if path.is_dir() {
                match shell {
                    ShellSyntax::Posix => println!("export CLAUDE_CONFIG_DIR='{}'", path.display()),
                    ShellSyntax::PowerShell => println!("{}", powershell_assignment(&path)),
                }
            } else {
                print_default_action(shell);
            }
        }
    }
    0
}

fn print_default_action(shell: ShellSyntax) {
    match shell {
        ShellSyntax::Posix => println!("unset CLAUDE_CONFIG_DIR"),
        ShellSyntax::PowerShell => {
            println!("Remove-Item Env:\\CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue")
        }
    }
}

fn powershell_assignment(path: &Path) -> String {
    format!(
        "$env:CLAUDE_CONFIG_DIR = {}",
        crate::powershell::single_quoted_literal(path.to_string_lossy().as_ref())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_activation_quotes_apostrophe_in_account_path() {
        assert_eq!(
            powershell_assignment(Path::new(r"C:\Users\O'Brien\accounts\work")),
            r"$env:CLAUDE_CONFIG_DIR = 'C:\Users\O''Brien\accounts\work'"
        );
    }
}
