use crate::config::{AppConfig, is_reserved_name, validate_name};
use crate::i18n::{I18n, Msg};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

pub fn run(config: &AppConfig, i18n: &I18n, name: &str, force: bool, purge: bool) {
    if is_reserved_name(name) {
        i18n.print(Msg::ReservedName(name.to_string()));
        std::process::exit(1);
    }

    if !validate_name(name) {
        i18n.print(Msg::NameInvalid);
        std::process::exit(1);
    }

    let acc_dir = config.account_path(name);
    if !acc_dir.is_dir() {
        i18n.print(Msg::RemoveNotFound(name.to_string()));
        std::process::exit(1);
    }

    if !force {
        // The question names the outcome, because the two are not equally
        // recoverable and the difference is the whole point of the flag.
        let prompt = if purge {
            Msg::RemovePurgeConfirm(name.to_string())
        } else {
            Msg::RemoveConfirm(name.to_string())
        };
        print!("{}", i18n.msg(prompt));
        io::stdout().flush().unwrap();
        let mut reply = String::new();
        io::stdin().read_line(&mut reply).unwrap();
        let reply = reply.trim().to_lowercase();
        if !reply.starts_with('y') && !reply.starts_with('д') {
            i18n.print(Msg::RemoveCancelled);
            std::process::exit(1);
        }
    }

    // Hold the per-account lifecycle lock across the removal so a concurrent
    // machine `account-action` or human `login`/`add` of the same account
    // cannot be mid-login, writing into the directory this is tearing down.
    // Taken OUTSIDE the routing lock (lifecycle -> routing ordering, matching
    // add/login/account-action), so there is no lock-ordering cycle. Fail closed
    // on a genuine lock-acquisition error rather than removing unguarded.
    let _lifecycle = match config.lock_account_lifecycle_blocking(name) {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("account lock unavailable: {e}");
            std::process::exit(1);
        }
    };

    // Clearing the default (if it names this account), dropping its links, and
    // removing the account directory are ONE transaction under the routing
    // lock. Otherwise a concurrent machine `link --json` could validate this
    // account and create a link to it in the window after its links were
    // dropped but before the directory is gone — leaving a link to a deleted
    // account. The confirmation prompt above is deliberately NOT under the lock.
    //
    // `--purge` deletes outright; otherwise the directory goes to the Trash (it
    // holds transcripts/settings/plugins that exist nowhere else), with an
    // outright delete as the fallback when no trash is available. Which happened
    // is always said out loud.
    //
    // Routing cleanup is checked, not best-effort: if clearing the default or
    // dropping the links fails, the `?` aborts the transaction BEFORE the
    // directory is touched, so the account is never half-removed with its links
    // left pointing at a directory that is gone.
    let removed = config.with_routing_lock(|| -> io::Result<RemovedTo> {
        if let Ok(Some(def)) = config.get_default()
            && def == name
        {
            config.set_setting_unlocked("default", "")?;
        }
        config.remove_links_for_account_unlocked(name)?;

        if purge {
            fs::remove_dir_all(&acc_dir)?;
            return Ok(RemovedTo::Deleted);
        }
        match crate::trash::trash_dir(&acc_dir) {
            Ok(dest) => Ok(RemovedTo::Trashed(dest)),
            Err(_) => {
                fs::remove_dir_all(&acc_dir)?;
                Ok(RemovedTo::Deleted)
            }
        }
    });

    match removed {
        Ok(RemovedTo::Trashed(dest)) => i18n.print(Msg::RemoveTrashed(
            name.to_string(),
            dest.display().to_string(),
        )),
        Ok(RemovedTo::Deleted) => i18n.print(Msg::RemoveDeleted(name.to_string())),
        Err(e) => {
            // Routing cleanup or directory removal failed: the account was NOT
            // fully removed. Do not print a success banner — report and exit
            // non-zero so a script sees the failure.
            eprintln!("remove: {e}");
            std::process::exit(1);
        }
    }
}

/// Where a removed account directory ended up, so the caller reports the
/// honest outcome after the locked removal transaction.
enum RemovedTo {
    Trashed(PathBuf),
    Deleted,
}
