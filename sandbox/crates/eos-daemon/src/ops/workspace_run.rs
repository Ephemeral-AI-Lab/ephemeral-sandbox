//! Workspace-run cancel daemon operation handlers.

use serde_json::Value;

use crate::dispatcher::DispatchContext;
use crate::error::DaemonError;

pub(crate) fn op_cancel_workspace_runs_by_caller_id(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    crate::services::workspace_run::op_cancel_workspace_runs_by_caller_id(args, context)
}

pub(crate) fn op_cancel_workspace_runs(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    crate::services::workspace_run::op_cancel_workspace_runs(args, context)
}
