//! Small helpers for emitting PowerShell source without treating paths as
//! unescaped source text.

/// Quote one value as a PowerShell single-quoted string literal.
///
/// PowerShell represents a literal apostrophe inside a single-quoted string
/// with two apostrophes. Windows permits apostrophes in every path component,
/// so generated shell integration must apply this before embedding a path.
pub fn single_quoted_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_quoted_literal_doubles_apostrophes() {
        assert_eq!(
            single_quoted_literal(r"C:\Users\O'Brien\claude-acc.exe"),
            r"'C:\Users\O''Brien\claude-acc.exe'"
        );
    }

    #[test]
    fn single_quoted_literal_keeps_other_powershell_metacharacters_literal() {
        assert_eq!(
            single_quoted_literal(r"C:\a $HOME & (b)\tool.exe"),
            r"'C:\a $HOME & (b)\tool.exe'"
        );
    }
}
