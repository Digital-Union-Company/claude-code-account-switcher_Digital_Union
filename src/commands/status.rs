use std::path::{Path, PathBuf};

use serde_json::json;

use crate::config::{AppConfig, LinkResolveError};
use crate::i18n::{I18n, Msg};
use crate::identity;
use crate::machine;
use crate::resolve;

pub fn run(config: &AppConfig, i18n: &I18n) -> i32 {
    let cwd = std::env::current_dir().expect("Cannot get current directory");

    let linked = match resolve::find_linked_dir(config, &cwd) {
        Ok(linked) => linked,
        Err(error) => {
            eprintln!("{}", resolve::error_message(i18n, &error));
            return 1;
        }
    };

    if let Some(link) = linked {
        let dir_name = std::path::Path::new(&link.directory)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&link.directory);
        let info = i18n.msg(Msg::StatusLinked(dir_name.to_string()));
        i18n.print(Msg::StatusActive(label(config, &link.account), info));
        return 0;
    }

    if let Ok(Some(ref acc)) = config.get_default() {
        let info = i18n.msg(Msg::StatusDefault);
        i18n.print(Msg::StatusActive(label(config, acc), info));
        return 0;
    }

    // Standard ~/.claude/ — show email if doctor cached one for it.
    let standard_label = standard_label(config);
    if standard_label != "~/.claude/" {
        // Cache exists; format follows StatusActive for visual consistency.
        i18n.print(Msg::StatusActive(
            standard_label,
            i18n.msg(Msg::ListStandard),
        ));
    } else {
        i18n.print(Msg::StatusStandard);
    }
    0
}

/// `claude-acc status --json [--path <dir>]` — docs/machine-api.md's single
/// routing authority. `path`, when given, is resolved relative to the
/// process's actual cwd; the decision itself (`resolve_query_path`) is kept
/// pure and separately testable from that one piece of I/O.
pub fn run_json(config: &AppConfig, path: Option<&str>) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => return machine::error("PATH_NOT_FOUND", &e.to_string(), json!({})),
    };
    let query_path = match resolve_query_path(path, &cwd) {
        Ok(p) => p,
        Err((code, message)) => return machine::error(code, &message, json!({})),
    };

    match resolve::effective_route(config, &query_path) {
        Ok(route) => machine::success(json!({
            "query_path": query_path.display().to_string(),
            "resolved_account": route.resolved_account,
            "source": route.source.as_str(),
            "owning_link_path": route.owning_link_path,
        })),
        Err(LinkResolveError::Ambiguous {
            directory,
            mappings,
        }) => {
            let mappings: Vec<serde_json::Value> = mappings
                .iter()
                .map(|(path, account)| json!({"stored_path": path, "account": account}))
                .collect();
            machine::error(
                "AMBIGUOUS_LINK",
                &format!("multiple accounts are linked to equivalent paths for {directory}"),
                json!({ "query_path": directory, "mappings": mappings }),
            )
        }
        Err(LinkResolveError::Io(e)) => {
            machine::error("LINKS_STORE_UNREADABLE", &e.to_string(), json!({}))
        }
    }
}

/// Decide the absolute directory `status --json` should resolve, and
/// validate it exists and is a directory. Pure given `cwd` — the one I/O
/// `run_json` performs is reading the real cwd, done once by the caller.
fn resolve_query_path(path: Option<&str>, cwd: &Path) -> Result<PathBuf, (&'static str, String)> {
    let candidate = match path {
        Some(p) => {
            let pb = PathBuf::from(p);
            if pb.is_absolute() { pb } else { cwd.join(pb) }
        }
        None => cwd.to_path_buf(),
    };
    if !candidate.exists() {
        return Err((
            "PATH_NOT_FOUND",
            format!("path does not exist: {}", candidate.display()),
        ));
    }
    if !candidate.is_dir() {
        return Err((
            "PATH_NOT_DIRECTORY",
            format!("path is not a directory: {}", candidate.display()),
        ));
    }
    Ok(candidate)
}

/// "<acc>" or "<acc> <email>" or "<acc> <email *>" depending on what's
/// cached. The trailing `*` flags drift between cached and current
/// keychain token (see commands/list.rs for the full convention).
fn label(config: &AppConfig, acc: &str) -> String {
    let acc_dir = config.account_path(acc);
    let Some(cache) = identity::read_cache(&acc_dir) else {
        return acc.to_string();
    };
    let Some(email) = cache.email else {
        return acc.to_string();
    };
    let drift = match (
        cache.token_hash.as_deref(),
        identity::current_token_hash(&acc_dir),
    ) {
        (Some(cached), Some(current)) if cached != current => " *",
        _ => "",
    };
    format!("{} <{}{}>", acc, email, drift)
}

/// "~/.claude/" or "~/.claude/ <email>" or "~/.claude/ <email *>".
fn standard_label(config: &AppConfig) -> String {
    let cache_path = identity::default_cache_path(&config.base_dir);
    let Some(cache) = identity::read_cache_at(&cache_path) else {
        return "~/.claude/".to_string();
    };
    let Some(email) = cache.email else {
        return "~/.claude/".to_string();
    };
    let drift = match (
        cache.token_hash.as_deref(),
        identity::standard_token_dir()
            .as_deref()
            .and_then(identity::current_token_hash),
    ) {
        (Some(cached), Some(current)) if cached != current => " *",
        _ => "",
    };
    format!("~/.claude/ <{}{}>", email, drift)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> (PathBuf, AppConfig) {
        let root =
            std::env::temp_dir().join(format!("cc-status-json-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let config = AppConfig {
            base_dir: root.join("manager"),
        };
        config.init().unwrap();
        (root, config)
    }

    #[test]
    fn resolve_query_path_defaults_to_cwd_when_no_path_given() {
        let (root, _config) = scratch("default-cwd");
        assert_eq!(resolve_query_path(None, &root).unwrap(), root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_query_path_joins_a_relative_path_onto_cwd() {
        let (root, _config) = scratch("relative");
        let child = root.join("child");
        fs::create_dir_all(&child).unwrap();
        assert_eq!(resolve_query_path(Some("child"), &root).unwrap(), child);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_query_path_accepts_an_absolute_path_regardless_of_cwd() {
        let (root, _config) = scratch("absolute");
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let unrelated_cwd = root.join("unrelated");
        fs::create_dir_all(&unrelated_cwd).unwrap();
        assert_eq!(
            resolve_query_path(Some(elsewhere.to_str().unwrap()), &unrelated_cwd).unwrap(),
            elsewhere
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_query_path_rejects_a_missing_path() {
        let (root, _config) = scratch("missing");
        let missing = root.join("does-not-exist");
        match resolve_query_path(Some(missing.to_str().unwrap()), &root) {
            Err((code, _)) => assert_eq!(code, "PATH_NOT_FOUND"),
            Ok(p) => panic!("expected PATH_NOT_FOUND, got {p:?}"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolve_query_path_rejects_a_path_that_is_a_file() {
        let (root, _config) = scratch("not-a-dir");
        let file = root.join("a-file");
        fs::write(&file, "x").unwrap();
        match resolve_query_path(Some(file.to_str().unwrap()), &root) {
            Err((code, _)) => assert_eq!(code, "PATH_NOT_DIRECTORY"),
            Ok(p) => panic!("expected PATH_NOT_DIRECTORY, got {p:?}"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_json_reports_the_linked_account_for_a_project_directory() {
        let (root, config) = scratch("run-linked");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(config.account_path("personal2")).unwrap();
        config
            .set_link(project.to_str().unwrap(), "personal2")
            .unwrap();

        assert_eq!(
            run_json(&config, Some(project.to_str().unwrap())),
            0,
            "a definite, non-ambiguous result exits 0"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_json_reports_the_standard_fallback_as_a_real_success_not_an_edge_case() {
        let (root, config) = scratch("run-standard");
        let unlinked = root.join("unlinked");
        fs::create_dir_all(&unlinked).unwrap();

        assert_eq!(run_json(&config, Some(unlinked.to_str().unwrap())), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_json_on_a_missing_path_exits_nonzero() {
        let (root, config) = scratch("run-missing");
        let missing = root.join("does-not-exist");
        assert_eq!(run_json(&config, Some(missing.to_str().unwrap())), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn run_json_on_ambiguous_links_exits_nonzero() {
        let (root, config) = scratch("run-ambiguous");
        let work = root.join("Work");
        fs::create_dir_all(&work).unwrap();
        fs::write(
            config.links_path(),
            format!(
                "{}=personal1\n{}=personal2\n",
                work.to_str().unwrap(),
                work.to_string_lossy().to_lowercase()
            ),
        )
        .unwrap();

        assert_eq!(run_json(&config, Some(work.to_str().unwrap())), 1);
        let _ = fs::remove_dir_all(root);
    }
}
