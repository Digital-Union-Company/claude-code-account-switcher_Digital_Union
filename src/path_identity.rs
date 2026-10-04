//! Platform-aware comparison for directory links.
//!
//! Stored paths remain untouched for display. Only comparisons use these
//! keys, and non-Windows platforms deliberately retain exact string matching.

/// Compare two stored/current directory spellings according to the platform's
/// routing semantics.
pub fn equivalent(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        windows_equivalent(left, right)
    }

    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsPathKey {
    value: String,
    absolute: bool,
    locally_canonicalizable: bool,
}

#[cfg(any(windows, test))]
fn windows_lexical_key(path: &str) -> WindowsPathKey {
    let mut path = path.replace('/', "\\");
    if path
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("\\\\?\\UNC\\"))
    {
        path = format!("\\\\{}", &path[8..]);
    } else if path
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("\\\\?\\"))
    {
        path = path[4..].to_string();
    }

    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let components = normalize_components(&path[3..], true);
        return WindowsPathKey {
            value: rooted_key(&format!("drive:{drive}:"), &components),
            absolute: true,
            locally_canonicalizable: true,
        };
    }

    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let components = normalize_components(&path[2..], false);
        return WindowsPathKey {
            value: relative_key(&format!("drive-relative:{drive}:"), &components),
            absolute: false,
            locally_canonicalizable: false,
        };
    }

    if path.starts_with("\\\\") {
        let components: Vec<&str> = path
            .trim_start_matches('\\')
            .split('\\')
            .filter(|component| !component.is_empty())
            .collect();
        if components.len() >= 2 {
            let prefix = format!(
                "unc:{}\\{}",
                fold_case(components[0]),
                fold_case(components[1])
            );
            let descendants = normalize_component_iter(components.into_iter().skip(2), true);
            return WindowsPathKey {
                value: rooted_key(&prefix, &descendants),
                absolute: true,
                // Do not turn comparison of an offline UNC spelling into a
                // network operation. UNC aliases receive lexical matching.
                locally_canonicalizable: false,
            };
        }
        return WindowsPathKey {
            value: format!("unc-incomplete:{}", fold_case(&path)),
            absolute: false,
            locally_canonicalizable: false,
        };
    }

    if let Some(relative) = path.strip_prefix('\\') {
        return WindowsPathKey {
            value: rooted_key("rooted", &normalize_components(relative, true)),
            absolute: true,
            locally_canonicalizable: true,
        };
    }

    WindowsPathKey {
        value: relative_key("relative", &normalize_components(&path, false)),
        absolute: false,
        locally_canonicalizable: false,
    }
}

#[cfg(any(windows, test))]
fn normalize_components(path: &str, anchored: bool) -> Vec<String> {
    normalize_component_iter(
        path.split('\\').filter(|component| !component.is_empty()),
        anchored,
    )
}

#[cfg(any(windows, test))]
fn normalize_component_iter<'a>(
    components: impl IntoIterator<Item = &'a str>,
    anchored: bool,
) -> Vec<String> {
    let mut normalized = Vec::new();
    for component in components {
        match component {
            "." => {}
            ".." => {
                if normalized.last().is_some_and(|last| last != "..") {
                    normalized.pop();
                } else if !anchored {
                    normalized.push("..".to_string());
                }
            }
            value => normalized.push(fold_case(value)),
        }
    }
    normalized
}

#[cfg(any(windows, test))]
fn rooted_key(prefix: &str, components: &[String]) -> String {
    if components.is_empty() {
        format!("{prefix}\\")
    } else {
        format!("{prefix}\\{}", components.join("\\"))
    }
}

#[cfg(any(windows, test))]
fn relative_key(prefix: &str, components: &[String]) -> String {
    if components.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}{}", components.join("\\"))
    }
}

#[cfg(any(windows, test))]
fn fold_case(value: &str) -> String {
    value.to_lowercase()
}

#[cfg(windows)]
fn windows_equivalent(left: &str, right: &str) -> bool {
    let left_key = windows_lexical_key(left);
    let right_key = windows_lexical_key(right);
    if left_key == right_key {
        return true;
    }
    if !left_key.absolute
        || !right_key.absolute
        || !left_key.locally_canonicalizable
        || !right_key.locally_canonicalizable
    {
        return false;
    }

    let (Ok(left), Ok(right)) = (std::fs::canonicalize(left), std::fs::canonicalize(right)) else {
        return false;
    };
    windows_lexical_key(left.to_string_lossy().as_ref())
        == windows_lexical_key(right.to_string_lossy().as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(left: &str, right: &str) {
        assert_eq!(windows_lexical_key(left), windows_lexical_key(right));
    }

    #[test]
    fn drive_case_path_case_and_slashes_are_equivalent() {
        same(r"C:\Work\Project", r"c:/work/project");
    }

    #[test]
    fn dot_dot_and_trailing_separators_are_lexically_normalized() {
        same(r"C:\Work\Project\.\src\..", r"c:\work\project\");
    }

    #[test]
    fn drive_roots_keep_root_semantics() {
        same(r"C:\", r"c:/");
        assert_eq!(windows_lexical_key(r"C:\").value, r"drive:c:\");
    }

    #[test]
    fn unc_roots_and_descendants_are_normalized() {
        same(r"\\server\share\", r"\\SERVER\SHARE");
        same(
            r"\\server\share\Project\.\src\..",
            r"\\SERVER\SHARE\project",
        );
    }

    #[test]
    fn verbatim_drive_and_unc_forms_match_ordinary_forms() {
        same(r"\\?\C:\Work\Project", r"C:\Work\Project");
        same(r"\\?\UNC\server\share\Project", r"\\server\share\Project");
        same(r"\\?\C:\", r"C:\");
        same(r"\\?\UNC\server\share\", r"\\server\share\");
    }

    #[test]
    fn drive_relative_is_not_drive_absolute() {
        assert_ne!(
            windows_lexical_key(r"C:foo"),
            windows_lexical_key(r"C:\foo")
        );
        assert!(!windows_lexical_key(r"C:foo").absolute);
        assert!(windows_lexical_key(r"C:\foo").absolute);
    }

    #[test]
    fn unrelated_prefixes_do_not_match() {
        assert_ne!(
            windows_lexical_key(r"C:\Work"),
            windows_lexical_key(r"C:\Work-child")
        );
    }
}
