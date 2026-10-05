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

/// Where an `effective_route` result came from — the three cases
/// claude-acc's resolver can produce (docs/machine-api.md's `status --json`
/// `source` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteSource {
    /// An ancestor directory link matched (whether or not it resolves to a
    /// usable managed account — see `effective_route`).
    Linked,
    /// No link matched, and a configured managed default account applies.
    Default,
    /// No link matched and no managed default is configured.
    Standard,
}

impl RouteSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            RouteSource::Linked => "linked",
            RouteSource::Default => "default",
            RouteSource::Standard => "standard",
        }
    }
}

/// The account claude-acc's routing would actually select for `dir` right
/// now, and why — the single source of truth shared by `status --json` and
/// the native Windows shim (`windows_shim::target_for_cwd`), so the two can
/// never silently disagree about what gets launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRoute {
    /// Never empty — `"default"` names the standard, unmanaged account the
    /// same way `list --json` does, never a null/absent value.
    pub resolved_account: String,
    pub source: RouteSource,
    /// The stored link's own spelling, when `source == Linked` — present
    /// even when that link's account turned out to be stale/missing and the
    /// effective result fell back to the standard account.
    pub owning_link_path: Option<String>,
}

/// Decide the effective account for `dir`, applying exactly the same
/// fallback claude-acc's native shim applies: a linked, existing managed
/// account wins; a link to a missing/stale account or the literal `default`
/// falls back to standard; otherwise a configured managed default wins if
/// its account still exists, else standard. Ambiguity (two+ equivalent
/// stored links naming different accounts) is propagated, never guessed.
pub fn effective_route(config: &AppConfig, dir: &Path) -> Result<EffectiveRoute, LinkResolveError> {
    if let Some(link) = find_linked_dir(config, dir)? {
        let resolved_account = if link.account != crate::sessions::DEFAULT_LABEL
            && config.account_exists(&link.account)
        {
            link.account.clone()
        } else {
            crate::sessions::DEFAULT_LABEL.to_string()
        };
        return Ok(EffectiveRoute {
            resolved_account,
            source: RouteSource::Linked,
            owning_link_path: Some(link.directory),
        });
    }

    match config.get_default().map_err(LinkResolveError::from)? {
        Some(name) if config.account_exists(&name) => Ok(EffectiveRoute {
            resolved_account: name,
            source: RouteSource::Default,
            owning_link_path: None,
        }),
        Some(_stale) => Ok(EffectiveRoute {
            resolved_account: crate::sessions::DEFAULT_LABEL.to_string(),
            source: RouteSource::Default,
            owning_link_path: None,
        }),
        None => Ok(EffectiveRoute {
            resolved_account: crate::sessions::DEFAULT_LABEL.to_string(),
            source: RouteSource::Standard,
            owning_link_path: None,
        }),
    }
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

    // --- effective_route: the shared status/shim routing decision ---

    #[test]
    fn linked_to_an_existing_managed_account() {
        let (root, config) = scratch("route-linked");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(config.account_path("personal2")).unwrap();
        config
            .set_link(project.to_str().unwrap(), "personal2")
            .unwrap();

        let route = effective_route(&config, &project).unwrap();
        assert_eq!(route.resolved_account, "personal2");
        assert_eq!(route.source, RouteSource::Linked);
        assert_eq!(route.owning_link_path.as_deref(), project.to_str());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn linked_explicitly_to_the_literal_default() {
        let (root, config) = scratch("route-linked-default");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        config
            .set_link(project.to_str().unwrap(), "default")
            .unwrap();

        let route = effective_route(&config, &project).unwrap();
        assert_eq!(route.resolved_account, "default");
        assert_eq!(route.source, RouteSource::Linked);
        assert_eq!(route.owning_link_path.as_deref(), project.to_str());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_link_to_a_missing_managed_account_falls_back_to_default_but_still_reports_linked() {
        let (root, config) = scratch("route-stale-link");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        // Never create the "missing-account" directory.
        config
            .set_link(project.to_str().unwrap(), "missing-account")
            .unwrap();

        let route = effective_route(&config, &project).unwrap();
        assert_eq!(route.resolved_account, "default");
        assert_eq!(route.source, RouteSource::Linked);
        assert_eq!(route.owning_link_path.as_deref(), project.to_str());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn no_link_but_a_configured_managed_default() {
        let (root, config) = scratch("route-default");
        let unlinked = root.join("unlinked");
        fs::create_dir_all(&unlinked).unwrap();
        fs::create_dir_all(config.account_path("work")).unwrap();
        config.set_default("work").unwrap();

        let route = effective_route(&config, &unlinked).unwrap();
        assert_eq!(route.resolved_account, "work");
        assert_eq!(route.source, RouteSource::Default);
        assert_eq!(route.owning_link_path, None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn no_link_and_no_default_falls_back_to_standard() {
        let (root, config) = scratch("route-standard");
        let unlinked = root.join("unlinked");
        fs::create_dir_all(&unlinked).unwrap();

        let route = effective_route(&config, &unlinked).unwrap();
        assert_eq!(route.resolved_account, "default");
        assert_eq!(route.source, RouteSource::Standard);
        assert_eq!(route.owning_link_path, None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn configured_default_whose_account_directory_vanished_falls_back_to_standard() {
        let (root, config) = scratch("route-stale-default");
        let unlinked = root.join("unlinked");
        fs::create_dir_all(&unlinked).unwrap();
        fs::create_dir_all(config.account_path("ghost")).unwrap();
        config.set_default("ghost").unwrap();
        fs::remove_dir_all(config.account_path("ghost")).unwrap();

        let route = effective_route(&config, &unlinked).unwrap();
        assert_eq!(route.resolved_account, "default");
        assert_eq!(route.source, RouteSource::Default);
        assert_eq!(route.owning_link_path, None);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn ambiguous_links_are_propagated_not_guessed() {
        let (root, config) = scratch("route-ambiguous");
        fs::write(
            config.links_path(),
            "C:\\Work=personal1\nc:/work/=personal2\n",
        )
        .unwrap();

        assert!(matches!(
            effective_route(&config, Path::new(r"c:\WORK")),
            Err(LinkResolveError::Ambiguous { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn route_source_as_str_matches_the_contract_vocabulary() {
        assert_eq!(RouteSource::Linked.as_str(), "linked");
        assert_eq!(RouteSource::Default.as_str(), "default");
        assert_eq!(RouteSource::Standard.as_str(), "standard");
    }
}
