//! Workspace-run cancel op adapters.
//!
//! A workspace run composes a caller's command session(s) (`command_session`,
//! cancel → discard the overlay) with its isolated workspace namespace + lease
//! (`isolated_workspace`, exit → discard the upperdir). Neither substrate ever
//! OCC-publishes on cancel — discard is structural, so the shared LayerStack is
//! persisted only by the request-level commit gate, never by cancellation. The
//! coordinator lives on `WorkspaceRuntime`; these adapters parse args and shape
//! the wire counts.
//!
//! This is the daemon half of the §7 cancellation integration:
//! - agent-core calls [`op_cancel_workspace_runs_by_caller_id`] once per
//!   cancelled agent run (`caller_id == agent_run_id`);
//! - the sandbox stage calls [`op_cancel_workspace_runs`] as the whole-sandbox
//!   backstop (the assert-no-leases gate + commit live in the cancellation spec,
//!   not here).

use serde_json::{json, Value};

use super::require_arg;
use crate::error::DaemonError;
use crate::runtime::context::DispatchContext;

/// `api.v1.cancel_workspace_runs_by_caller_id` — agent-core's one-RPC per-run
/// teardown. Best-effort: a not-open isolated workspace is normal (the caller
/// was ephemeral) and not surfaced as an error.
pub(crate) fn op_cancel_workspace_runs_by_caller_id(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let caller_id = match require_arg(args, "caller_id") {
        Ok(caller_id) => caller_id,
        Err(error) => return Ok(error),
    };
    let grace_s = args.get("grace_s").and_then(Value::as_f64);
    let workspace = &context.require_services()?.workspace;
    let outcome = workspace.cancel_runs_for_caller(&caller_id, grace_s);
    Ok(json!({
        "success": true,
        "caller_id": caller_id,
        "cancelled_command_sessions": outcome.cancelled_sessions,
        "isolated_exited": outcome.isolated.is_ok(),
    }))
}

/// `api.v1.cancel_workspace_runs` — whole-sandbox cancel sweep backstop.
pub(crate) fn op_cancel_workspace_runs(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let grace_s = args.get("grace_s").and_then(Value::as_f64);
    let workspace = &context.require_services()?.workspace;
    let (cancelled_sessions, isolated_exited) = workspace.cancel_all_runs(grace_s);
    Ok(json!({
        "success": true,
        "cancelled_command_sessions": cancelled_sessions,
        "isolated_callers_exited": isolated_exited,
    }))
}
