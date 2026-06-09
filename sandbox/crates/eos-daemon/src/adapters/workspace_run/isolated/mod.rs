//! Daemon-owned isolated-workspace lifecycle routing.
//!
//! This module is the first Rust lifecycle slice behind
//! `api.isolated_workspace.*`: it owns the dispatch entry points for one
//! daemon-local `eos-isolated-workspace` session. State construction, structured
//! error payloads, and namespace runtime details live in child modules so this
//! file stays a routing surface.

mod errors;
#[cfg(target_os = "linux")]
mod ns_runner;
mod runtime;
mod state;

use std::collections::HashSet;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use eos_workspace_modes::isolated::{CallerId, IsolatedError};
#[cfg(target_os = "linux")]
pub(crate) use eos_workspace_run::CommandHandle;
use serde_json::{json, Value};

use crate::adapters::workspace_run;
use crate::dispatcher::DispatchContext;
use crate::error::DaemonError;

use errors::{
    env_true, error_json, error_payload, require_arg, setup_error, test_runtime_stub_enabled,
};
#[cfg(target_os = "linux")]
use runtime::command_handle_from;
use state::{ensure_state, lock_state_cell, reset_test_manager_file, with_state};
#[cfg(test)]
use state::default_isolated_workspace_config;
pub(crate) use state::configure_isolated_workspace;

const TEST_HARNESS_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_HARNESS";

// Dispatcher op handlers share the `Result<Value, DaemonError>` ABI even when
// isolated-workspace failures are represented as structured JSON responses.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_enter(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    let root = match require_arg(args, "layer_stack_root") {
        Ok(root) => PathBuf::from(root),
        Err(error) => return Ok(error),
    };
    let active_command_sessions = workspace_run::active_command_sessions_for_caller(&caller_id);
    if active_command_sessions > 0 {
        return Ok(error_json(
            "active_background_work",
            "cannot enter isolated workspace while command sessions are active",
            json!({"active_command_sessions": active_command_sessions}),
        ));
    }
    match ensure_state(&root)
        .and_then(|()| with_state(|state| state.session.enter(&CallerId(caller_id))))
    {
        Ok(handle) => Ok(json!({
            "success": true,
            "manifest_version": handle.manifest_version,
            "manifest_root_hash": handle.manifest_root_hash,
            "workspace_handle_id": handle.workspace_handle_id.0,
            "workspace_root": handle.workspace_root,
        })),
        Err(error) => Ok(error_payload(&error)),
    }
}

pub fn op_exit(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    let grace_s = args.get("grace_s").and_then(Value::as_f64);
    // Exit is the per-caller workspace-run teardown: discard the caller's
    // isolated command sessions, then tear down its namespace + lease. The
    // isolated exit result carries this op's response shape.
    crate::adapters::workspace_run::cancel_workspace_runs_by_caller_id(&caller_id, grace_s)
        .isolated
        .map_or_else(|error| Ok(error_payload(&error)), Ok)
}

// Dispatcher op handlers share the fallible ABI even though status misses are
// represented as `{success: true, open: false}`.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_status(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    match with_state(|state| Ok(state.session.get_handle(&CallerId(caller_id)))) {
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
pub fn op_list_open(_args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    match with_state(|state| Ok(state.session.list_open_callers())) {
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
pub fn op_test_reset(_args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    if !env_true(TEST_HARNESS_ENV) {
        return Ok(error_json(
            "forbidden",
            "api.isolated_workspace.test_reset requires EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true",
            json!({}),
        ));
    }
    let exited_callers = {
        let mut guard = lock_state_cell();
        let exited_callers = if let Some(state) = guard.as_mut() {
            let callers = state.session.list_open_callers();
            for caller_id in &callers {
                let _ = state.session.exit(&CallerId(caller_id.clone()), Some(0.0));
            }
            state.session.reap_orphan_resources();
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

#[cfg(target_os = "linux")]
pub fn command_handle_for_args(args: &Value) -> Option<CommandHandle> {
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
                .session
                .get_handle(&CallerId(caller_id))
                .map(|handle| (state.layer_stack_root.clone(), handle))
        })
    }?;
    Some(command_handle_from(&layer_stack_root, handle))
}

pub fn caller_has_active_handle(caller_id: &str) -> bool {
    let caller_id = caller_id.trim();
    if caller_id.is_empty() {
        return false;
    }
    let guard = lock_state_cell();
    guard
        .as_ref()
        .and_then(|state| state.session.get_handle(&CallerId(caller_id.to_owned())))
        .is_some()
}

/// Tear down `caller_id`'s isolated workspace if open: namespace/network/cgroup,
/// release the lease, discard the upperdir (never published). The single
/// isolated-teardown primitive shared by `op_exit` and the workspace-run cancel
/// surface. Returns `Err(IsolatedError::NotOpen)` when the caller is not
/// isolated (the cancel surface treats that as a no-op).
pub fn exit_isolated(caller_id: &str, grace_s: Option<f64>) -> Result<Value, IsolatedError> {
    with_state(|state| state.session.exit(&CallerId(caller_id.to_owned()), grace_s))
}

/// Exit every open isolated workspace and reap orphaned resources (the
/// whole-sandbox cancel sweep). Returns the number of callers exited.
pub fn exit_all_and_reap(grace_s: Option<f64>) -> usize {
    let mut guard = lock_state_cell();
    let Some(state) = guard.as_mut() else {
        return 0;
    };
    let callers = state.session.list_open_callers();
    for caller in &callers {
        let _ = state.session.exit(&CallerId(caller.clone()), grace_s);
    }
    state.session.reap_orphan_resources();
    callers.len()
}

pub fn ttl_sweep() -> usize {
    let mut guard = lock_state_cell();
    let Some(state) = guard.as_mut() else {
        return 0;
    };
    // Protect callers that still own at least one live command session. The
    // command-session registry is the authority for this now that the isolated
    // side-map is gone (lock order: isolated state -> command-session registry).
    let active_callers = state
        .session
        .list_open_callers()
        .into_iter()
        .filter(|caller| workspace_run::active_command_sessions_for_caller(caller) > 0)
        .collect::<HashSet<_>>();
    state.session.ttl_sweep(&active_callers)
}

#[cfg(target_os = "linux")]
pub fn record_tool_call(caller_id: &str, payload: Value) {
    let mut guard = lock_state_cell();
    if let Some(state) = guard.as_mut() {
        state
            .session
            .record_tool_call(&CallerId(caller_id.to_owned()), payload);
    }
}

#[cfg(test)]
pub(crate) fn lock_isolated_test_state() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "../../../../tests/isolated_workspace/service.rs"]
mod tests;
