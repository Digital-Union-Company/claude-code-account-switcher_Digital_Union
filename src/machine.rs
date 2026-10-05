//! The shared JSON success/error envelope used by every `--json` command.
//!
//! docs/machine-api.md (and ClaudeManagerPS's docs/MACHINE-API-CONTRACT.md,
//! which this implements) requires exactly one JSON document on stdout per
//! invocation, carrying `schema_version`/`ok` at the top level, with
//! operational failures (as opposed to a command's own semantic exit code,
//! e.g. `doctor`'s) always shaped as `{"ok": false, "error": {code, message,
//! details}}`. Centralizing both shapes here is what keeps every command
//! honest about the invariant instead of re-deriving it per call site.

use serde_json::{Value, json};

pub const SCHEMA_VERSION: u64 = 1;

/// Print `{"schema_version":1,"ok":true, ...fields}` as the one JSON document
/// on stdout and return the exit code every success case under contract rule
/// 4(A) uses. `fields` must be a JSON object; its keys are merged into the
/// envelope.
pub fn success(fields: Value) -> i32 {
    let mut doc = json!({ "schema_version": SCHEMA_VERSION, "ok": true });
    merge_object(&mut doc, fields);
    print_doc(&doc);
    0
}

/// Print the structured operational-failure envelope and return exit code 1.
/// `code` is a stable, per-command string a caller branches on; `message` is
/// display-only; `details` is command-specific structured context (an empty
/// object when there is none).
pub fn error(code: &str, message: &str, details: Value) -> i32 {
    let doc = json!({
        "schema_version": SCHEMA_VERSION,
        "ok": false,
        "error": { "code": code, "message": message, "details": details },
    });
    print_doc(&doc);
    1
}

fn merge_object(base: &mut Value, extra: Value) {
    if let (Some(base_obj), Value::Object(extra_obj)) = (base.as_object_mut(), extra) {
        for (k, v) in extra_obj {
            base_obj.insert(k, v);
        }
    }
}

/// Serialize and print `doc`. A serialization failure must not degrade into
/// empty/partial stdout — fall back to a fixed, always-valid error document
/// instead, so the one-JSON-document invariant holds even on that path.
fn print_doc(doc: &Value) {
    match serde_json::to_string_pretty(doc) {
        Ok(s) => println!("{s}"),
        Err(_) => println!(
            r#"{{"schema_version":1,"ok":false,"error":{{"code":"SERIALIZATION_FAILED","message":"internal error: failed to serialize JSON output","details":{{}}}}}}"#
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_object_inserts_every_key_from_fields() {
        let mut base = json!({"schema_version": 1, "ok": true});
        merge_object(&mut base, json!({"a": 1, "b": "two"}));
        assert_eq!(base["schema_version"], 1);
        assert_eq!(base["ok"], true);
        assert_eq!(base["a"], 1);
        assert_eq!(base["b"], "two");
    }

    #[test]
    fn merge_object_ignores_a_non_object_extra() {
        let mut base = json!({"ok": true});
        merge_object(&mut base, json!("not an object"));
        assert_eq!(base, json!({"ok": true}));
    }
}
