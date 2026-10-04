use crate::config::{AppConfig, LinkResolveError, ResolvedLink};
use crate::i18n::{I18n, Msg};
use std::path::Path;

/// Walk up from `dir` to root, checking links for each ancestor.
pub fn resolve_account(config: &AppConfig, dir: &Path) -> Result<Option<String>, LinkResolveError> {
    let mut current = dir.to_path_buf();
    loop {
        if let Some(dir_str) = current.to_str()
            && let Some(link) = config.find_link(dir_str)?
        {
            return Ok(Some(link.account));
        }
        if !current.pop() {
            break;
        }
    }
    // Fallback to default only after every ancestor was checked without an
    // identity conflict. Ambiguity must never silently become default.
    config.get_default().map_err(LinkResolveError::from)
}

/// Find the stored link that owns the active directory (for status/display).
pub fn find_linked_dir(
    config: &AppConfig,
    dir: &Path,
) -> Result<Option<ResolvedLink>, LinkResolveError> {
    let mut current = dir.to_path_buf();
    loop {
        if let Some(dir_str) = current.to_str()
            && let Some(link) = config.find_link(dir_str)?
        {
            return Ok(Some(link));
        }
        if !current.pop() {
            break;
        }
    }
    Ok(None)
}

pub fn error_message(i18n: &I18n, error: &LinkResolveError) -> String {
    match error {
        LinkResolveError::Io(error) => i18n.msg(Msg::LinkResolveFailed(error.to_string())),
        LinkResolveError::Ambiguous {
            directory,
            mappings,
        } => {
            let mappings = mappings
                .iter()
                .map(|(path, account)| format!("{path} → {account}"))
                .collect::<Vec<_>>()
                .join("; ");
            i18n.msg(Msg::LinkIdentityConflict(directory.clone(), mappings))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> (std::path::PathBuf, AppConfig) {
        let root = std::env::temp_dir().join(format!("cc-resolve-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let config = AppConfig {
            base_dir: root.join("manager"),
        };
        config.init().unwrap();
        (root, config)
    }

    #[test]
    fn nearest_linked_ancestor_still_wins() {
        let (root, config) = scratch("nearest");
        let outer = root.join("repo");
        let inner = outer.join("nested");
        let child = inner.join("child");
        fs::create_dir_all(&child).unwrap();
        config.set_link(outer.to_str().unwrap(), "outer").unwrap();
        config.set_link(inner.to_str().unwrap(), "inner").unwrap();

        assert_eq!(
            resolve_account(&config, &child).unwrap().as_deref(),
            Some("inner")
        );
        assert_eq!(
            find_linked_dir(&config, &child).unwrap().unwrap().directory,
            inner.to_string_lossy()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn ambiguity_is_propagated_instead_of_falling_back_to_default() {
        let (root, config) = scratch("ambiguity");
        fs::write(
            config.links_path(),
            "C:\\Work=personal1\nc:/work/=personal2\n",
        )
        .unwrap();
        config.set_default("default-account").unwrap();

        assert!(matches!(
            resolve_account(&config, Path::new(r"c:\WORK")),
            Err(LinkResolveError::Ambiguous { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }
}
