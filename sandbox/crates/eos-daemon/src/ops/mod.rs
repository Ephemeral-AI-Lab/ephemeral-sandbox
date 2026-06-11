//! Daemon JSON operation adapters.
//!
//! These modules parse wire `args`, call the owning service/crate, and shape
//! the stable response object. Domain lifecycle policy should live below this
//! adapter layer.

pub(crate) mod cancel;
pub(crate) mod checkpoint;
pub(crate) mod command;
pub(crate) mod control;
pub(crate) mod files;
pub(crate) mod isolation;
pub(crate) mod plugin;

use serde_json::{json, Value};

/// Structured handler-level error payload shared by workspace-family ops:
/// `{"success": false, "error": {kind, message, details}}`.
pub(crate) fn error_json(kind: &str, message: impl Into<String>, details: Value) -> Value {
    json!({
        "success": false,
        "error": {
            "kind": kind,
            "message": message.into(),
            "details": if details.is_null() { json!({}) } else { details },
        },
    })
}

/// Read `key` as a trimmed non-empty string, encoding a miss as a structured
/// `invalid_argument` error payload (the workspace-family arg convention).
pub(crate) fn require_arg(args: &Value, key: &str) -> Result<String, Value> {
    crate::request_args::require_string(args, key).map_err(|_| {
        error_json(
            "invalid_argument",
            format!("{key} is required"),
            json!({"key": key}),
        )
    })
}

/// The wire caller id used for isolated-workspace routing: `caller_id`
/// trimmed, defaulting to `"default"` when absent.
pub(crate) fn caller_id_or_default(args: &Value) -> String {
    args.get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .trim()
        .to_owned()
}
