pub fn run(shell: &str) {
    let bin = std::env::current_exe()
        .expect("Cannot determine binary path")
        .to_str()
        .expect("Invalid binary path")
        .to_string();

    let template = match shell {
        "zsh" => include_str!("../../shell/init.zsh"),
        "bash" => include_str!("../../shell/init.bash"),
        "pwsh" | "powershell" => include_str!("../../shell/init.ps1"),
        other => {
            eprintln!("Unsupported shell: {}. Use: zsh, bash, pwsh", other);
            std::process::exit(1);
        }
    };

    let rendered = match shell {
        // The template already places every placeholder inside single quotes.
        // Replace the whole literal so apostrophes in the path are doubled
        // exactly once.
        "pwsh" | "powershell" => template.replace(
            "'__CLAUDE_ACC_BIN__'",
            &crate::powershell::single_quoted_literal(&bin),
        ),
        _ => template.replace("__CLAUDE_ACC_BIN__", &bin),
    };
    print!("{}", rendered);
}

#[cfg(test)]
mod tests {
    #[test]
    fn powershell_template_replacement_is_safe_for_apostrophe_paths() {
        let template = "& '__CLAUDE_ACC_BIN__' activate --shell powershell";
        let path = r"C:\Users\O'Brien\claude-acc.exe";
        let rendered = template.replace(
            "'__CLAUDE_ACC_BIN__'",
            &crate::powershell::single_quoted_literal(path),
        );
        assert_eq!(
            rendered,
            r"& 'C:\Users\O''Brien\claude-acc.exe' activate --shell powershell"
        );
    }
}
