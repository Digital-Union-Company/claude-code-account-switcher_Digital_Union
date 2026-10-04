use crate::config::AppConfig;
use crate::resolve;
use std::path::Path;

#[derive(Clone, Copy)]
pub enum ShellSyntax {
    Posix,
    PowerShell,
}

pub fn run(config: &AppConfig, shell: ShellSyntax) {
    let cwd = std::env::current_dir().expect("Cannot get current directory");
    let account = resolve::resolve_account(config, &cwd);

    match account.as_deref() {
        Some("default") | None => match shell {
            ShellSyntax::Posix => println!("unset CLAUDE_CONFIG_DIR"),
            ShellSyntax::PowerShell => {
                println!("Remove-Item Env:\\CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue")
            }
        },
        Some(name) => {
            let path = config.account_path(name);
            if path.is_dir() {
                match shell {
                    ShellSyntax::Posix => println!("export CLAUDE_CONFIG_DIR='{}'", path.display()),
                    ShellSyntax::PowerShell => println!("{}", powershell_assignment(&path)),
                }
            } else {
                match shell {
                    ShellSyntax::Posix => println!("unset CLAUDE_CONFIG_DIR"),
                    ShellSyntax::PowerShell => println!(
                        "Remove-Item Env:\\CLAUDE_CONFIG_DIR -ErrorAction SilentlyContinue"
                    ),
                }
            }
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
