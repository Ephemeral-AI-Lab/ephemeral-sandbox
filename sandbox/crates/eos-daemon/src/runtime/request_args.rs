//! Shared request argument and JSON conversion helpers.

use eos_layerstack::WorkspaceBinding;
use serde_json::{json, Value};

use crate::error::DaemonError;

pub(crate) fn require_string(args: &Value, key: &str) -> Result<String, DaemonError> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if value.is_empty() {
        return Err(DaemonError::InvalidEnvelope(format!("{key} is required")));
    }
    Ok(value)
}

/// Read `key` as a trimmed owned string, defaulting to empty when absent or
/// non-string. Unlike [`require_string`], an empty result is not an error.
pub(crate) fn trimmed_string(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

pub(crate) fn require_raw_string(args: &Value, key: &str) -> Result<String, DaemonError> {
    let Some(value) = args.get(key) else {
        return Err(DaemonError::InvalidEnvelope(format!("{key} is required")));
    };
    let Some(value) = value.as_str() else {
        return Err(DaemonError::InvalidEnvelope(format!(
            "{key} must be a string"
        )));
    };
    Ok(value.to_owned())
}

/// Read `key` as a present, non-blank string, preserving its exact bytes
/// (shell commands keep leading/trailing whitespace). A missing or non-string
/// value reads as "required"; a blank value as "must be non-empty".
pub(crate) fn require_command_string(args: &Value, key: &str) -> Result<String, DaemonError> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DaemonError::InvalidEnvelope(format!("{key} is required")))?;
    if value.trim().is_empty() {
        return Err(DaemonError::InvalidEnvelope(format!(
            "{key} must be non-empty"
        )));
    }
    Ok(value.to_owned())
}

/// Like [`require_command_string`] but only rejects the empty string, so
/// whitespace-only payloads (e.g. stdin chars) stay valid.
pub(crate) fn require_nonempty_string(args: &Value, key: &str) -> Result<String, DaemonError> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DaemonError::InvalidEnvelope(format!("{key} is required")))?;
    if value.is_empty() {
        return Err(DaemonError::InvalidEnvelope(format!(
            "{key} must be non-empty"
        )));
    }
    Ok(value.to_owned())
}

pub(crate) fn binding_to_value(binding: &WorkspaceBinding) -> Result<Value, DaemonError> {
    serde_json::to_value(binding).map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))
}

pub(crate) fn timings_to_value_map(
    timings: &std::collections::BTreeMap<String, f64>,
) -> serde_json::Map<String, Value> {
    timings
        .iter()
        .map(|(key, value)| (key.clone(), json!(value)))
        .collect()
}
