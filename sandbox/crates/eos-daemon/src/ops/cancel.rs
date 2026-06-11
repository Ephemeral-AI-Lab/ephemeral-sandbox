//! Workspace-run cancel adapters. The runtime owns command-session and isolated
//! workspace teardown; this module only parses args and shapes counts.

use serde_json::{json, Value};

use super::require_arg;
use crate::error::DaemonError;
use crate::DispatchContext;

/// Per-caller teardown; a missing isolated workspace is normal.
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

/// Whole-sandbox cancel sweep backstop.
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
