//! Workspace-run cancel surface — the caller-keyed teardown coordinator.
//!
//! A workspace run composes a caller's command session(s) (`command_session`,
//! cancel → discard the overlay) with its isolated workspace namespace + lease
//! (`isolated_workspace`, exit → discard the upperdir). Neither substrate ever
//! OCC-publishes on cancel — discard is structural, so the shared LayerStack is
//! persisted only by the request-level commit gate, never by cancellation.
//!
//! This is the daemon half of the §7 cancellation integration:
//! - agent-core calls [`op_cancel_workspace_runs_by_caller_id`] once per
//!   cancelled agent run (`caller_id == agent_run_id`);
//! - the sandbox stage calls [`op_cancel_workspace_runs`] as the whole-sandbox
//!   backstop (the assert-no-leases gate + commit live in the cancellation spec,
//!   not here).

use eos_isolated_workspace::IsolatedError;
use serde_json::{json, Value};

use crate::dispatcher::DispatchContext;
use crate::error::DaemonError;
use crate::services::{command_session, isolated_workspace};

/// Outcome of tearing down one caller's workspace runs.
pub struct CallerCancel {
    /// Command sessions that were live at entry (now cancelled + discarded).
    pub cancelled_sessions: usize,
    /// Isolated-workspace teardown result: the exit response if the caller was
    /// isolated, `Err(IsolatedError::NotOpen)` if it was ephemeral (or had no
    /// isolated workspace), or another `IsolatedError` on teardown failure.
    pub isolated: Result<Value, IsolatedError>,
}

/// Cancel every workspace run owned by `caller_id`: discard its command
/// session(s), then exit its isolated workspace if open. The order matters —
/// sessions are cancelled before the isolated namespace/lease teardown.
pub fn cancel_workspace_runs_by_caller_id(caller_id: &str, grace_s: Option<f64>) -> CallerCancel {
    let cancelled_sessions =
        command_session::cleanup_command_sessions_for_caller(caller_id, grace_s);
    let isolated = isolated_workspace::exit_isolated(caller_id, grace_s);
    CallerCancel {
        cancelled_sessions,
        isolated,
    }
}

/// Cancel every workspace run in the sandbox: discard all command sessions,
/// exit every isolated caller, then reap orphaned namespace/cgroup/scratch
/// resources. Returns the per-substrate counts.
pub fn cancel_all_workspace_runs(grace_s: Option<f64>) -> (usize, usize) {
    let cancelled_sessions = command_session::cancel_all_command_sessions(grace_s);
    let isolated_exited = isolated_workspace::exit_all_and_reap(grace_s);
    (cancelled_sessions, isolated_exited)
}

/// `api.v1.cancel_workspace_runs_by_caller_id` — agent-core's one-RPC per-run
/// teardown. Best-effort: a not-open isolated workspace is normal (the caller
/// was ephemeral) and not surfaced as an error.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_cancel_workspace_runs_by_caller_id(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let caller_id = match require_caller_id(args) {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    let grace_s = args.get("grace_s").and_then(Value::as_f64);
    let outcome = cancel_workspace_runs_by_caller_id(&caller_id, grace_s);
    Ok(json!({
        "success": true,
        "caller_id": caller_id,
        "cancelled_command_sessions": outcome.cancelled_sessions,
        "isolated_exited": outcome.isolated.is_ok(),
    }))
}

/// `api.v1.cancel_workspace_runs` — whole-sandbox cancel sweep backstop.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_cancel_workspace_runs(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let grace_s = args.get("grace_s").and_then(Value::as_f64);
    let (cancelled_sessions, isolated_exited) = cancel_all_workspace_runs(grace_s);
    Ok(json!({
        "success": true,
        "cancelled_command_sessions": cancelled_sessions,
        "isolated_callers_exited": isolated_exited,
    }))
}

fn require_caller_id(args: &Value) -> Result<String, Value> {
    let caller_id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if caller_id.is_empty() {
        return Err(json!({
            "success": false,
            "error": {
                "kind": "invalid_argument",
                "message": "caller_id is required",
                "details": {"key": "caller_id"},
            },
        }));
    }
    Ok(caller_id)
}
