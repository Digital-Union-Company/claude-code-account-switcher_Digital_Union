use serde_json::{Value, json};

use crate::config::AppConfig;
use crate::i18n::{self, I18n, Msg};
use crate::identity::{self, CachedInfo, Usage, UsageResult, UsageSource, UsageWindow};
use crate::machine;

const BAR_WIDTH: usize = 20;

pub fn run(config: &AppConfig, i18n: &I18n) {
    let default_acc = config.get_default().ok().flatten();
    let accounts = config.list_accounts().unwrap_or_default();

    // Same visibility rule as `list`: the standard ~/.claude/ row shows up only
    // if it has actually been logged into (live token or a prior doctor cache).
    let standard = identity::standard_token_dir();
    let standard_logged_in = standard
        .as_deref()
        .and_then(identity::current_token_hash)
        .is_some()
        || identity::read_cache_at(&identity::default_cache_path(&config.base_dir)).is_some();

    if accounts.is_empty() && !standard_logged_in {
        i18n.print(Msg::ListEmpty);
        return;
    }

    i18n.print(Msg::UsageHeader);

    for acc in &accounts {
        let acc_dir = config.account_path(acc);
        let is_default = Some(acc.as_str()) == default_acc.as_deref();
        let marker = if is_default { "★" } else { " " };
        let suffix = label_suffix(identity::read_cache(&acc_dir));
        println!("  {} {}{}", marker, acc, suffix);
        print_result(identity::fetch_account_usage(&acc_dir), i18n, acc);
    }

    if standard_logged_in {
        let cache = identity::read_cache_at(&identity::default_cache_path(&config.base_dir));
        let suffix = label_suffix(cache);
        println!("    ~/.claude/{}  {}", suffix, i18n.msg(Msg::ListStandard));
        if let Some(dir) = standard.as_deref() {
            print_result(identity::fetch_account_usage(dir), i18n, "~/.claude/");
        }
    }
}

/// `claude-acc usage --json` — docs/machine-api.md §5's informational,
/// non-authoritative usage data. Carries no `plan` (that stays owned by
/// `list --json`/`doctor --json`), and never gates anything — a slow or
/// failing fetch degrades to `"unavailable"`, never blocks.
pub fn run_json(config: &AppConfig) -> i32 {
    let accounts = match config.list_accounts() {
        Ok(v) => v,
        Err(e) => return machine::error("CONFIG_STORE_UNREADABLE", &e.to_string(), json!({})),
    };

    // Same account-universe/visibility rule as `list --json`, so the GUI can
    // join the two by name deterministically.
    let standard = identity::standard_token_dir();
    let standard_logged_in = standard
        .as_deref()
        .and_then(identity::current_token_hash)
        .is_some()
        || identity::read_cache_at(&identity::default_cache_path(&config.base_dir)).is_some();

    let mut entries: Vec<Value> = accounts
        .iter()
        .map(|acc| {
            let dir = config.account_path(acc);
            usage_entry_json(acc, identity::fetch_account_usage_detail(&dir))
        })
        .collect();

    if standard_logged_in && let Some(dir) = standard.as_deref() {
        entries.push(usage_entry_json(
            crate::sessions::DEFAULT_LABEL,
            identity::fetch_account_usage_detail(dir),
        ));
    }

    machine::success(json!({ "accounts": entries }))
}

/// Build one `accounts[]` entry from an already-obtained `UsageSource` — the
/// decision half of `run_json`, kept separate from the I/O that produces a
/// `UsageSource` (network/keychain/filesystem) so it is directly testable
/// with hand-built fixtures instead of a real token.
fn usage_entry_json(name: &str, source: UsageSource) -> Value {
    match source {
        UsageSource::Live(usage, fetched_at) => json!({
            "name": name,
            "source": "live",
            "fetched_at": identity::epoch_to_iso8601(fetched_at),
            "five_hour": window_json(&usage.five_hour, /* suppress_expired */ false),
            "seven_day": window_json(&usage.seven_day, /* suppress_expired */ false),
        }),
        UsageSource::Cache(usage, fetched_at) => json!({
            "name": name,
            "source": "cache",
            "fetched_at": identity::epoch_to_iso8601(fetched_at),
            // Matches the human form's print_cached_usage: a cached window
            // whose reset has already passed is omitted rather than shown
            // as current (machine-api.md §5/§20).
            "five_hour": window_json(&usage.five_hour, /* suppress_expired */ true),
            "seven_day": window_json(&usage.seven_day, /* suppress_expired */ true),
        }),
        UsageSource::Unavailable => json!({
            "name": name,
            "source": "unavailable",
            "fetched_at": null,
            "five_hour": null,
            "seven_day": null,
        }),
    }
}

/// `{"utilization": .., "resets_at": ..}` for a present window, `null` for
/// an absent one — and, when `suppress_expired`, also `null` for a window
/// whose `resets_at` has already passed (never presented as current).
fn window_json(window: &Option<UsageWindow>, suppress_expired: bool) -> Value {
    let Some(window) = window else {
        return Value::Null;
    };
    if suppress_expired && has_reset(window) {
        return Value::Null;
    }
    json!({ "utilization": window.utilization, "resets_at": window.resets_at })
}

fn print_result(result: UsageResult, i18n: &I18n, name: &str) {
    match result {
        UsageResult::Ok(usage) => print_usage(&usage, i18n),
        UsageResult::NoToken => {
            println!("      {}", i18n.msg(Msg::DoctorNoToken(name.to_string())));
        }
        UsageResult::Cached(usage, age) => {
            println!("      {}", i18n.msg(Msg::UsageFromCache(age)));
            print_cached_usage(&usage, i18n);
        }
        UsageResult::Offline => {
            println!("      {}", i18n.msg(Msg::DoctorOffline));
        }
    }
}

pub fn print_usage(usage: &Usage, i18n: &I18n) {
    if let Some(w) = &usage.five_hour {
        print_window("5h", w, i18n);
    }
    if let Some(w) = &usage.seven_day {
        print_window("7d", w, i18n);
    }
}

/// The same two windows, rendered from a reading that is no longer live.
///
/// A window whose reset has already gone by gets no bar at all. Claude Code
/// only measures while a session is running, so the figure it last wrote sits
/// there unchanged across the reset — which is how a limit that has actually
/// started over comes to look like one that is still full. A bar is read
/// before any caveat printed beside it, so the honest thing is not to draw
/// one.
fn print_cached_usage(usage: &Usage, i18n: &I18n) {
    for (label, w) in [("5h", &usage.five_hour), ("7d", &usage.seven_day)] {
        let Some(w) = w else { continue };
        if has_reset(w) {
            println!("      {}  {}", label, i18n.msg(Msg::UsageWindowHasReset));
        } else {
            print_window(label, w, i18n);
        }
    }
}

/// Whether this window's reset moment has already passed, making its
/// utilization the spend of a window that has ended.
fn has_reset(w: &UsageWindow) -> bool {
    w.resets_at
        .as_deref()
        .and_then(identity::seconds_until)
        .is_some_and(|secs| secs <= 0)
}

fn print_window(label: &str, w: &UsageWindow, i18n: &I18n) {
    let pct = w.utilization.clamp(0.0, 100.0);
    let reset = reset_label(w.resets_at.as_deref(), i18n);
    println!(
        "      {}  {}  {:>3}%  {}",
        label,
        bar(pct),
        pct.round() as i64,
        reset
    );
}

/// "resets in 2h 14m" / "available now" / "" (when the window has no reset).
fn reset_label(resets_at: Option<&str>, i18n: &I18n) -> String {
    let Some(resets_at) = resets_at else {
        return String::new();
    };
    match identity::seconds_until(resets_at) {
        Some(secs) if secs > 0 => i18n.msg(Msg::UsageResetsIn(i18n::forward_duration(
            secs as u64,
            i18n.lang,
        ))),
        Some(_) => i18n.msg(Msg::UsageAvailableNow),
        None => String::new(),
    }
}

/// "  <email>" or "  <email>  Max 20x" from a cached identity. Empty when the
/// cache has no email (e.g. `doctor` hasn't audited the account yet).
pub fn label_suffix(cache: Option<CachedInfo>) -> String {
    let Some(c) = cache else {
        return String::new();
    };
    let Some(email) = c.email else {
        return String::new();
    };
    let plan = c.plan.map(|p| format!("  {}", p)).unwrap_or_default();
    format!("  <{}>{}", email, plan)
}

/// A 20-cell `[████░░░░…]` bar for a 0–100 percentage.
fn bar(pct: f64) -> String {
    let filled = ((pct / 100.0) * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);
    let mut s = String::with_capacity(BAR_WIDTH + 2);
    s.push('[');
    for _ in 0..filled {
        s.push('█');
    }
    for _ in 0..BAR_WIDTH - filled {
        s.push('░');
    }
    s.push(']');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_empty_at_zero() {
        assert_eq!(bar(0.0), format!("[{}]", "░".repeat(BAR_WIDTH)));
    }

    #[test]
    fn bar_full_at_hundred() {
        assert_eq!(bar(100.0), format!("[{}]", "█".repeat(BAR_WIDTH)));
    }

    #[test]
    fn bar_half_at_fifty() {
        assert_eq!(bar(50.0), format!("[{}{}]", "█".repeat(10), "░".repeat(10)));
    }

    #[test]
    fn bar_clamps_overflow() {
        assert_eq!(bar(250.0), format!("[{}]", "█".repeat(BAR_WIDTH)));
    }

    fn cache(email: Option<&str>, plan: Option<&str>) -> CachedInfo {
        CachedInfo {
            email: email.map(String::from),
            uuid: None,
            org: None,
            fetched_at: None,
            token_hash: None,
            plan: plan.map(String::from),
        }
    }

    #[test]
    fn label_suffix_email_only() {
        assert_eq!(
            label_suffix(Some(cache(Some("a@b.com"), None))),
            "  <a@b.com>"
        );
    }

    #[test]
    fn label_suffix_email_and_plan() {
        assert_eq!(
            label_suffix(Some(cache(Some("a@b.com"), Some("Max 20x")))),
            "  <a@b.com>  Max 20x"
        );
    }

    #[test]
    fn label_suffix_empty_without_cache_or_email() {
        assert_eq!(label_suffix(None), "");
        assert_eq!(label_suffix(Some(cache(None, Some("Max 20x")))), "");
    }

    fn window(resets_at: Option<&str>) -> UsageWindow {
        UsageWindow {
            utilization: 97.0,
            resets_at: resets_at.map(String::from),
        }
    }

    // A saved reading whose reset has gone by is the spend of a window that
    // has already started over — the case a bar would misreport as "still
    // full", so `print_cached_usage` draws none.
    #[test]
    fn a_window_whose_reset_has_passed_is_recognised() {
        assert!(has_reset(&window(Some("2020-01-01T00:00:00Z"))));
    }

    #[test]
    fn a_window_still_running_is_not() {
        assert!(!has_reset(&window(Some("2099-01-01T00:00:00Z"))));
    }

    // No reset timestamp, and one that does not parse, both mean "we cannot
    // say it has reset" — the bar is drawn rather than suppressed on a guess.
    #[test]
    fn a_window_without_a_usable_reset_is_not_treated_as_reset() {
        assert!(!has_reset(&window(None)));
        assert!(!has_reset(&window(Some("not a timestamp"))));
    }

    // --- window_json / usage_entry_json (the --json formatting decision) ---

    #[test]
    fn window_json_is_null_for_an_absent_window() {
        assert_eq!(window_json(&None, false), serde_json::Value::Null);
        assert_eq!(window_json(&None, true), serde_json::Value::Null);
    }

    #[test]
    fn window_json_passes_a_present_window_through_as_is() {
        let w = Some(window(Some("2099-01-01T00:00:00Z")));
        assert_eq!(
            window_json(&w, false),
            serde_json::json!({"utilization": 97.0, "resets_at": "2099-01-01T00:00:00Z"})
        );
    }

    #[test]
    fn window_json_suppresses_an_expired_window_only_when_asked_to() {
        let expired = Some(window(Some("2020-01-01T00:00:00Z")));
        assert_eq!(window_json(&expired, true), serde_json::Value::Null);
        // Live results are not suppressed — matching the human form, which
        // only ever suppresses a *cached* reading's stale window.
        assert_ne!(window_json(&expired, false), serde_json::Value::Null);
    }

    #[test]
    fn usage_entry_json_live_carries_no_plan_field_and_formats_fetched_at() {
        let usage = Usage {
            five_hour: Some(window(Some("2099-01-01T00:00:00Z"))),
            seven_day: None,
        };
        let entry = usage_entry_json("work", UsageSource::Live(usage, 0));
        assert_eq!(entry["name"], "work");
        assert_eq!(entry["source"], "live");
        assert_eq!(entry["fetched_at"], "1970-01-01T00:00:00Z");
        assert_eq!(entry["seven_day"], serde_json::Value::Null);
        assert!(entry.get("plan").is_none(), "{entry:?}");
    }

    #[test]
    fn usage_entry_json_cache_suppresses_an_expired_window() {
        let usage = Usage {
            five_hour: Some(window(Some("2020-01-01T00:00:00Z"))),
            seven_day: Some(window(Some("2099-01-01T00:00:00Z"))),
        };
        let entry = usage_entry_json("work", UsageSource::Cache(usage, 0));
        assert_eq!(entry["source"], "cache");
        assert_eq!(entry["five_hour"], serde_json::Value::Null);
        assert_ne!(entry["seven_day"], serde_json::Value::Null);
    }

    #[test]
    fn usage_entry_json_unavailable_has_every_data_field_null() {
        let entry = usage_entry_json("work", UsageSource::Unavailable);
        assert_eq!(entry["source"], "unavailable");
        assert_eq!(entry["fetched_at"], serde_json::Value::Null);
        assert_eq!(entry["five_hour"], serde_json::Value::Null);
        assert_eq!(entry["seven_day"], serde_json::Value::Null);
    }

    // --- run_json (filesystem-only path: no token anywhere in these fixtures) ---

    fn run_json_scratch(tag: &str) -> AppConfig {
        let dir = std::env::temp_dir().join(format!("cc-usage-json-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = AppConfig { base_dir: dir };
        config.init().unwrap();
        config
    }

    // The standard ~/.claude/ row, when it appears, always refers to the
    // real machine's home directory — there is no scratch-dir equivalent —
    // so this only asserts the exit code, which holds either way.
    #[test]
    fn run_json_succeeds_with_no_managed_accounts() {
        let config = run_json_scratch("empty");
        assert_eq!(run_json(&config), 0);
        let _ = std::fs::remove_dir_all(&config.base_dir);
    }

    #[test]
    fn run_json_reports_unavailable_for_a_managed_account_with_no_token() {
        let config = run_json_scratch("no-token");
        std::fs::create_dir_all(config.account_path("work")).unwrap();
        assert_eq!(
            run_json(&config),
            0,
            "an unavailable reading is still a successful enumeration"
        );
        let _ = std::fs::remove_dir_all(&config.base_dir);
    }
}
