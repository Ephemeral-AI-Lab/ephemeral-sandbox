//! Linux command-session finalize, teardown, and stdin/cancel handling.

use std::io::Write;
use std::thread;
use std::time::Duration;

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use serde_json::{json, Value};

use eos_ephemeral_workspace::command_session::types::{
    EphemeralCommandFinalizeContext, EphemeralCommandSessionPort,
};
use eos_ephemeral_workspace::{
    AgentId, EphemeralSnapshot, EphemeralWorkspace, EphemeralWorkspaceError, EphemeralWorkspaceOps,
    InvocationId, PathChange, PublishOutcome, WorkspacePublisherPort,
    WorkspaceRoot as EphemeralWorkspaceRoot,
};
use eos_isolated_workspace::command_session::types::{
    IsolatedCommandFinalizeContext, IsolatedCommandSessionPort,
};
use eos_isolated_workspace::IsolatedWorkspaceOps;
use eos_layerstack::LayerStack;
use eos_protocol::LayerChange;
use eos_runner::RunResult;
use eos_workspace_api::{
    CommandWorkspaceOps, FinalizeCommandRequest, WorkspaceApiError, WorkspaceCommandOutcome,
};

use super::lifecycle::{require_string, EphemeralCommandWorkspace, IsolatedCommandWorkspace};
use super::session::{
    command_session_registry, lock_command_session_state, wait_for_yield, CommandSession,
    WaitOutcome,
};
use super::{command_result, command_session_config, command_session_not_found, optional_u64};
use crate::error::DaemonError;
use crate::response_timings::{resource_timings, timing_map};
use crate::services::overlay::DaemonPublisherPort;

fn command_workspace_error(error: WorkspaceApiError) -> DaemonError {
    DaemonError::InvalidEnvelope(error.to_string())
}

fn workspace_api_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("daemon_command_workspace_error", error.to_string())
}

fn finalize_request(
    session: &CommandSession,
    runner: Option<&RunResult>,
    status: &str,
    exit_code: i64,
    stdout: &str,
    include_session_id: bool,
) -> Result<FinalizeCommandRequest, DaemonError> {
    Ok(FinalizeCommandRequest {
        finalize_context: json!({}),
        runner_result: runner
            .map(serde_json::to_value)
            .transpose()
            .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
        command_elapsed_s: session.started_at.elapsed().as_secs_f64(),
        spool_truncated: session.output.spool_truncated(),
        status: status.to_owned(),
        exit_code: Some(exit_code),
        stdout: stdout.to_owned(),
        stderr: String::new(),
        command_session_id: include_session_id.then(|| session.id.clone()),
    })
}

fn write_final_response(path: &std::path::Path, response: &Value) -> Result<(), DaemonError> {
    std::fs::write(
        path,
        serde_json::to_vec_pretty(response)
            .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    Ok(())
}

fn command_outcome_response(outcome: WorkspaceCommandOutcome) -> Value {
    let mode = outcome.mode.as_str();
    let mut response = json!({
        "success": outcome.success,
        "workspace": mode,
        "workspace_mode": mode,
        "status": outcome.status,
        "exit_code": outcome.exit_code,
        "output": {
            "stdout": outcome.stdout,
            "stderr": outcome.stderr,
        },
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
        "conflict": outcome.conflict,
        "conflict_reason": outcome.conflict_reason,
        "changed_paths": outcome.changed_paths,
        "changed_path_kinds": outcome.changed_path_kinds,
        "mutation_source": outcome.mutation_source,
        "error": null,
        "timings": outcome.timings,
    });
    if let Some(command_session_id) = outcome.command_session_id {
        response["command_session_id"] = json!(command_session_id);
    }
    if let Some(metadata) = outcome.metadata.as_object() {
        for (key, value) in metadata {
            response[key] = value.clone();
        }
    }
    response
}

struct IsolatedCommandFinalizePort<'a> {
    workspace: &'a IsolatedCommandWorkspace,
}

impl IsolatedCommandSessionPort for IsolatedCommandFinalizePort<'_> {
    fn finalize_context(&self) -> Result<IsolatedCommandFinalizeContext, WorkspaceApiError> {
        let manifest = LayerStack::open(self.workspace.handle.layer_stack_root.clone())
            .and_then(|stack| stack.read_active_manifest())
            .map_err(workspace_api_error)?;
        Ok(IsolatedCommandFinalizeContext {
            agent_id: self.workspace.handle.agent_id.clone(),
            workspace_handle_id: self.workspace.handle.workspace_handle_id.clone(),
            manifest_version: self.workspace.handle.manifest_version,
            manifest_root_hash: self.workspace.handle.manifest_root_hash.clone(),
            upperdir: self.workspace.handle.upperdir.clone(),
            base_timings: timing_map(resource_timings(&manifest, 0)),
        })
    }
}

struct EphemeralCommandFinalizePort<'a> {
    session: &'a CommandSession,
    workspace: &'a EphemeralCommandWorkspace,
}

impl EphemeralCommandSessionPort for EphemeralCommandFinalizePort<'_> {
    fn finalize_context(&self) -> Result<EphemeralCommandFinalizeContext, WorkspaceApiError> {
        let manifest = LayerStack::open(self.workspace.root.clone())
            .and_then(|stack| stack.read_active_manifest())
            .map_err(workspace_api_error)?;
        Ok(EphemeralCommandFinalizeContext {
            workspace: EphemeralWorkspace {
                layer_stack_root: EphemeralWorkspaceRoot(self.workspace.root.clone()),
                workspace_root: self.workspace.workspace_root.clone(),
                agent_id: AgentId(self.session.agent_id.clone()),
                invocation_id: InvocationId(self.session.id.clone()),
                snapshot: EphemeralSnapshot {
                    lease_id: self.workspace.lease_id.clone(),
                    manifest_version: self.workspace.manifest_version,
                    manifest_root_hash: self.workspace.manifest_root_hash.clone(),
                    layer_paths: self.workspace.layer_paths.clone(),
                },
                dirs: self.workspace.dirs.clone(),
            },
            base_timings: timing_map(resource_timings(&manifest, 0)),
        })
    }

    fn publish_upperdir_changes(
        &self,
        root: &EphemeralWorkspaceRoot,
        snapshot: &EphemeralSnapshot,
        changes: &[LayerChange],
        path_kinds: &[PathChange],
    ) -> Result<PublishOutcome, EphemeralWorkspaceError> {
        DaemonPublisherPort::new(&self.workspace.root)
            .publish_upperdir_changes(root, snapshot, changes, path_kinds)
    }
}

pub(super) fn finalize_isolated_command_workspace(
    session: &CommandSession,
    workspace: &IsolatedCommandWorkspace,
    runner: Option<&RunResult>,
    status: &str,
    exit_code: i64,
    stdout: &str,
    include_session_id: bool,
) -> Result<Value, DaemonError> {
    let mut outcome = IsolatedWorkspaceOps::new(IsolatedCommandFinalizePort { workspace })
        .finalize_command_workspace(finalize_request(
            session,
            runner,
            status,
            exit_code,
            stdout,
            include_session_id,
        )?)
        .map_err(command_workspace_error)?;
    let audit = outcome
        .metadata
        .get("audit")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(metadata) = outcome.metadata.as_object_mut() {
        metadata.remove("audit");
    }
    let response = command_outcome_response(outcome);
    write_final_response(&workspace.final_path, &response)?;
    crate::services::isolated_workspace::record_tool_call(
        &workspace.handle.agent_id,
        merge_audit_changed_paths(audit, response["changed_paths"].clone()),
    );
    Ok(response)
}

pub(super) fn finalize_command_workspace(
    session: &CommandSession,
    workspace: &EphemeralCommandWorkspace,
    status: &str,
    exit_code: i64,
    stdout: &str,
    include_session_id: bool,
) -> Result<Value, DaemonError> {
    let outcome = EphemeralWorkspaceOps::new(EphemeralCommandFinalizePort { session, workspace })
        .finalize_command_workspace(finalize_request(
            session,
            None,
            status,
            exit_code,
            stdout,
            include_session_id,
        )?)
        .map_err(command_workspace_error)?;
    let response = command_outcome_response(outcome);
    write_final_response(&workspace.dirs.final_path, &response)?;
    Ok(response)
}

fn merge_audit_changed_paths(mut audit: Value, changed_paths: Value) -> Value {
    if let Some(object) = audit.as_object_mut() {
        object.insert("changed_paths".to_owned(), changed_paths);
    }
    audit
}

pub(crate) fn strip_session_id(mut response: Value) -> Value {
    if let Some(object) = response.as_object_mut() {
        object.remove("command_session_id");
    }
    response
}

pub(crate) fn response_with_stdout(mut response: Value, stdout: String) -> Value {
    response["output"]["stdout"] = json!(stdout);
    response["stdout"] = response["output"]["stdout"].clone();
    response
}

pub(crate) fn terminate_command_process_group(pgid: i32) {
    if killpg(Pid::from_raw(pgid), Signal::SIGTERM).is_ok() {
        thread::sleep(Duration::from_millis(50));
        let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
    }
}

pub(crate) fn command_session_write_stdin(args: &Value) -> Result<Value, DaemonError> {
    let id = require_string(args, "command_session_id")?;
    let chars = args
        .get("chars")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let yield_time_ms = optional_u64(args, "yield_time_ms")
        .unwrap_or(command_session_config().default_yield_time_ms);
    let max_tokens = optional_u64(args, "max_output_tokens");
    // sense-2 D7: `terminate` is the explicit teardown channel, decoupled from
    // `\x03` (which is SIGINT/interrupt only).
    let terminate = args
        .get("terminate")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let registry = command_session_registry();
    let Some(session) = registry.get(&id) else {
        // The live session is gone; a reaper-parked completion may remain.
        if let Some(result) = registry.take_completed_result(&id) {
            return Ok(result);
        }
        return Ok(command_session_not_found());
    };
    {
        let mut writer = lock_command_session_state(&session.writer);
        writer.write_all(chars.as_bytes())?;
    }
    // `\x03` interrupts the foreground program (SIGINT) only — teardown is a
    // separate concern (sense-2 D7).
    if chars.contains('\u{3}') {
        *lock_command_session_state(&session.interrupted) = true;
        let _ = killpg(Pid::from_raw(session.pgid), Signal::SIGINT);
    }
    // `terminate: true` tears the session down (SIGTERM→SIGKILL); `wait_for_yield`
    // then finalizes it inline with a `cancelled` status.
    if terminate {
        *lock_command_session_state(&session.cancelled) = true;
        terminate_command_process_group(session.pgid);
    }
    // Unified wait: early-return on completion (inline finalize) or
    // quiet-after-output, capped at `yield_time_ms` (sense-2 §2.3).
    match wait_for_yield(&session, yield_time_ms, max_tokens) {
        WaitOutcome::Completed(result) => Ok(result),
        WaitOutcome::Running(stdout) => Ok(command_result("running", None, &stdout, "", Some(id))),
    }
}

pub(crate) fn command_session_cancel(args: &Value) -> Result<Value, DaemonError> {
    let id = require_string(args, "command_session_id")?;
    let registry = command_session_registry();
    let Some(session) = registry.get(&id) else {
        if let Some(result) = registry.take_completed_result(&id) {
            return Ok(result);
        }
        return Ok(command_session_not_found());
    };
    *lock_command_session_state(&session.cancelled) = true;
    terminate_command_process_group(session.pgid);
    // Finalize inline so the lease/scratch is reclaimed and the cancelled status
    // is stamped; if the child is somehow still alive, the reaper finalizes it.
    match wait_for_yield(
        &session,
        command_session_config().cancel_wait_ms,
        optional_u64(args, "max_output_tokens"),
    ) {
        WaitOutcome::Completed(result) => Ok(result),
        WaitOutcome::Running(stdout) => Ok(command_result("cancelled", None, &stdout, "", None)),
    }
}
