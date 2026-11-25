//! Genuinely-separate-process concurrency tests for the routing-store lock and
//! the `link --json` conditional mutation. Each spawns the real built binary
//! against a synthetic store via `CLAUDE_SWITCH_HOME`, so no test ever touches
//! the machine's real `~/.claude-switch`. Thread-level tests cover the lock
//! primitive; these cover the full command lifecycle across process boundaries.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

fn claude_acc() -> &'static str {
    env!("CARGO_BIN_EXE_claude-acc")
}

/// A synthetic, initialized store (config + empty links + the named accounts)
/// plus `project_count` project directories, under one temp root.
fn synthetic_store(
    tag: &str,
    accounts: &[&str],
    project_count: usize,
) -> (PathBuf, PathBuf, Vec<PathBuf>) {
    let root = std::env::temp_dir().join(format!("cc-conc-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("switch");
    fs::create_dir_all(home.join("accounts")).unwrap();
    for acc in accounts {
        fs::create_dir_all(home.join("accounts").join(acc)).unwrap();
    }
    fs::write(home.join("config"), "default=\n").unwrap();
    fs::write(home.join("links"), "").unwrap();
    let projects = (0..project_count)
        .map(|i| {
            let p = root.join(format!("p{i}"));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();
    (root, home, projects)
}

fn spawn(home: &Path, cwd: Option<&Path>, args: &[&str]) -> Child {
    let mut cmd = Command::new(claude_acc());
    cmd.env("CLAUDE_SWITCH_HOME", home).args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.spawn().expect("spawn claude-acc")
}

fn run(home: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(claude_acc())
        .env("CLAUDE_SWITCH_HOME", home)
        .args(args)
        .output()
        .expect("run claude-acc");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Pull `"<key>": "<value>"` out of a JSON document without a JSON dependency.
fn json_string(doc: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\": \"");
    let start = doc.find(&needle)? + needle.len();
    let rest = &doc[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[test]
fn concurrent_link_processes_never_lose_each_others_links() {
    // Eight processes each link a different directory to `work` at once. Without
    // the cross-process routing lock the read-modify-writes would race and links
    // would be lost; with it, every one survives.
    let (root, home, projects) = synthetic_store("no-lost-update", &["work"], 8);
    let children: Vec<Child> = projects
        .iter()
        .map(|p| spawn(&home, Some(p), &["link", "work"]))
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    let links = fs::read_to_string(home.join("links")).unwrap();
    let count = links
        .lines()
        .filter(|l| l.trim_end().ends_with("=work"))
        .count();
    assert_eq!(count, 8, "every process's link must survive:\n{links}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_machine_link_racing_an_account_removal_never_leaves_a_stale_link() {
    // A machine commit and a `remove --force --purge work` run concurrently.
    // Whichever the lock orders first, the account is never left with a link
    // pointing at a directory that is gone. `--purge` is deliberate: the
    // account directory here is a task-owned synthetic fixture, so it is
    // deleted outright rather than sent to the real system Trash.
    let (root, home, projects) = synthetic_store("link-vs-remove", &["work"], 1);
    let p0 = projects[0].to_str().unwrap().to_string();

    let (_, preview) = run(
        &home,
        &[
            "link",
            "--json",
            "--path",
            &p0,
            "--account",
            "work",
            "--dry-run",
        ],
    );
    let token = json_string(&preview, "expected_state").expect("preview token");

    let a = spawn(
        &home,
        None,
        &[
            "link",
            "--json",
            "--path",
            &p0,
            "--account",
            "work",
            "--expect-state",
            &token,
        ],
    );
    let b = spawn(&home, None, &["remove", "--force", "--purge", "work"]);
    let link_code = a.wait_with_output().unwrap().status.code().unwrap_or(-1);
    let remove_code = b.wait_with_output().unwrap().status.code().unwrap_or(-1);

    // Non-vacuity guards: the test must not pass simply because both commands
    // crashed and wrote nothing. `remove --purge` of an existing account always
    // succeeds, and it must actually have removed the account directory; the
    // link commit must have reached a defined outcome — it either won the lock
    // and committed (0), or lost it to the removal and was refused with a
    // structured envelope (1) — never a crash.
    assert_eq!(remove_code, 0, "the account removal must succeed");
    assert!(
        link_code == 0 || link_code == 1,
        "the link commit must reach a defined 0/1 outcome, got {link_code}"
    );
    assert!(
        !home.join("accounts").join("work").is_dir(),
        "the removal must actually have deleted the account directory"
    );

    let links = fs::read_to_string(home.join("links")).unwrap_or_default();
    for line in links.lines() {
        if let Some((_, acct)) = line.rsplit_once('=') {
            let acct = acct.trim();
            if acct != "default" {
                assert!(
                    home.join("accounts").join(acct).is_dir(),
                    "stale link to a removed account {acct:?} survived:\n{links}"
                );
            }
        }
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn two_commits_with_the_same_token_let_exactly_one_change_the_store() {
    // Cross-process compare-and-swap: both processes present the same
    // expected-state token. The lock serializes them; the first commit changes
    // the store, which invalidates the token the second still holds, so the
    // second is refused (STATE_CHANGED) rather than double-writing.
    let (root, home, projects) = synthetic_store("cas", &["work"], 1);
    let p0 = projects[0].to_str().unwrap().to_string();
    let (_, preview) = run(
        &home,
        &[
            "link",
            "--json",
            "--path",
            &p0,
            "--account",
            "work",
            "--dry-run",
        ],
    );
    let token = json_string(&preview, "expected_state").expect("preview token");

    let a = spawn(
        &home,
        None,
        &[
            "link",
            "--json",
            "--path",
            &p0,
            "--account",
            "work",
            "--expect-state",
            &token,
        ],
    );
    let b = spawn(
        &home,
        None,
        &[
            "link",
            "--json",
            "--path",
            &p0,
            "--account",
            "work",
            "--expect-state",
            &token,
        ],
    );
    let codes: Vec<i32> = [a, b]
        .into_iter()
        .map(|c| c.wait_with_output().unwrap().status.code().unwrap_or(-1))
        .collect();

    assert!(
        codes.contains(&0) && codes.contains(&1),
        "exactly one commit should change the store and the other be refused, got {codes:?}"
    );
    // And the store ends with the one link, not a duplicate.
    let links = fs::read_to_string(home.join("links")).unwrap();
    assert_eq!(
        links.lines().filter(|l| !l.is_empty()).count(),
        1,
        "{links}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_synthetic_store_override_never_resolves_to_the_home_default_store() {
    // CLAUDE_SWITCH_HOME is the isolation mechanism every test here (and the
    // released-binary capability check) relies on. Verify its boundary: with it
    // set, claude-acc initializes and operates ONLY inside it and never falls
    // back to the home-derived ~/.claude-switch — so a test can never
    // accidentally read or mutate a real production store. HOME and USERPROFILE
    // are pointed at a separate empty directory; a state-initializing command
    // must land entirely in the override home and leave no `.claude-switch`
    // under the stand-in home.
    let root =
        std::env::temp_dir().join(format!("cc-conc-override-boundary-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("switch"); // the CLAUDE_SWITCH_HOME override
    let fake_home = root.join("home"); // stand-in for the user's real home
    fs::create_dir_all(&fake_home).unwrap();
    // Deliberately do NOT pre-create `home`: init must create it under the
    // override, which is exactly what proves the override is in force.

    // `link <account>` initializes the store, then fails because the account
    // does not exist — its non-zero exit is irrelevant here; only WHERE it
    // wrote matters.
    let _ = Command::new(claude_acc())
        .env("CLAUDE_SWITCH_HOME", &home)
        .env("HOME", &fake_home)
        .env("USERPROFILE", &fake_home)
        .args(["link", "work"])
        .current_dir(&fake_home)
        .output()
        .expect("run claude-acc");

    assert!(
        home.join("config").exists(),
        "the override home must be where claude-acc initialized its store"
    );
    assert!(
        !fake_home.join(".claude-switch").exists(),
        "claude-acc must never fall back to the home-default store when the override is set"
    );
    let _ = fs::remove_dir_all(root);
}
