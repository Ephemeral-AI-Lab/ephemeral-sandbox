//! Command-session lifecycle operations for the daemon dispatcher, driving the
//! caller-keyed workspace-run registry.

#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
use eos_layerstack::require_workspace_binding;
#[cfg(target_os = "linux")]
use eos_workspace_runtime::command_session::{
    CancelCommandSession, CommandResponse, CommandSessionCompletion, CommandSessionError,
    ReadCommandProgress, StartCommandSession, WriteStdin,
};
use serde_json::{json, Value};

use crate::dispatcher::DispatchContext;
use crate::error::DaemonError;
use crate::response_timings::u64_to_f64_saturating;

#[cfg(target_os = "linux")]
use super::config::{
    command_session_config, command_session_scratch_root, runtime_command_session_config,
};
#[cfg(not(target_os = "linux"))]
use super::wire::command_result;
#[cfg(target_os = "linux")]
use super::wire::require_nonempty_string;
use super::wire::{caller_id_arg, command_session_not_found, optional_u64, require_command_string};
#[cfg(target_os = "linux")]
use super::wire::{
    collect_completed_request, command_response_to_wire, command_session_completion_to_wire,
    command_session_error, strip_session_id,
};
#[cfg(target_os = "linux")]
use eos_workspace_runtime::{StartTarget, WorkspaceRunManager};

#[cfg(target_os = "linux")]
fn workspace_run_manager() -> &'static WorkspaceRunManager {
    static MANAGER: OnceLock<WorkspaceRunManager> = OnceLock::new();
    MANAGER.get_or_init(|| {
        WorkspaceRunManager::new(
            runtime_command_session_config(),
            std::sync::Arc::new(super::host_ports::DaemonRunHostPorts),
        )
    })
}

/// `api.v1.exec_command` — command-session start contract.
pub(crate) fn op_exec_command(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let cmd = require_command_string(args, "cmd")?;
    #[cfg(target_os = "linux")]
    let command_config = command_session_config();
    #[cfg(not(target_os = "linux"))]
    let _ = &cmd;
    #[cfg(target_os = "linux")]
    let timeout_seconds = Some(exec_timeout_seconds(args, &command_config));
    #[cfg(not(target_os = "linux"))]
    let timeout_seconds = optional_u64(args, "timeout")
        .or_else(|| optional_u64(args, "timeout_seconds"))
        .map(u64_to_f64_saturating);
    #[cfg(not(target_os = "linux"))]
    if crate::adapters::workspace_run::isolated::caller_has_active_handle(caller_id_arg(args)) {
        return Ok(command_result(
            "error",
            None,
            "",
            "isolated exec_command is only supported on linux",
            None,
        ));
    }
    #[cfg(target_os = "linux")]
    if let Some(handle) = crate::adapters::workspace_run::isolated::command_handle_for_args(args) {
        let yield_time_ms =
            optional_u64(args, "yield_time_ms").unwrap_or(command_config.default_yield_time_ms);
        return start_manager_command_session(
            args,
            &cmd,
            timeout_seconds,
            yield_time_ms,
            handle.caller_id.clone(),
            StartTarget::Isolated {
                handle: Box::new(handle),
            },
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
            StartTarget::Ephemeral {
                root,
                workspace_root: PathBuf::from(binding.workspace_root),
                scratch_root: command_session_scratch_root(),
            },
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

#[cfg(any(target_os = "linux", test))]
fn exec_timeout_seconds(args: &Value, config: &crate::config::CommandSessionConfig) -> f64 {
    u64_to_f64_saturating(
        optional_u64(args, "timeout")
            .or_else(|| optional_u64(args, "timeout_seconds"))
            .unwrap_or(config.default_timeout_s),
    )
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
pub(crate) fn op_command_write_stdin(
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
pub(crate) fn op_command_read_progress(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        command_session_read_progress(args)
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
pub(crate) fn op_command_cancel(
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
pub(crate) fn op_command_collect_completed(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        let response = workspace_run_manager().collect_completed(&collect_completed_request(args));
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
    #[cfg(target_os = "linux")]
    {
        let count =
            workspace_run_manager().count_by_caller((!caller_id.is_empty()).then_some(&caller_id));
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
    target: StartTarget,
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
    let response = workspace_run_manager()
        .start(request, target)
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
fn command_session_write_stdin(args: &Value) -> Result<Value, DaemonError> {
    let request = WriteStdin {
        command_session_id: require_command_string(args, "command_session_id")?,
        chars: require_nonempty_string(args, "chars")?,
        yield_time_ms: optional_u64(args, "yield_time_ms")
            .unwrap_or(command_session_config().default_yield_time_ms),
    };
    command_session_response_to_wire(workspace_run_manager().write_stdin(request))
}

#[cfg(target_os = "linux")]
fn command_session_read_progress(args: &Value) -> Result<Value, DaemonError> {
    let last_n_lines = optional_u64(args, "last_n_lines").unwrap_or(50);
    let request = ReadCommandProgress {
        command_session_id: require_command_string(args, "command_session_id")?,
        last_n_lines: last_n_lines
            .try_into()
            .map_err(|_| DaemonError::InvalidEnvelope("last_n_lines is too large".to_owned()))?,
    };
    command_session_response_to_wire(workspace_run_manager().read_progress(request))
}

#[cfg(target_os = "linux")]
#[must_use]
pub(crate) fn active_command_sessions_for_caller(caller_id: &str) -> usize {
    let caller_id = caller_id.trim();
    if caller_id.is_empty() {
        return 0;
    }
    workspace_run_manager().count_by_caller(Some(caller_id))
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn active_command_sessions_for_caller(_caller_id: &str) -> usize {
    0
}

#[cfg(target_os = "linux")]
fn command_session_cancel(args: &Value) -> Result<Value, DaemonError> {
    let request = CancelCommandSession {
        command_session_id: require_command_string(args, "command_session_id")?,
    };
    command_session_response_to_wire(workspace_run_manager().cancel(request))
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
pub(crate) fn cleanup_command_sessions_for_caller(caller_id: &str, grace_s: Option<f64>) -> usize {
    workspace_run_manager().cleanup_caller(caller_id, grace_s)
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn cleanup_command_sessions_for_caller(
    _caller_id: &str,
    _grace_s: Option<f64>,
) -> usize {
    0
}

/// Cancel and discard every live command session across all callers (the
/// whole-sandbox cancel sweep). Returns the number cancelled.
#[cfg(target_os = "linux")]
pub(crate) fn cancel_all_command_sessions(grace_s: Option<f64>) -> usize {
    workspace_run_manager().cancel_all(grace_s)
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn cancel_all_command_sessions(_grace_s: Option<f64>) -> usize {
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
pub(crate) fn command_session_reaper_sweep() {
    workspace_run_manager().sweep_expired(Instant::now());
}

/// Startup recovery (sense-2 §2.4): a previous daemon may have left ephemeral
/// command-session metadata behind. Park an `orphan_reaped` completion for each
/// so a recovering agent learns the session is dead, then remove the stale dir.
///
/// We deliberately do **not** `killpg` the old children: their pgids are not
/// persisted, so a restarted daemon could otherwise signal a reused PID. Their
/// own runner timeout reclaims them; lease cleanup is left to LayerStack GC.
#[cfg(target_os = "linux")]
pub(crate) fn recover_orphaned_command_sessions() {
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
                    workspace_run_manager().push_completed(CommandSessionCompletion {
                        command_session_id: id.to_owned(),
                        caller_id: caller_id.to_owned(),
                        command: command.to_owned(),
                        result,
                    });
                }
            }
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn command_session_reaper_sweep() {}

#[cfg(not(target_os = "linux"))]
pub(crate) fn recover_orphaned_command_sessions() {}

#[cfg(test)]
#[path = "../../../tests/command/mod.rs"]
mod tests;
