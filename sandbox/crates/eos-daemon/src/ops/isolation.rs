//! Daemon-owned isolated-workspace lifecycle routing.
//!
//! This module is the first Rust lifecycle slice behind
//! `api.isolated_workspace.*`: it owns the dispatch entry points for one
//! daemon-local isolated runtime session. State construction and namespace
//! runtime details live in `crate::services::workspace`.

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use eos_command_ops::CommandBinding;
use eos_isolated_workspace::WorkspaceHandle;
use eos_isolated_workspace::{ExitOutcome, IsolatedError};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::runtime::context::DispatchContext;

use super::{error_json, require_arg};
#[cfg(test)]
pub(crate) use crate::services::workspace::configure_isolated_workspace;
#[cfg(test)]
use crate::services::workspace::default_isolated_workspace_config;
use crate::services::workspace::{
    ensure_state, lock_state_cell, reset_test_manager_file, with_state, DaemonIsolatedState,
};

const TEST_HARNESS_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_HARNESS";

// Dispatcher op handlers share the `Result<Value, DaemonError>` ABI even when
// isolated-workspace failures are represented as structured JSON responses.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub(crate) fn op_enter(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    let root = match require_arg(args, "layer_stack_root") {
        Ok(root) => PathBuf::from(root),
        Err(error) => return Ok(error),
    };
    let active_command_sessions = eos_command_ops::active_command_sessions_for_caller(&caller_id);
    if active_command_sessions > 0 {
        return Ok(error_json(
            "active_background_work",
            "cannot enter isolated workspace while command sessions are active",
            json!({"active_command_sessions": active_command_sessions}),
        ));
    }
    match ensure_state(&root).and_then(|()| {
        with_state(|state| {
            let snapshot = state.acquire_snapshot(&caller_id)?;
            let lease_id = snapshot.lease_id.clone();
            match state.manager.enter(&caller_id, snapshot) {
                Ok(handle) => Ok(handle),
                Err(error) => {
                    let _ = state.release_lease(&lease_id);
                    Err(error)
                }
            }
        })
    }) {
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

pub(crate) fn op_exit(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    let grace_s = args.get("grace_s").and_then(Value::as_f64);
    // Exit is the per-caller workspace-run teardown: discard the caller's
    // isolated command sessions, then tear down its namespace + lease. The
    // isolated exit result carries this op's response shape.
    crate::ops::cancel::cancel_workspace_runs_by_caller_id(&caller_id, grace_s)
        .isolated
        .map_or_else(|error| Ok(error_payload(&error)), Ok)
}

// Dispatcher op handlers share the fallible ABI even though status misses are
// represented as `{success: true, open: false}`.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub(crate) fn op_status(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    match with_state(|state| Ok(state.manager.get_handle(&caller_id))) {
        Ok(Some(handle)) => Ok(json!({
            "success": true,
            "open": true,
            "manifest_version": handle.manifest_version,
            "manifest_root_hash": handle.manifest_root_hash,
            "workspace_root": handle.workspace_root,
            "created_at": handle.created_at,
            "last_activity": handle.last_activity,
        })),
        Ok(None) => Ok(json!({"success": true, "open": false})),
        Err(error) => Ok(error_payload(&error)),
    }
}

// Dispatcher op handlers share the fallible ABI even though disabled state is
// represented as an empty open-caller list.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub(crate) fn op_list_open(
    _args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    match with_state(|state| Ok(state.manager.list_open_callers())) {
        Ok(open_caller_ids) => Ok(json!({"success": true, "open_caller_ids": open_caller_ids})),
        Err(IsolatedError::FeatureDisabled) => Ok(json!({"success": true, "open_caller_ids": []})),
        Err(error) => Ok(error_payload(&error)),
    }
}

// Dispatcher op handlers share the fallible ABI even though harness gating is
// represented as a structured JSON error.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub(crate) fn op_test_reset(
    _args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    if !env_true(TEST_HARNESS_ENV) {
        return Ok(error_json(
            "forbidden",
            "sandbox.isolation.test_reset requires EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true",
            json!({}),
        ));
    }
    let exited_callers = {
        let mut guard = lock_state_cell();
        let exited_callers = if let Some(state) = guard.as_mut() {
            let callers = state.manager.list_open_callers();
            for caller_id in &callers {
                if let Ok(outcome) = state.manager.exit(caller_id, Some(0.0)) {
                    let _ = state.release_lease(&outcome.lease_id);
                }
            }
            state.manager.reap_orphan_resources();
            callers
        } else {
            Vec::new()
        };
        *guard = None;
        exited_callers
    };
    reset_test_manager_file();
    Ok(json!({"success": true, "reset": true, "exited_callers": exited_callers}))
}

pub(crate) fn command_handle_for_args(args: &Value) -> Option<CommandBinding> {
    let caller_id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .trim()
        .to_owned();
    if caller_id.is_empty() {
        return None;
    }
    let (layer_stack_root, handle) = {
        let guard = lock_state_cell();
        guard.as_ref().and_then(|state| {
            state
                .manager
                .get_handle(&caller_id)
                .map(|handle| (state.layer_stack_root.clone(), handle))
        })
    }?;
    Some(command_handle_from(&layer_stack_root, handle))
}

fn command_handle_from(layer_stack_root: &Path, handle: WorkspaceHandle) -> CommandBinding {
    CommandBinding {
        caller_id: handle.caller_id,
        workspace_handle_id: handle.workspace_id.0,
        layer_stack_root: layer_stack_root.to_path_buf(),
        manifest_version: handle.manifest_version,
        manifest_root_hash: handle.manifest_root_hash,
        workspace_root: PathBuf::from(handle.workspace_root),
        scratch_dir: handle.scratch_dir,
        upperdir: handle.upperdir,
        workdir: handle.workdir,
        layer_paths: handle.layer_paths,
        ns_fds: handle.ns_fds,
        cgroup_path: handle.cgroup_path,
    }
}

pub(crate) fn caller_has_active_handle(caller_id: &str) -> bool {
    let caller_id = caller_id.trim();
    if caller_id.is_empty() {
        return false;
    }
    let guard = lock_state_cell();
    guard
        .as_ref()
        .and_then(|state| state.manager.get_handle(caller_id))
        .is_some()
}

/// Tear down `caller_id`'s isolated workspace if open: namespace/network/cgroup,
/// release the lease, discard the upperdir (never published). The single
/// isolated-teardown primitive shared by `op_exit` and the workspace-run cancel
/// surface. Returns `Err(IsolatedError::NotOpen)` when the caller is not
/// isolated (the cancel surface treats that as a no-op).
pub(crate) fn exit_isolated(caller_id: &str, grace_s: Option<f64>) -> Result<Value, IsolatedError> {
    with_state(|state| {
        let outcome = state.manager.exit(caller_id, grace_s)?;
        Ok(exit_response(state, outcome))
    })
}

/// Release the exited workspace's lease and shape the stable exit response,
/// splicing the lease fields into the teardown inspection.
fn exit_response(state: &mut DaemonIsolatedState, outcome: ExitOutcome) -> Value {
    let lease_released = state.release_lease(&outcome.lease_id);
    let active_leases_after = state.active_lease_count();
    let mut inspection = outcome.inspection;
    if let Some(object) = inspection.as_object_mut() {
        object.insert("lease_released".to_owned(), json!(lease_released));
        object.insert("active_leases_after".to_owned(), json!(active_leases_after));
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

/// Exit every open isolated workspace and reap orphaned resources (the
/// whole-sandbox cancel sweep). Returns the number of callers exited.
pub(crate) fn exit_all_and_reap(grace_s: Option<f64>) -> usize {
    let mut guard = lock_state_cell();
    let Some(state) = guard.as_mut() else {
        return 0;
    };
    let callers = state.manager.list_open_callers();
    for caller in &callers {
        if let Ok(outcome) = state.manager.exit(caller, grace_s) {
            let _ = state.release_lease(&outcome.lease_id);
        }
    }
    state.manager.reap_orphan_resources();
    callers.len()
}

pub(crate) fn ttl_sweep() -> usize {
    let mut guard = lock_state_cell();
    let Some(state) = guard.as_mut() else {
        return 0;
    };
    // Protect callers that still own at least one live command session. The
    // command-session registry is the authority for this now that the isolated
    // side-map is gone (lock order: isolated state -> command-session registry).
    let active_callers = state
        .manager
        .list_open_callers()
        .into_iter()
        .filter(|caller| eos_command_ops::active_command_sessions_for_caller(caller) > 0)
        .collect::<HashSet<_>>();
    let evicted = state.manager.ttl_sweep(&active_callers);
    let count = evicted.len();
    for outcome in evicted {
        let _ = state.release_lease(&outcome.lease_id);
    }
    count
}

/// Bump the caller's isolated-workspace TTL liveness (file/command activity).
pub(crate) fn touch_isolated(caller_id: &str) {
    let mut guard = lock_state_cell();
    if let Some(state) = guard.as_mut() {
        state.manager.touch(caller_id);
    }
}

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
