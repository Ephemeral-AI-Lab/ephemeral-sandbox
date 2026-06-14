//! Workspace-run cancel adapters. The runtime owns command and isolated
//! workspace teardown; this module only parses args and shapes counts.

use operation::workspace_run::contract::{
    RunCancelAllInput, RunCancelAllOutput, RunEndInput, RunEndOutput,
};
use serde_json::Value;

use crate::error::DaemonError;
use crate::DispatchContext;

use super::to_wire_value;

/// Per-caller teardown; a missing isolated workspace is normal.
pub(crate) fn op_cancel_workspace_runs_by_caller_id(
    input: RunEndInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let caller_id = input.caller.to_string();
    let workspace = &context.require_services()?.workspace;
    let outcome = workspace.cancel_runs_for_caller(&caller_id, input.grace_s);
    Ok(to_wire_value(RunEndOutput {
        success: true,
        caller_id,
        cancelled_commands: outcome.cancelled_commands,
        isolated_exited: outcome.isolated.is_ok(),
    }))
}

/// Whole-sandbox cancel sweep backstop.
pub(crate) fn op_cancel_workspace_runs(
    input: RunCancelAllInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let workspace = &context.require_services()?.workspace;
    let (cancelled_commands, isolated_exited) = workspace.cancel_all_runs(input.grace_s);
    Ok(to_wire_value(RunCancelAllOutput {
        success: true,
        cancelled_commands,
        isolated_callers_exited: isolated_exited,
    }))
}
