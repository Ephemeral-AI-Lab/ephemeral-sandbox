//! Isolated-workspace op adapters behind `sandbox.isolation.*`: wire arg
//! parsing and response/error shaping over [`crate::WorkspaceRuntime`].

#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use eos_operation::isolation::contract::{
    IsolationEnterInput, IsolationEnterOutput, IsolationExitInput, IsolationExitOutput,
    IsolationStatusInput, IsolationStatusOutput, ListOpenOutput, TestResetOutput,
};
use eos_operation::{OpError, OpResponse};
use eos_workspace::{IsolatedError, WorkspaceHandle};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::DispatchContext;
use crate::{ExitOutcome, WorkspaceEnterError, WorkspaceRecoveryReport};

use super::to_wire_value;

const TEST_HARNESS_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_HARNESS";

pub(crate) fn op_enter(
    input: IsolationEnterInput,
    context: DispatchContext<'_>,
) -> Result<OpResponse, DaemonError> {
    let caller_id = input.caller.to_string();
    let root = input.layer_stack_root;
    context.record_trace_event(
        "workspace.route",
        "route_selected",
        json!({
            "kind": "isolated_workspace",
            "reason": "isolation_enter_lifecycle",
        }),
    );
    record_enter_started(&context, &caller_id, &root);
    let workspace = &context.require_services()?.workspace;
    match workspace.enter(&caller_id, &root) {
        Ok(handle) => {
            record_entered(&context, &handle);
            Ok(success_response(to_wire_value(IsolationEnterOutput {
                success: true,
                manifest_version: handle.manifest_version,
                manifest_root_hash: handle.manifest_root_hash,
                workspace_handle_id: handle.workspace_id.0,
                workspace_root: handle.workspace_root,
            })))
        }
        Err(WorkspaceEnterError::ActiveCommands { active_commands }) => Ok(refused_response(
            "active_background_work",
            "cannot enter isolated workspace while commands are active",
            json!({"active_commands": active_commands}),
        )),
        Err(WorkspaceEnterError::Isolated(error)) => Ok(error_payload(&error)),
    }
}

pub(crate) fn op_exit(
    input: IsolationExitInput,
    context: DispatchContext<'_>,
) -> Result<OpResponse, DaemonError> {
    let caller_id = input.caller.to_string();
    context.record_trace_event(
        "workspace.route",
        "route_selected",
        json!({
            "kind": "isolated_workspace",
            "reason": "isolation_exit_lifecycle",
        }),
    );
    record_exit_started(&context, &caller_id);
    let workspace = &context.require_services()?.workspace;
    // Exit is the per-caller workspace-run teardown: discard the caller's
    // isolated commands, then tear down its namespace + lease. The
    // isolated exit result carries this op's response shape.
    workspace
        .cancel_runs_for_caller(&caller_id, input.grace_s)
        .isolated
        .map_or_else(
            |error| Ok(error_payload(&error)),
            |exit| {
                record_exited(&context, &exit);
                Ok(success_response(exit_response(exit)))
            },
        )
}

pub(crate) fn op_status(
    input: IsolationStatusInput,
    context: DispatchContext<'_>,
) -> Result<OpResponse, DaemonError> {
    let caller_id = input.caller.to_string();
    let workspace = &context.require_services()?.workspace;
    match workspace.status(&caller_id) {
        Ok(Some(handle)) => {
            record_status_read(&context, &caller_id, Some(&handle), None);
            Ok(success_response(status_response(&handle)))
        }
        Ok(None) => {
            record_status_read(&context, &caller_id, None, None);
            Ok(success_response(to_wire_value(
                IsolationStatusOutput::Closed {
                    success: true,
                    open: false,
                },
            )))
        }
        Err(error) => {
            record_status_read(&context, &caller_id, None, Some(error.kind()));
            Ok(error_payload(&error))
        }
    }
}

pub(crate) fn op_list_open(context: DispatchContext<'_>) -> Result<OpResponse, DaemonError> {
    let workspace = &context.require_services()?.workspace;
    Ok(success_response(to_wire_value(ListOpenOutput {
        success: true,
        open_caller_ids: workspace.list_open(),
    })))
}

pub(crate) fn op_test_reset(context: DispatchContext<'_>) -> Result<OpResponse, DaemonError> {
    if !env_true(TEST_HARNESS_ENV) {
        return Ok(refused_response(
            "forbidden",
            "sandbox.isolation.test_reset requires EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true",
            json!({}),
        ));
    }
    let workspace = &context.require_services()?.workspace;
    record_recovery_started(&context);
    let recovery = workspace.test_reset_report();
    record_recovery_finished(&context, &recovery);
    Ok(success_response(to_wire_value(TestResetOutput {
        success: true,
        reset: true,
        exited_callers: recovery.exited_callers,
    })))
}

fn status_response(handle: &WorkspaceHandle) -> Value {
    to_wire_value(IsolationStatusOutput::Open {
        success: true,
        open: true,
        manifest_version: handle.manifest_version,
        manifest_root_hash: handle.manifest_root_hash.clone(),
        workspace_root: handle.workspace_root.clone(),
        created_at: handle.created_at,
        last_activity: handle.last_activity,
    })
}

/// Shape the stable exit response, splicing the lease custody fields into the
/// teardown inspection.
pub(crate) fn exit_response(exit: ExitOutcome) -> Value {
    let outcome = exit.isolated;
    let mut inspection = outcome.inspection;
    if let Some(object) = inspection.as_object_mut() {
        object.insert("lease_released".to_owned(), json!(exit.lease_released));
        object.insert(
            "active_leases_after".to_owned(),
            json!(exit.active_leases_after),
        );
    }
    to_wire_value(IsolationExitOutput {
        success: true,
        evicted_upperdir_bytes: outcome.evicted_upperdir_bytes,
        lifetime_s: outcome.lifetime_s,
        total_ms: outcome.total_ms,
        phases_ms: to_wire_value(outcome.phases_ms),
        inspection,
    })
}

fn record_enter_started(context: &DispatchContext<'_>, caller_id: &str, root: &std::path::Path) {
    context.record_trace_event(
        "isolated_workspace",
        "enter_started",
        json!({
            "caller_id": caller_id,
            "layer_stack_root": root.display().to_string(),
        }),
    );
}

fn record_entered(context: &DispatchContext<'_>, handle: &WorkspaceHandle) {
    let common = json!({
        "caller_id": handle.caller_id.as_str(),
        "workspace_handle_id": handle.workspace_id.0.as_str(),
        "holder_pid": handle.holder_pid,
    });
    context.record_trace_event("isolated_workspace", "holder_started", common);
    context.record_trace_event(
        "isolated_workspace",
        "network_configured",
        json!({
            "caller_id": handle.caller_id.as_str(),
            "workspace_handle_id": handle.workspace_id.0.as_str(),
            "dns_fallback_applied": handle.dns_configuration.fallback_applied,
            "previous_first_nameserver": handle
                .dns_configuration
                .previous_first_nameserver
                .as_deref(),
            "veth_host_name": handle.veth.as_ref().map(|veth| veth.host_name.as_str()),
            "veth_ns_name": handle.veth.as_ref().map(|veth| veth.ns_name.as_str()),
        }),
    );
}

fn record_status_read(
    context: &DispatchContext<'_>,
    caller_id: &str,
    handle: Option<&WorkspaceHandle>,
    error_kind: Option<&str>,
) {
    context.record_trace_event(
        "isolated_workspace",
        "status_read",
        json!({
            "caller_id": caller_id,
            "open": handle.is_some(),
            "workspace_handle_id": handle.map(|handle| handle.workspace_id.0.as_str()),
            "error_kind": error_kind,
        }),
    );
}

fn record_exit_started(context: &DispatchContext<'_>, caller_id: &str) {
    context.record_trace_event(
        "isolated_workspace",
        "exit_started",
        json!({
            "caller_id": caller_id,
        }),
    );
}

fn record_recovery_started(context: &DispatchContext<'_>) {
    context.record_trace_event("isolated_workspace", "recovery_started", json!({}));
}

fn record_recovery_finished(context: &DispatchContext<'_>, recovery: &WorkspaceRecoveryReport) {
    context.record_trace_event(
        "isolated_workspace",
        "recovery_finished",
        json!({
            "exited_caller_count": recovery.exited_callers.len(),
            "exited_callers": recovery.exited_callers.clone(),
            "manager_json_error": recovery.manager_json_error.as_deref(),
            "orphan_cleanup_error": recovery.orphan_cleanup_error.as_deref(),
        }),
    );
}

fn record_exited(context: &DispatchContext<'_>, exit: &ExitOutcome) {
    for (phase, duration_ms) in sorted_phases(&exit.isolated.phases_ms) {
        context.record_trace_event(
            "isolated_workspace",
            "teardown_phase_finished",
            teardown_phase_details(phase, duration_ms, &exit.isolated.inspection),
        );
    }
    context.record_trace_event(
        "isolated_workspace",
        "exited",
        json!({
            "caller_id": exit.isolated.caller_id.as_str(),
            "workspace_handle_id": exit.isolated.workspace_id.0.as_str(),
            "lifetime_s": exit.isolated.lifetime_s,
            "total_ms": exit.isolated.total_ms,
            "evicted_upperdir_bytes": exit.isolated.evicted_upperdir_bytes,
            "lease_released": exit.lease_released,
            "active_leases_after": exit.active_leases_after,
            "mountinfo_scan_error": exit
                .isolated
                .inspection
                .get("mountinfo_reference_count_after")
                .is_none_or(Value::is_null),
        }),
    );
}

fn sorted_phases(phases: &std::collections::HashMap<String, f64>) -> Vec<(&str, f64)> {
    let mut phases = phases
        .iter()
        .map(|(phase, duration_ms)| (phase.as_str(), *duration_ms))
        .collect::<Vec<_>>();
    phases.sort_by_key(|(phase, _)| *phase);
    phases
}

fn teardown_phase_details(phase: &str, duration_ms: f64, inspection: &Value) -> Value {
    let mut details = json!({
        "phase": phase,
        "duration_ms": duration_ms,
    });
    if phase == "kill_holder" {
        if let Some(object) = details.as_object_mut() {
            object.insert(
                "holder_was_alive".to_owned(),
                json!(inspection
                    .get("holder_pid")
                    .and_then(Value::as_i64)
                    .is_some_and(|holder_pid| holder_pid > 0)),
            );
            object.insert(
                "holder_kill_error".to_owned(),
                inspection
                    .get("holder_kill_error")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
        }
    }
    details
}

/// Serialize tests that toggle the process-wide
/// `EOS_ISOLATED_WORKSPACE_TEST_HARNESS` environment variable.
#[cfg(test)]
pub(crate) fn lock_isolated_test_state() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Map an [`IsolatedError`] onto the structured error payload, carrying the
/// variant-specific detail fields.
fn success_response(value: Value) -> OpResponse {
    OpResponse::Success(value)
}

fn refused_response(kind: &'static str, message: impl Into<String>, details: Value) -> OpResponse {
    OpResponse::Refused(OpError {
        kind,
        message: message.into(),
        details: Some(details),
    })
}

fn error_payload(error: &IsolatedError) -> OpResponse {
    let details = match error {
        IsolatedError::AlreadyOpen {
            created_at,
            last_activity,
        } => json!({
            "created_at": created_at,
            "last_activity": last_activity,
        }),
        IsolatedError::QuotaExceeded { total_cap } => json!({
            "total_cap": total_cap,
        }),
        IsolatedError::HostRamPressure {
            required_bytes,
            budget_bytes,
        } => json!({
            "required_bytes": required_bytes,
            "budget_bytes": budget_bytes,
        }),
        IsolatedError::SetupFailed { step } => json!({
            "failed_step": step,
        }),
        _ => json!({}),
    };
    refused_response(error.kind(), error.to_string(), details)
}

fn env_true(key: &str) -> bool {
    std::env::var(key)
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("true")
}

#[cfg(test)]
#[path = "../../tests/unit/isolated_workspace/service.rs"]
mod tests;
