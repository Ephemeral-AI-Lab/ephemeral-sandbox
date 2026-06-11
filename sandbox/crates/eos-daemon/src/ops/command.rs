//! Command-session dispatcher handlers, driving the caller-keyed
//! command runtime in `eos-command-ops`.

use std::path::PathBuf;

use eos_command_ops::{
    command_ops, command_session_config, command_session_scratch_root, ExecTarget,
};
use eos_command_session::{
    CancelCommandSession, CollectCompleted, CommandResponse, CommandSessionCompletion,
    CommandSessionError, ReadCommandProgress, StartCommandSession, WriteStdin,
};
use eos_layerstack::require_workspace_binding;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::request_args::{require_command_string, require_nonempty_string};
use crate::response::u64_to_f64_saturating;
use crate::runtime::context::DispatchContext;

/// `api.v1.exec_command` — command-session start contract.
pub(crate) fn op_exec_command(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let cmd = require_command_string(args, "cmd")?;
    let command_config = command_session_config();
    let timeout_seconds = Some(exec_timeout_seconds(args, &command_config));
    let yield_time_ms =
        optional_u64(args, "yield_time_ms").unwrap_or(command_config.default_yield_time_ms);
    let isolated_binding = context.services().and_then(|services| {
        services
            .workspace
            .command_binding_for(&super::caller_id_or_default(args))
    });
    if let Some(binding) = isolated_binding {
        return start_manager_command_session(
            args,
            &cmd,
            timeout_seconds,
            yield_time_ms,
            binding.caller_id.clone(),
            ExecTarget::Isolated {
                binding: Box::new(binding),
            },
        );
    }
    let root = PathBuf::from(require_command_string(args, "layer_stack_root")?);
    let binding = require_workspace_binding(&root)?;
    start_manager_command_session(
        args,
        &cmd,
        timeout_seconds,
        yield_time_ms,
        caller_id_arg(args).to_owned(),
        ExecTarget::Ephemeral {
            root,
            workspace_root: PathBuf::from(binding.workspace_root),
            scratch_root: command_session_scratch_root(),
        },
    )
}

fn exec_timeout_seconds(args: &Value, config: &crate::config::CommandSessionConfig) -> f64 {
    u64_to_f64_saturating(
        optional_u64(args, "timeout")
            .or_else(|| optional_u64(args, "timeout_seconds"))
            .unwrap_or(config.default_timeout_s),
    )
}

pub(crate) fn op_command_write_stdin(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    command_session_write_stdin(args)
}

pub(crate) fn op_command_read_progress(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    command_session_read_progress(args)
}

pub(crate) fn op_command_cancel(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    command_session_cancel(args)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub(crate) fn op_command_collect_completed(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let response = command_ops().collect_completed(&collect_completed_request(args));
    let completions = response
        .completions
        .into_iter()
        .map(command_session_completion_to_wire)
        .collect::<Vec<_>>();
    Ok(json!({"success": response.success, "completions": completions}))
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub(crate) fn op_command_session_count(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let caller_id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let count = command_ops().count_by_caller((!caller_id.is_empty()).then_some(&caller_id));
    Ok(json!({"success": true, "caller_id": caller_id, "count": count}))
}

fn start_manager_command_session(
    args: &Value,
    cmd: &str,
    timeout_seconds: Option<f64>,
    yield_time_ms: u64,
    caller_id: String,
    target: ExecTarget,
) -> Result<Value, DaemonError> {
    let request = StartCommandSession {
        invocation_id: args
            .get("invocation_id")
            .and_then(Value::as_str)
            .unwrap_or("exec_command")
            .to_owned(),
        caller_id,
        cmd: cmd.to_owned(),
        timeout_seconds,
        yield_time_ms,
    };
    let response = command_ops()
        .exec_command(request, target)
        .map_err(command_session_error)?;
    let wire = command_response_to_wire(response);
    if wire
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "running")
    {
        Ok(wire)
    } else {
        Ok(strip_session_id(wire))
    }
}

fn command_session_write_stdin(args: &Value) -> Result<Value, DaemonError> {
    let request = WriteStdin {
        command_session_id: require_command_string(args, "command_session_id")?,
        chars: require_nonempty_string(args, "chars")?,
        yield_time_ms: optional_u64(args, "yield_time_ms")
            .unwrap_or(command_session_config().default_yield_time_ms),
    };
    command_session_response_to_wire(command_ops().write_stdin(request))
}

fn command_session_read_progress(args: &Value) -> Result<Value, DaemonError> {
    let last_n_lines = optional_u64(args, "last_n_lines").unwrap_or(50);
    let request = ReadCommandProgress {
        command_session_id: require_command_string(args, "command_session_id")?,
        last_n_lines: last_n_lines
            .try_into()
            .map_err(|_| DaemonError::InvalidEnvelope("last_n_lines is too large".to_owned()))?,
    };
    command_session_response_to_wire(command_ops().read_command_progress(request))
}

fn command_session_cancel(args: &Value) -> Result<Value, DaemonError> {
    let request = CancelCommandSession {
        command_session_id: require_command_string(args, "command_session_id")?,
    };
    command_session_response_to_wire(command_ops().cancel(request))
}

fn command_session_response_to_wire(
    response: Result<CommandResponse, CommandSessionError>,
) -> Result<Value, DaemonError> {
    match response {
        Ok(response) => Ok(command_response_to_wire(response)),
        Err(CommandSessionError::NotFound(_)) => Ok(command_session_not_found()),
        Err(error) => Err(command_session_error(error)),
    }
}

fn caller_id_arg(args: &Value) -> &str {
    args.get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
}

fn optional_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

fn command_result(
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

fn command_session_not_found() -> Value {
    command_result("error", None, "", "command_session_not_found", None)
}

fn command_response_to_wire(response: CommandResponse) -> Value {
    response.to_wire_value()
}

fn strip_session_id(mut response: Value) -> Value {
    if let Some(object) = response.as_object_mut() {
        object.remove("command_session_id");
    }
    response
}

fn command_session_error(error: CommandSessionError) -> DaemonError {
    match error {
        CommandSessionError::Io(message) => DaemonError::OverlayPipeline(message),
        other => DaemonError::InvalidEnvelope(other.to_string()),
    }
}

fn collect_completed_request(args: &Value) -> CollectCompleted {
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

fn command_session_completion_to_wire(completion: CommandSessionCompletion) -> Value {
    json!({
        "command_session_id": completion.command_session_id,
        "caller_id": completion.caller_id,
        "command": completion.command,
        "result": completion.result.to_wire_value(),
    })
}

#[cfg(test)]
#[path = "../../tests/unit/command/mod.rs"]
mod tests;
