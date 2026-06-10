//! The caller-workspace feature: file ops, command-session runs, the isolated
//! lifecycle, and the cross-substrate cancel surface.
//!
//! A workspace run composes the `eos-command-ops` tier with the
//! daemon-resident seams (OCC publish, resource telemetry, isolated-audit
//! sink). Each family submodule owns its dispatcher handlers; [`cancel`] is
//! the coordinator that tears down a caller's command sessions and isolated
//! namespace in order, so "cancel never publishes" stays structural.

pub(crate) mod cancel;
pub(crate) mod files;
pub(crate) mod isolated;
pub(crate) mod run;

use serde_json::{json, Value};

/// Structured handler-level error payload shared by the workspace families:
/// `{"success": false, "error": {kind, message, details}}`, returned as an
/// ordinary (`Ok`) op response rather than a transport error envelope.
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
/// `invalid_argument` error payload.
pub(crate) fn require_arg(args: &Value, key: &str) -> Result<String, Value> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if value.is_empty() {
        return Err(error_json(
            "invalid_argument",
            format!("{key} is required"),
            json!({"key": key}),
        ));
    }
    Ok(value)
}
