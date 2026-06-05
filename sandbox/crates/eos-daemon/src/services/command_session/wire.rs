#[cfg(target_os = "linux")]
use eos_command_session::{
    CollectCompleted, CommandResponse, CommandSessionCompletion, CommandSessionError,
};
use serde_json::{json, Value};

use crate::error::DaemonError;

pub(super) fn require_command_string(args: &Value, key: &str) -> Result<String, DaemonError> {
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

pub(super) fn caller_id_arg(args: &Value) -> &str {
    args.get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
}

pub(super) fn optional_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

pub(super) fn command_result(
    status: &str,
    exit_code: Option<i64>,
    stdout: &str,
    stderr: &str,
    command_session_id: Option<String>,
) -> Value {
    let mut response = json!({
        "status": status,
        "exit_code": exit_code,
        "output": {
            "stdout": stdout,
            "stderr": stderr,
        },
    });
    if let Some(command_session_id) = command_session_id {
        response["command_session_id"] = json!(command_session_id);
    }
    response
}

pub(super) fn command_session_not_found() -> Value {
    command_result("error", None, "", "command_session_not_found", None)
}

#[cfg(test)]
pub(super) const fn should_publish_command_session_completion(
    publish_completion: bool,
    cancelled: bool,
    owned_live_session: bool,
) -> bool {
    publish_completion && !cancelled && owned_live_session
}

#[cfg(target_os = "linux")]
pub(super) fn command_response_to_wire(response: CommandResponse) -> Value {
    response.to_wire_value()
}

#[cfg(target_os = "linux")]
pub(super) fn strip_session_id(mut response: Value) -> Value {
    if let Some(object) = response.as_object_mut() {
        object.remove("command_session_id");
    }
    response
}

#[cfg(target_os = "linux")]
pub(super) fn command_session_error(error: CommandSessionError) -> DaemonError {
    match error {
        CommandSessionError::Io(message) => DaemonError::OverlayPipeline(message),
        other => DaemonError::InvalidEnvelope(other.to_string()),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn collect_completed_request(args: &Value) -> CollectCompleted {
    let command_session_ids = args
        .get("command_session_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
    let caller_id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|caller_id| !caller_id.is_empty())
        .map(str::to_owned);
    CollectCompleted {
        command_session_ids,
        caller_id,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn command_session_completion_to_wire(completion: CommandSessionCompletion) -> Value {
    json!({
        "command_session_id": completion.command_session_id,
        "caller_id": completion.caller_id,
        "command": completion.command,
        "result": completion.result.to_wire_value(),
        "notification_result": completion.notification_result.to_wire_value(),
    })
}
