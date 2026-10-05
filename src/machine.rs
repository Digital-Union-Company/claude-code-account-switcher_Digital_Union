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
    emit_success_document(fields);
    0
}

/// [`success`], without deciding the exit code — for a command like
/// `doctor` whose exit code encodes a semantic finding about the audited
/// data (rule 4(B)), not "did this call fail" (rule 4(A)). Still goes
/// through the same [`print_doc`] fallback, so `doctor`'s own emission
/// gets the same never-silently-empty guarantee as every other command's,
/// without a second, independently-unverified serialization path.
pub fn emit_success_document(fields: Value) {
    let mut doc = json!({ "schema_version": SCHEMA_VERSION, "ok": true });
    merge_object(&mut doc, fields);
    print_doc(&doc);
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

/// The one-JSON-document invariant must hold even if serializing `doc`
/// itself somehow fails — a literal string constant, not built through
/// `serde_json`, since that is exactly the machinery assumed broken on this
/// path. Factored out (rather than inlined in `print_doc`) so it can be
/// parsed and asserted on directly in a test instead of only trusted by
/// inspection.
const SERIALIZATION_FAILED_FALLBACK: &str = r#"{"schema_version":1,"ok":false,"error":{"code":"SERIALIZATION_FAILED","message":"internal error: failed to serialize JSON output","details":{}}}"#;

/// Serialize and print `doc`. A serialization failure must not degrade into
/// empty/partial stdout — fall back to [`SERIALIZATION_FAILED_FALLBACK`]
/// instead, so the one-JSON-document invariant holds even on that path.
fn print_doc(doc: &Value) {
    match serde_json::to_string_pretty(doc) {
        Ok(s) => println!("{s}"),
        Err(_) => println!("{SERIALIZATION_FAILED_FALLBACK}"),
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

    // The last-resort path's own output must satisfy the exact same
    // contract everything else here does — proven by parsing it with the
    // same JSON library the rest of this module uses, not by inspection.
    #[test]
    fn serialization_failed_fallback_is_itself_valid_json() {
        let parsed: Value = serde_json::from_str(SERIALIZATION_FAILED_FALLBACK)
            .expect("the last-resort fallback must itself be valid JSON");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"]["code"], "SERIALIZATION_FAILED");
        assert_eq!(parsed["error"]["details"], json!({}));
    }
}
