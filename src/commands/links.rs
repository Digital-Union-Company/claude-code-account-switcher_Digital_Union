use serde_json::json;

use crate::config::{AppConfig, LinkResolveError, StrictLinksRead};
use crate::i18n::{I18n, Msg};
use crate::machine;
use crate::resolve;

pub fn run(config: &AppConfig, i18n: &I18n) -> i32 {
    let links = match config.all_links() {
        Ok(links) => links,
        Err(error) => {
            eprintln!(
                "{}",
                resolve::error_message(i18n, &LinkResolveError::Io(error))
            );
            return 1;
        }
    };
    if links.is_empty() {
        i18n.print(Msg::LinksEmpty);
        return 0;
    }

    i18n.print(Msg::LinksHeader);

    let cwd = std::env::current_dir().ok();
    let mut conflict_paths = Vec::new();
    let mut failed = false;
    let active_link = match cwd
        .as_deref()
        .map(|directory| resolve::find_linked_dir(config, directory))
    {
        Some(Ok(link)) => link,
        Some(Err(error)) => {
            if let LinkResolveError::Ambiguous { mappings, .. } = &error {
                conflict_paths.extend(mappings.iter().map(|(path, _)| path.clone()));
            }
            eprintln!("{}", resolve::error_message(i18n, &error));
            failed = true;
            None
        }
        None => None,
    };

    let home = dirs::home_dir();
    let mut sorted = links;
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    for (dir, account) in &sorted {
        let display = home
            .as_deref()
            .and_then(|h| h.to_str())
            .and_then(|h_str| dir.strip_prefix(h_str).map(|rest| format!("~{}", rest)))
            .unwrap_or_else(|| dir.clone());

        if conflict_paths.iter().any(|path| path == dir) {
            println!(
                "  {} → {}  {}",
                display,
                account,
                i18n.msg(Msg::LinksConflict)
            );
        } else if active_link.as_ref().is_some_and(|active| {
            active.account == *account && crate::path_identity::equivalent(&active.directory, dir)
        }) {
            println!(
                "  {} → {}  {}",
                display,
                account,
                i18n.msg(Msg::LinksActive)
            );
        } else {
            println!("  {} → {}", display, account);
        }
    }
    if failed { 1 } else { 0 }
}

/// `claude-acc links --json` — docs/machine-api.md §3's stored routing map
/// plus a genuine whole-store conflict analysis. Unlike the human command,
/// a line that fails to parse is reported (`LINKS_STORE_INVALID`), never
/// silently dropped from an `ok: true` map presented as complete.
pub fn run_json(config: &AppConfig) -> i32 {
    let links = match config.read_links_strict() {
        Ok(StrictLinksRead::Ok(links)) => links,
        Ok(StrictLinksRead::Malformed(bad)) => {
            let lines: Vec<serde_json::Value> = bad
                .iter()
                .map(|m| json!({"line_number": m.line_number, "raw": m.raw}))
                .collect();
            return machine::error(
                "LINKS_STORE_INVALID",
                "the links store contains one or more lines that could not be parsed",
                json!({ "lines": lines }),
            );
        }
        Err(e) => return machine::error("LINKS_STORE_UNREADABLE", &e.to_string(), json!({})),
    };

    let link_entries: Vec<serde_json::Value> = links
        .iter()
        .map(|(path, account)| json!({"stored_path": path, "account": account}))
        .collect();
    let conflict_entries: Vec<serde_json::Value> = conflict_groups(&links)
        .into_iter()
        .map(|group| {
            let mappings: Vec<serde_json::Value> = group
                .iter()
                .map(|(path, account)| json!({"stored_path": path, "account": account}))
                .collect();
            json!({"code": "AMBIGUOUS_EQUIVALENT_PATHS", "mappings": mappings})
        })
        .collect();

    machine::success(json!({ "links": link_entries, "conflicts": conflict_entries }))
}

/// Whole-store conflict analysis (docs/machine-api.md §3, ADR-28): group the
/// stored links into filesystem-equivalence classes via claude-acc's own
/// `path_identity::equivalent`, then keep only the groups naming two or more
/// distinct accounts. Equivalent spellings mapped to the *same* account are
/// not a conflict — they are simply separate, ordinary `links` entries.
/// Every member of a reported group is kept, in original file order, so an
/// arbitrary-size ambiguity group is represented losslessly.
fn conflict_groups(links: &[(String, String)]) -> Vec<Vec<(String, String)>> {
    let n = links.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], x: usize) -> usize {
        if parent[x] != x {
            parent[x] = find(parent, parent[x]);
        }
        parent[x]
    }

    for i in 0..n {
        for j in (i + 1)..n {
            if crate::path_identity::equivalent(&links[i].0, &links[j].0) {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }

    let mut order: Vec<usize> = Vec::new();
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups
            .entry(root)
            .or_insert_with(|| {
                order.push(root);
                Vec::new()
            })
            .push(i);
    }

    order
        .into_iter()
        .filter_map(|root| groups.remove(&root))
        .filter(|members| {
            let mut accounts: Vec<&str> = members.iter().map(|&i| links[i].1.as_str()).collect();
            accounts.sort_unstable();
            accounts.dedup();
            accounts.len() >= 2
        })
        .map(|members| members.into_iter().map(|i| links[i].clone()).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn pair(path: &str, account: &str) -> (String, String) {
        (path.to_string(), account.to_string())
    }

    #[test]
    fn conflict_groups_is_empty_for_no_links() {
        assert_eq!(conflict_groups(&[]), Vec::<Vec<(String, String)>>::new());
    }

    #[test]
    fn conflict_groups_is_empty_when_nothing_is_equivalent() {
        let links = vec![pair("/one", "work"), pair("/two", "personal")];
        assert!(conflict_groups(&links).is_empty());
    }

    // Exact-string equivalence holds on every platform (non-Windows path
    // identity is exact-string by design), so this is the one conflict case
    // this suite can exercise without `#[cfg(windows)]`.
    #[test]
    fn identical_stored_paths_mapped_to_different_accounts_conflict() {
        let links = vec![pair("/work", "personal1"), pair("/work", "personal2")];
        let groups = conflict_groups(&links);
        assert_eq!(groups, vec![links]);
    }

    #[test]
    fn identical_stored_paths_mapped_to_the_same_account_do_not_conflict() {
        let links = vec![pair("/work", "personal1"), pair("/work", "personal1")];
        assert!(conflict_groups(&links).is_empty());
    }

    #[test]
    fn a_three_way_group_keeps_every_member_even_though_two_share_an_account() {
        let links = vec![
            pair("/work", "personal1"),
            pair("/work", "personal1"),
            pair("/work", "personal2"),
        ];
        let groups = conflict_groups(&links);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], links, "every member must be preserved");
    }

    #[test]
    fn two_independent_conflicts_are_both_reported_as_separate_groups() {
        let links = vec![
            pair("/a", "x1"),
            pair("/a", "x2"),
            pair("/b", "y1"),
            pair("/b", "y2"),
        ];
        let groups = conflict_groups(&links);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], vec![pair("/a", "x1"), pair("/a", "x2")]);
        assert_eq!(groups[1], vec![pair("/b", "y1"), pair("/b", "y2")]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_and_slash_variants_of_three_different_accounts_form_one_group() {
        let links = vec![
            pair(r"C:\Work", "personal1"),
            pair("c:/work/", "personal2"),
            pair(r"C:\WORK\", "work"),
        ];
        let groups = conflict_groups(&links);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 3, "every spelling must be preserved");
    }

    fn scratch(tag: &str) -> AppConfig {
        let dir = std::env::temp_dir().join(format!("cc-links-json-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let config = AppConfig { base_dir: dir };
        config.init().unwrap();
        config
    }

    #[test]
    fn run_json_on_an_empty_store_succeeds_with_empty_arrays() {
        let config = scratch("empty");
        assert_eq!(run_json(&config), 0);
        let _ = fs::remove_dir_all(&config.base_dir);
    }

    #[test]
    fn run_json_preserves_original_stored_spelling() {
        let config = scratch("spelling");
        fs::write(config.links_path(), "C:\\Work=personal1\n").unwrap();
        // The exit code alone proves the strict parse succeeded; spelling
        // preservation itself is covered at the `all_links`/`read_links_strict`
        // level in config.rs, which `run_json` reads from verbatim.
        assert_eq!(run_json(&config), 0);
        let _ = fs::remove_dir_all(&config.base_dir);
    }

    #[test]
    fn run_json_on_a_malformed_line_exits_nonzero() {
        let config = scratch("malformed");
        fs::write(config.links_path(), "missing-delimiter\n").unwrap();
        assert_eq!(run_json(&config), 1);
        let _ = fs::remove_dir_all(&config.base_dir);
    }

    #[test]
    fn run_json_on_an_unreadable_store_exits_nonzero() {
        let config = scratch("unreadable");
        // Replace the links file with a directory, so `read_links_strict`'s
        // `fs::read_to_string` fails at the filesystem level instead of
        // seeing a malformed/empty file.
        fs::remove_file(config.links_path()).unwrap();
        fs::create_dir_all(config.links_path()).unwrap();
        assert_eq!(run_json(&config), 1);
        let _ = fs::remove_dir_all(&config.base_dir);
    }
}
