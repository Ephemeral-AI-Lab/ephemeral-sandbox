//! Command-session operations for the daemon dispatcher.

mod config;
#[cfg(target_os = "linux")]
mod ports;
mod wire;

#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
use eos_command_session::{
    CancelCommandSession, CommandResponse, CommandSessionCompletion, CommandSessionError,
    CommandSessionManager, DynCommandWorkspacePolicy, StartCommandSession, WriteStdin,
};
#[cfg(target_os = "linux")]
use eos_ephemeral_workspace::command_session::EphemeralCommandPolicy;
#[cfg(target_os = "linux")]
use eos_isolated_workspace::command_session::IsolatedCommandPolicy;
#[cfg(target_os = "linux")]
use eos_layerstack::require_workspace_binding;
use serde_json::{json, Value};

use crate::dispatcher::DispatchContext;
use crate::error::DaemonError;
use crate::response_timings::u64_to_f64_saturating;

pub(crate) use config::configure_command_sessions;
#[cfg(target_os = "linux")]
use config::{
    command_session_config, command_session_scratch_root, runtime_command_session_config,
};
#[cfg(target_os = "linux")]
use ports::ephemeral::DaemonEphemeralCommandPort;
#[cfg(target_os = "linux")]
use ports::isolated::DaemonIsolatedCommandPort;
#[cfg(not(target_os = "linux"))]
use wire::command_result;
#[cfg(test)]
use wire::should_publish_command_session_completion;
use wire::{caller_id_arg, command_session_not_found, optional_u64, require_command_string};
#[cfg(target_os = "linux")]
use wire::{
    collect_completed_request, command_response_to_wire, command_session_completion_to_wire,
    command_session_error, strip_session_id,
};

#[cfg(target_os = "linux")]
fn command_session_manager() -> &'static CommandSessionManager {
    static MANAGER: OnceLock<CommandSessionManager> = OnceLock::new();
    MANAGER.get_or_init(|| CommandSessionManager::new(runtime_command_session_config()))
}

/// `api.v1.exec_command` — command-session start contract.
pub fn op_exec_command(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let cmd = require_command_string(args, "cmd")?;
    #[cfg(target_os = "linux")]
    let command_config = command_session_config();
    #[cfg(not(target_os = "linux"))]
    let _ = &cmd;
    let timeout_seconds = optional_u64(args, "timeout")
        .or_else(|| optional_u64(args, "timeout_seconds"))
        .map(u64_to_f64_saturating);
    #[cfg(not(target_os = "linux"))]
    if crate::services::isolated_workspace::caller_has_active_handle(caller_id_arg(args)) {
        return Ok(command_result(
            "error",
            None,
            "",
            "isolated exec_command is only supported on linux",
            None,
        ));
    }
    #[cfg(target_os = "linux")]
    if let Some(handle) = crate::services::isolated_workspace::command_handle_for_args(args) {
        let yield_time_ms =
            optional_u64(args, "yield_time_ms").unwrap_or(command_config.default_yield_time_ms);
        return start_manager_command_session(
            args,
            &cmd,
            timeout_seconds,
            yield_time_ms,
            handle.caller_id.clone(),
            Box::new(IsolatedCommandPolicy::new(DaemonIsolatedCommandPort::new(
                handle,
            ))),
        );
    }

    #[cfg(target_os = "linux")]
    {
        let yield_time_ms =
            optional_u64(args, "yield_time_ms").unwrap_or(command_config.default_yield_time_ms);
        let root = PathBuf::from(require_command_string(args, "layer_stack_root")?);
        let binding = require_workspace_binding(&root)?;
        start_manager_command_session(
            args,
            &cmd,
            timeout_seconds,
            yield_time_ms,
            caller_id_arg(args).to_owned(),
            Box::new(EphemeralCommandPolicy::new(
                DaemonEphemeralCommandPort::new(
                    root,
                    PathBuf::from(binding.workspace_root),
                    command_session_scratch_root(),
                ),
            )),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = timeout_seconds;
        Ok(command_result(
            "error",
            None,
            "",
            "command sessions are only supported on linux",
            None,
        ))
    }
}

// Dispatcher op handlers share the `Result<Value, DaemonError>` ABI even when
// a specific op encodes all domain failures in its JSON response.
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "dispatcher handlers share a fallible ABI"
    )
)]
pub fn op_command_write_stdin(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        command_session_write_stdin(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(command_session_not_found())
    }
}

#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "dispatcher handlers share a fallible ABI"
    )
)]
pub fn op_command_cancel(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        command_session_cancel(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(command_session_not_found())
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_command_collect_completed(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        let response =
            command_session_manager().collect_completed(&collect_completed_request(args));
        let completions = response
            .completions
            .into_iter()
            .map(command_session_completion_to_wire)
            .collect::<Vec<_>>();
        Ok(json!({"success": response.success, "completions": completions}))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(json!({"success": true, "completions": []}))
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_command_session_count(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let caller_id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    #[cfg(target_os = "linux")]
    {
        let count = command_session_manager()
            .count_by_caller((!caller_id.is_empty()).then_some(&caller_id));
        Ok(json!({"success": true, "caller_id": caller_id, "count": count}))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(json!({"success": true, "caller_id": caller_id, "count": 0}))
    }
}

#[cfg(target_os = "linux")]
fn start_manager_command_session(
    args: &Value,
    cmd: &str,
    timeout_seconds: Option<f64>,
    yield_time_ms: u64,
    caller_id: String,
    policy: DynCommandWorkspacePolicy,
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
        max_output_tokens: optional_u64(args, "max_output_tokens"),
    };
    let response = command_session_manager()
        .start_boxed(request, policy)
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

#[cfg(target_os = "linux")]
pub(crate) fn command_session_write_stdin(args: &Value) -> Result<Value, DaemonError> {
    let request = WriteStdin {
        command_session_id: require_command_string(args, "command_session_id")?,
        chars: args
            .get("chars")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        terminate: args
            .get("terminate")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        yield_time_ms: optional_u64(args, "yield_time_ms")
            .unwrap_or(command_session_config().default_yield_time_ms),
        max_output_tokens: optional_u64(args, "max_output_tokens"),
    };
    command_session_response_to_wire(command_session_manager().write_stdin(request))
}

#[cfg(target_os = "linux")]
pub(crate) fn command_session_cancel(args: &Value) -> Result<Value, DaemonError> {
    let request = CancelCommandSession {
        command_session_id: require_command_string(args, "command_session_id")?,
        max_output_tokens: optional_u64(args, "max_output_tokens"),
    };
    command_session_response_to_wire(command_session_manager().cancel(request))
}

#[cfg(target_os = "linux")]
fn command_session_response_to_wire(
    response: Result<CommandResponse, CommandSessionError>,
) -> Result<Value, DaemonError> {
    match response {
        Ok(response) => Ok(command_response_to_wire(response)),
        Err(CommandSessionError::NotFound(_)) => Ok(command_session_not_found()),
        Err(error) => Err(command_session_error(error)),
    }
}

#[cfg(target_os = "linux")]
/// Best-effort lifecycle backstop for callers that bypass the model-facing
/// `RequireNoBackgroundSessions` hook.
pub fn cleanup_command_sessions_for_caller(caller_id: &str, grace_s: Option<f64>) -> usize {
    command_session_manager().cleanup_caller(caller_id, grace_s)
}

#[cfg(not(target_os = "linux"))]
pub const fn cleanup_command_sessions_for_caller(_caller_id: &str, _grace_s: Option<f64>) -> usize {
    0
}

/// Periodic reaper (sense-2 §2.4, §3): enforce the per-session timeout backstop
/// and finalize any session whose child has exited without a live poller,
/// parking the completion for the heartbeat. The runner enforces the per-call
/// timeout internally (primary); this is the backstop for a wedged or
/// no-timeout runner and the only finalizer for fire-and-forget sessions. A
/// session started without an explicit `timeout` falls back to the configured
/// wall-clock cap so it can never run forever.
#[cfg(target_os = "linux")]
pub fn command_session_reaper_sweep() {
    let _ = command_session_manager().sweep_expired(Instant::now());
}

/// Startup recovery (sense-2 §2.4): a previous daemon may have left ephemeral
/// command-session metadata behind. Park an `orphan_reaped` completion for each
/// so a recovering agent learns the session is dead, then remove the stale dir.
///
/// We deliberately do **not** `killpg` the old children: their pgids are not
/// persisted, so a restarted daemon could otherwise signal a reused PID. Their
/// own runner timeout reclaims them; lease cleanup is left to LayerStack GC.
#[cfg(target_os = "linux")]
pub fn recover_orphaned_command_sessions() {
    let dir = command_session_scratch_root();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Ok(bytes) = std::fs::read(path.join("metadata.json")) {
            if let Ok(meta) = serde_json::from_slice::<Value>(&bytes) {
                let id = meta
                    .get("command_session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !id.is_empty() {
                    let caller_id = meta
                        .get("caller_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let command = meta
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let result = CommandResponse {
                        status: "error".to_owned(),
                        exit_code: Some(1),
                        stdout: String::new(),
                        stderr: "orphan_reaped: daemon restarted".to_owned(),
                        command_session_id: Some(id.to_owned()),
                        workspace_mode: None,
                        metadata: Value::Null,
                    };
                    command_session_manager()
                        .registry()
                        .push_completed(CommandSessionCompletion {
                            command_session_id: id.to_owned(),
                            caller_id: caller_id.to_owned(),
                            command: command.to_owned(),
                            result: result.clone(),
                            notification_result: result,
                        });
                }
            }
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn command_session_reaper_sweep() {}

#[cfg(not(target_os = "linux"))]
pub fn recover_orphaned_command_sessions() {}

#[cfg(test)]
#[path = "../../../tests/command/mod.rs"]
mod tests;
