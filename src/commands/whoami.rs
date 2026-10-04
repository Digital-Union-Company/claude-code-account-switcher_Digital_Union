// `claude-acc whoami` — print the most-identifying string for the active
// account, suitable for use in shell prompts and conditional scripts.
//
// Resolution order (matches `status` for active-account detection):
//   1. Cached email (managed account or standard ~/.claude/)
//   2. Account name (managed but no cached email)
//   3. The literal `default` (standard with no cached identity)
//
// Successful resolution exits 0 and prints one non-empty value. Routing
// conflicts fail closed instead of presenting a fallback identity as valid.

use crate::config::{AppConfig, LinkResolveError};
use crate::i18n::I18n;
use crate::identity;
use crate::resolve;
use std::path::Path;

pub fn run(config: &AppConfig, i18n: &I18n) -> i32 {
    let cwd = std::env::current_dir().expect("Cannot get current directory");
    match whoami_label(config, &cwd) {
        Ok(label) => {
            println!("{label}");
            0
        }
        Err(error) => {
            eprintln!("{}", resolve::error_message(i18n, &error));
            1
        }
    }
}

fn whoami_label(config: &AppConfig, cwd: &Path) -> Result<String, LinkResolveError> {
    match resolve::resolve_account(config, cwd)? {
        Some(account) if account != "default" => Ok(account_label(config, &account)),
        Some(_) | None => Ok(standard_label(config)),
    }
}

fn account_label(config: &AppConfig, acc: &str) -> String {
    let acc_dir = config.account_path(acc);
    identity::read_cache(&acc_dir)
        .and_then(|c| c.email)
        .unwrap_or_else(|| acc.to_string())
}

fn standard_label(config: &AppConfig) -> String {
    let cache_path = identity::default_cache_path(&config.base_dir);
    identity::read_cache_at(&cache_path)
        .and_then(|c| c.email)
        .unwrap_or_else(|| "default".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> (std::path::PathBuf, AppConfig) {
        let root = std::env::temp_dir().join(format!("cc-whoami-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let config = AppConfig {
            base_dir: root.join("manager"),
        };
        config.init().unwrap();
        (root, config)
    }

    #[test]
    fn linked_named_account_keeps_existing_label_output() {
        let (root, config) = scratch("named");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(config.account_path("work")).unwrap();
        config.set_link(repo.to_str().unwrap(), "work").unwrap();

        assert_eq!(whoami_label(&config, &repo).unwrap(), "work");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_and_implicit_upstream_default_keep_default_output() {
        let (root, config) = scratch("upstream-default");
        let implicit = root.join("implicit");
        let explicit = root.join("explicit");
        fs::create_dir_all(&implicit).unwrap();
        fs::create_dir_all(&explicit).unwrap();
        config
            .set_link(explicit.to_str().unwrap(), "default")
            .unwrap();

        assert_eq!(whoami_label(&config, &implicit).unwrap(), "default");
        assert_eq!(whoami_label(&config, &explicit).unwrap(), "default");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn configured_named_default_keeps_named_output() {
        let (root, config) = scratch("named-default");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(config.account_path("work")).unwrap();
        config.set_default("work").unwrap();

        assert_eq!(whoami_label(&config, &repo).unwrap(), "work");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn ambiguity_returns_error_instead_of_fallback_identity() {
        let (root, config) = scratch("ambiguity");
        fs::write(
            config.links_path(),
            "C:\\Work=personal1\nc:/work/=personal2\n",
        )
        .unwrap();
        config.set_default("private-default").unwrap();

        assert!(matches!(
            whoami_label(&config, Path::new(r"c:\WORK")),
            Err(LinkResolveError::Ambiguous { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }
}
