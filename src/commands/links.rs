use crate::config::{AppConfig, LinkResolveError};
use crate::i18n::{I18n, Msg};
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
            active.account == *account
                && crate::path_identity::equivalent(&active.directory, dir)
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
    if failed {
        1
    } else {
        0
    }
}
