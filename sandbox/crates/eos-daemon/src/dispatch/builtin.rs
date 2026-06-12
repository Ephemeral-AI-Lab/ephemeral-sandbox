//! Exhaustive builtin dispatch: one arm per [`OpRequest`] variant, plus the
//! family→channel map for parse failures and the `DaemonError` →
//! [`OpResponseError`] boundary conversion.

use eos_operation::core::catalog::{BuiltinOp, OpFamily};
use eos_operation::{
    ArgsError, OpError, OpRequest, OpResponse, OpResponseError, OpResponseErrorKind,
};
use serde_json::json;

use crate::error::DaemonError;
use crate::op_adapter::{checkpoint, command, control, files, isolation, plugin, workspace_run};
use crate::wire::ErrorKind;
use crate::DispatchContext;

pub(crate) fn dispatch(request: OpRequest, context: DispatchContext<'_>) -> OpResponse {
    match request {
        OpRequest::RuntimeReady(input) => daemon_result(control::op_runtime_ready(input, context)),
        OpRequest::InvocationHeartbeat(input) => {
            OpResponse::Success(control::op_heartbeat(input, context))
        }
        OpRequest::InvocationCancel(input) => {
            OpResponse::Success(control::op_cancel(input, context))
        }
        OpRequest::InflightCount(input) => {
            OpResponse::Success(control::op_inflight_count(input, context))
        }
        OpRequest::TraceExport(input) => OpResponse::Success(control::op_trace_export(input)),
        OpRequest::LayerMetrics(input) => daemon_result(checkpoint::layer_metrics(input, context)),
        OpRequest::EnsureWorkspaceBase(input) => {
            daemon_result(checkpoint::ensure_workspace_base(input, context))
        }
        OpRequest::BuildWorkspaceBase(input) => {
            daemon_result(checkpoint::build_workspace_base(input, context))
        }
        OpRequest::CommitToWorkspace(input) => {
            daemon_result(checkpoint::commit_to_workspace(input, context))
        }
        OpRequest::CommitToGit(input) => daemon_result(checkpoint::commit_to_git(input, context)),
        OpRequest::WorkspaceBinding(input) => {
            daemon_result(checkpoint::workspace_binding(input, context))
        }
        OpRequest::ReadFile(input) => daemon_result(files::op_read_file(input, context)),
        OpRequest::WriteFile(input) => daemon_result(files::op_write_file(input, context)),
        OpRequest::EditFile(input) => daemon_result(files::op_edit_file(input, context)),
        OpRequest::PluginEnsure(input) => daemon_result(plugin::op_ensure(*input, context)),
        OpRequest::PluginStatus(input) => daemon_result(plugin::op_status(input, context)),
        OpRequest::IsolatedWorkspaceEnter(input) => {
            daemon_response_result(isolation::op_enter(input, context))
        }
        OpRequest::IsolatedWorkspaceExit(input) => {
            daemon_response_result(isolation::op_exit(input, context))
        }
        OpRequest::IsolatedWorkspaceStatus(input) => {
            daemon_response_result(isolation::op_status(input, context))
        }
        OpRequest::IsolatedWorkspaceListOpen => {
            daemon_response_result(isolation::op_list_open(context))
        }
        OpRequest::IsolatedWorkspaceTestReset => {
            daemon_response_result(isolation::op_test_reset(context))
        }
        OpRequest::ExecCommand(input) => daemon_result(command::op_exec_command(input, context)),
        OpRequest::WriteStdin(input) => daemon_result(command::command_write_stdin(input, context)),
        OpRequest::CommandReadProgress(input) => {
            daemon_result(command::command_read_progress(input, context))
        }
        OpRequest::CommandCancel(input) => daemon_result(command::command_cancel(input, context)),
        OpRequest::CommandCollectCompleted(input) => {
            OpResponse::Success(command::op_command_collect_completed(input, context))
        }
        OpRequest::CommandCount(input) => {
            OpResponse::Success(command::op_command_count(input, context))
        }
        OpRequest::CancelWorkspaceRunsByCaller(input) => daemon_result(
            workspace_run::op_cancel_workspace_runs_by_caller_id(input, context),
        ),
        OpRequest::CancelWorkspaceRuns(input) => {
            daemon_result(workspace_run::op_cancel_workspace_runs(input, context))
        }
    }
}

/// The per-family parse-failure channel: workspace families refuse in-band,
/// every other family answers with a structured `invalid_request` error
/// response.
pub(crate) fn parse_error_response(op: BuiltinOp, error: ArgsError) -> OpResponse {
    match op.contract().family {
        OpFamily::IsolatedWorkspace | OpFamily::WorkspaceRun => OpResponse::Refused(OpError {
            kind: "invalid_argument",
            message: error.message(),
            details: Some(json!({"key": error.key})),
        }),
        _ => OpResponse::Error(OpResponseError::invalid_request(format!(
            "invalid request: {}",
            error.message()
        ))),
    }
}

fn daemon_result(result: Result<serde_json::Value, DaemonError>) -> OpResponse {
    match result {
        Ok(value) => OpResponse::Success(value),
        Err(err) => daemon_error(err),
    }
}

fn daemon_response_result(result: Result<OpResponse, DaemonError>) -> OpResponse {
    match result {
        Ok(response) => response,
        Err(err) => daemon_error(err),
    }
}

fn daemon_error(err: DaemonError) -> OpResponse {
    OpResponse::Error(OpResponseError::new(
        response_error_kind(err.wire_kind()),
        err.to_string(),
        json!({}),
    ))
}

fn response_error_kind(kind: ErrorKind) -> OpResponseErrorKind {
    match kind {
        ErrorKind::InvalidRequest => OpResponseErrorKind::InvalidRequest,
        ErrorKind::BadJson => OpResponseErrorKind::BadJson,
        ErrorKind::RequestTooLarge => OpResponseErrorKind::RequestTooLarge,
        ErrorKind::Unauthorized => OpResponseErrorKind::Unauthorized,
        ErrorKind::UnknownOp => OpResponseErrorKind::UnknownOp,
        ErrorKind::InternalError => OpResponseErrorKind::InternalError,
        ErrorKind::Forbidden => OpResponseErrorKind::Forbidden,
        ErrorKind::ForbiddenInIsolatedWorkspace => {
            OpResponseErrorKind::ForbiddenInIsolatedWorkspace
        }
        ErrorKind::LifecycleInProgress => OpResponseErrorKind::LifecycleInProgress,
    }
}
