//! Isolated-workspace op adapters behind `api.isolated_workspace.*`: wire arg
//! parsing, the command-session entry gate, and response/error shaping over
//! [`eos_workspace_runtime::WorkspaceRuntime`].

use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use eos_isolated_workspace::{IsolatedError, WorkspaceHandle};
use eos_workspace_runtime::ExitOutcome;
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::runtime::context::DispatchContext;

use super::{error_json, require_arg};

const TEST_HARNESS_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_HARNESS";

pub(crate) fn op_enter(args: &Value, context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    let root = match require_arg(args, "layer_stack_root") {
        Ok(root) => PathBuf::from(root),
        Err(error) => return Ok(error),
    };
    let workspace = &context.require_services()?.workspace;
    // Cross-domain entry gate: a caller with live command sessions cannot
    // switch its runs into an isolated workspace mid-flight.
    let active_command_sessions = eos_command_ops::active_command_sessions_for_caller(&caller_id);
    if active_command_sessions > 0 {
        return Ok(error_json(
            "active_background_work",
            "cannot enter isolated workspace while command sessions are active",
            json!({"active_command_sessions": active_command_sessions}),
        ));
    }
    match workspace.enter(&caller_id, &root) {
        Ok(handle) => Ok(json!({
            "success": true,
            "manifest_version": handle.manifest_version,
            "manifest_root_hash": handle.manifest_root_hash,
            "workspace_handle_id": handle.workspace_id.0,
            "workspace_root": handle.workspace_root,
        })),
        Err(error) => Ok(error_payload(&error)),
    }
}

pub(crate) fn op_exit(args: &Value, context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    let grace_s = args.get("grace_s").and_then(Value::as_f64);
    let workspace = &context.require_services()?.workspace;
    // Exit is the per-caller workspace-run teardown: discard the caller's
    // isolated command sessions, then tear down its namespace + lease. The
    // isolated exit result carries this op's response shape.
    workspace
        .cancel_runs_for_caller(&caller_id, grace_s)
        .isolated
        .map_or_else(|error| Ok(error_payload(&error)), |exit| Ok(exit_response(exit)))
}

pub(crate) fn op_status(args: &Value, context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    let workspace = &context.require_services()?.workspace;
    match workspace.status(&caller_id) {
        Ok(Some(handle)) => Ok(status_response(&handle)),
        Ok(None) => Ok(json!({"success": true, "open": false})),
        Err(error) => Ok(error_payload(&error)),
    }
}

pub(crate) fn op_list_open(
    _args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let workspace = &context.require_services()?.workspace;
    Ok(json!({"success": true, "open_caller_ids": workspace.list_open()}))
}

pub(crate) fn op_test_reset(
    _args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    if !env_true(TEST_HARNESS_ENV) {
        return Ok(error_json(
            "forbidden",
            "sandbox.isolation.test_reset requires EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true",
            json!({}),
        ));
    }
    let workspace = &context.require_services()?.workspace;
    let exited_callers = workspace.test_reset();
    Ok(json!({"success": true, "reset": true, "exited_callers": exited_callers}))
}

fn status_response(handle: &WorkspaceHandle) -> Value {
    json!({
        "success": true,
        "open": true,
        "manifest_version": handle.manifest_version,
        "manifest_root_hash": handle.manifest_root_hash,
        "workspace_root": handle.workspace_root,
        "created_at": handle.created_at,
        "last_activity": handle.last_activity,
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
    json!({
        "success": true,
        "evicted_upperdir_bytes": outcome.evicted_upperdir_bytes,
        "lifetime_s": outcome.lifetime_s,
        "total_ms": outcome.total_ms,
        "phases_ms": outcome.phases_ms,
        "inspection": inspection,
    })
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
fn error_payload(error: &IsolatedError) -> Value {
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
    error_json(error.kind(), error.to_string(), details)
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
