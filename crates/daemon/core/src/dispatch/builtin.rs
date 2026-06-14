//! Exhaustive builtin dispatch: one arm per [`OpRequest`] variant, plus the
//! family→channel map for parse failures and the `DaemonError` envelope
//! boundary conversion.

use operation::{ArgsError, OpRequest};
use protocol::catalog::{BuiltinOp, OpFamily};
use serde_json::json;
use serde_json::Value;

use crate::error::DaemonError;
use crate::op_adapter::{
    checkpoint, command, control, error_envelope, files, isolation, ok_envelope, plugin,
    rejected_fault_envelope, workspace_run,
};
use crate::DispatchContext;

pub(crate) fn dispatch(request: OpRequest, context: DispatchContext<'_>) -> Value {
    match request {
        OpRequest::RuntimeReady(input) => daemon_result(control::op_runtime_ready(input, context)),
        OpRequest::InvocationHeartbeat(input) => ok_envelope(control::op_heartbeat(input, context)),
        OpRequest::InvocationCancel(input) => ok_envelope(control::op_cancel(input, context)),
        OpRequest::InflightCount(input) => ok_envelope(control::op_inflight_count(input, context)),
        OpRequest::TraceExport(input) => ok_envelope(control::op_trace_export(input)),
        OpRequest::TraceExportAck(input) => ok_envelope(control::op_trace_export_ack(input)),
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
        OpRequest::PluginList(input) => daemon_response_result(plugin::op_list(input, context)),
        OpRequest::PluginHealth(input) => daemon_response_result(plugin::op_health(input, context)),
        OpRequest::PyrightLspQuerySymbols(input) => {
            daemon_response_result(plugin::op_pyright_lsp_query_symbols(input, context))
        }
        OpRequest::PyrightLspDefinition(input) => {
            daemon_response_result(plugin::op_pyright_lsp_definition(input, context))
        }
        OpRequest::PyrightLspReferences(input) => {
            daemon_response_result(plugin::op_pyright_lsp_references(input, context))
        }
        OpRequest::PyrightLspDiagnostics(input) => {
            daemon_response_result(plugin::op_pyright_lsp_diagnostics(input, context))
        }
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
            daemon_result(command::op_command_collect_completed(input, context))
        }
        OpRequest::CommandCount(input) => daemon_result(command::op_command_count(input, context)),
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
pub(crate) fn parse_error_response(op: BuiltinOp, error: ArgsError) -> Value {
    match op.contract().family {
        OpFamily::IsolatedWorkspace | OpFamily::WorkspaceRun => rejected_fault_envelope(
            "invalid_argument",
            error.message(),
            json!({"key": error.key}),
        ),
        _ => error_envelope(
            crate::wire::ErrorKind::InvalidRequest,
            format!("invalid request: {}", error.message()),
            json!({"message": error.message()}),
        ),
    }
}

fn daemon_result(result: Result<serde_json::Value, DaemonError>) -> Value {
    match result {
        Ok(value) => ok_envelope(value),
        Err(err) => daemon_error(err),
    }
}

fn daemon_response_result(result: Result<Value, DaemonError>) -> Value {
    match result {
        Ok(response) => response,
        Err(err) => daemon_error(err),
    }
}

fn daemon_error(err: DaemonError) -> Value {
    error_envelope(err.wire_kind(), err.to_string(), json!({}))
}
