use serde_json::{json, Value};

use crate::command::{
    CommandOutput, CommandServiceError, CommandStatus, ExecCommandInput, ReadCommandLinesInput,
    WriteCommandStdinInput,
};
use crate::operations::dispatch::OperationEntry;
use crate::workspace_crate::WorkspaceSessionId;
use crate::SandboxRuntimeOperations;
use sandbox_operation_catalog::runtime::{EXEC_COMMAND_SPEC, READ_LINES_SPEC, WRITE_STDIN_SPEC};
use sandbox_operation_contract::{OperationRequest, OperationResponse};
use sandbox_runtime_namespace_execution::NamespaceExecutionId;

const EXEC_COMMAND: OperationEntry =
    OperationEntry::public(&EXEC_COMMAND_SPEC, dispatch_exec_command);
const WRITE_COMMAND_STDIN: OperationEntry =
    OperationEntry::public(&WRITE_STDIN_SPEC, dispatch_write_command_stdin);
const READ_COMMAND_LINES: OperationEntry =
    OperationEntry::public(&READ_LINES_SPEC, dispatch_read_command_lines);

const PUBLIC_OPERATIONS: &[OperationEntry] =
    &[EXEC_COMMAND, WRITE_COMMAND_STDIN, READ_COMMAND_LINES];

pub(crate) const fn public_operation_entries() -> &'static [OperationEntry] {
    PUBLIC_OPERATIONS
}

fn dispatch_exec_command(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let input = match parse_exec_command_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    command_output_response(operations.command.exec_command(input))
}

fn parse_exec_command_input(
    request: &OperationRequest,
) -> Result<ExecCommandInput, OperationResponse> {
    Ok(ExecCommandInput {
        workspace_session_id: request
            .optional_string("workspace_session_id")?
            .filter(|workspace_session_id| !workspace_session_id.is_empty())
            .map(WorkspaceSessionId),
        cmd: request.required_string("cmd")?,
        timeout_ms: request.optional_u64("timeout_ms")?,
        yield_time_ms: request.optional_u64("yield_time_ms")?,
    })
}

fn dispatch_write_command_stdin(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let input = match parse_write_command_stdin_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    command_output_response(operations.command.write_command_stdin(input))
}

fn parse_write_command_stdin_input(
    request: &OperationRequest,
) -> Result<WriteCommandStdinInput, OperationResponse> {
    Ok(WriteCommandStdinInput {
        command_session_id: NamespaceExecutionId(request.required_string("command_session_id")?),
        stdin: request.required_string("stdin")?,
        yield_time_ms: request.optional_u64("yield_time_ms")?,
    })
}

fn dispatch_read_command_lines(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let input = match parse_read_command_lines_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    command_output_response(Ok(operations.command.read_command_lines(input)))
}

fn parse_read_command_lines_input(
    request: &OperationRequest,
) -> Result<ReadCommandLinesInput, OperationResponse> {
    Ok(ReadCommandLinesInput {
        command_session_id: NamespaceExecutionId(request.required_string("command_session_id")?),
        start_offset: request.optional_u64("start_offset")?,
        limit: request.optional_usize("limit")?,
    })
}

fn command_output_response(
    result: Result<CommandOutput, CommandServiceError>,
) -> OperationResponse {
    match result {
        Ok(output) if output.status == CommandStatus::Running => {
            OperationResponse::running(command_output_value(output))
        }
        Ok(output) => OperationResponse::ok(command_output_value(output)),
        Err(error) => command_service_error_response(error),
    }
}

fn command_service_error_response(error: CommandServiceError) -> OperationResponse {
    let details = command_error_details(&error);
    OperationResponse::fault_with_details("operation_failed", error.to_string(), details)
}

fn command_error_details(error: &CommandServiceError) -> Value {
    match error {
        CommandServiceError::CommandFinalizationFailed {
            command_session_id, ..
        } => json!({
            "command_session_id": command_session_id.0.as_str(),
        }),
        CommandServiceError::LayerStack(error) => match error.as_ref() {
            crate::layerstack::LayerStackServiceError::PublishRejected { rejection } => json!({
                "publish_rejection": publish_reject_value(rejection),
            }),
            _ => json!({}),
        },
        _ => json!({}),
    }
}

fn command_output_value(output: CommandOutput) -> Value {
    let mut value = json!({
        "status": status_name(output.status),
        "exit_code": output.exit_code,
        "wall_time_seconds": output.wall_time_seconds,
        "command_total_time_seconds": output.command_total_time_seconds,
        "start_offset": output.start_offset,
        "end_offset": output.end_offset,
        "total_lines": output.total_lines,
        "original_token_count": output.original_token_count,
        "output": output.output,
    });
    if let Some(command_session_id) = output.command_session_id {
        value["command_session_id"] = Value::String(command_session_id.0);
    }
    if let Some(workspace_session_id) = output.workspace_session_id {
        value["workspace_session_id"] = Value::String(workspace_session_id.0);
    }
    if let Some(reject_class) = output.publish_rejected {
        value["publish_rejected"] = Value::Bool(true);
        value["publish_reject_class"] = Value::String(reject_class.to_owned());
    }
    value
}

fn status_name(status: CommandStatus) -> &'static str {
    status.as_str()
}

fn publish_reject_value(rejection: &sandbox_runtime_layerstack::PublishReject) -> Value {
    json!({
        "path": rejection.path.as_ref().map(ToString::to_string),
        "reason": publish_reject_reason_name(rejection.reason),
        "source_conflict": rejection.source_conflict.as_ref().map(|conflict| {
            json!({
                "path": conflict.path.to_string(),
                "expected": content_fingerprint_value(&conflict.expected),
                "actual": content_fingerprint_value(&conflict.actual),
            })
        }),
        "protected_drop": rejection.protected_drop.as_ref().map(|drop| {
            json!({
                "path": drop.path.as_str(),
                "reason": protected_drop_reason_name(drop.reason),
            })
        }),
        "message": rejection.message.as_deref(),
    })
}

fn publish_reject_reason_name(
    reason: sandbox_runtime_layerstack::PublishRejectReason,
) -> &'static str {
    match reason {
        sandbox_runtime_layerstack::PublishRejectReason::InvalidBaseRevision => {
            "invalid_base_revision"
        }
        sandbox_runtime_layerstack::PublishRejectReason::ProtectedPath => "protected_path",
        sandbox_runtime_layerstack::PublishRejectReason::SourceConflict => "source_conflict",
        sandbox_runtime_layerstack::PublishRejectReason::OpaqueDirProtectedDescendant => {
            "opaque_dir_protected_descendant"
        }
        sandbox_runtime_layerstack::PublishRejectReason::OpaqueDirMixedRoutes => {
            "opaque_dir_mixed_routes"
        }
        sandbox_runtime_layerstack::PublishRejectReason::OpaqueDirExpansionLimit => {
            "opaque_dir_expansion_limit"
        }
        sandbox_runtime_layerstack::PublishRejectReason::RoutePreparationFailed => {
            "route_preparation_failed"
        }
    }
}

fn protected_drop_reason_name(
    reason: sandbox_runtime_layerstack::LayerProtectedDropReason,
) -> &'static str {
    match reason {
        sandbox_runtime_layerstack::LayerProtectedDropReason::UnsupportedSpecialFile => {
            "unsupported_special_file"
        }
        sandbox_runtime_layerstack::LayerProtectedDropReason::InvalidLayerPath => {
            "invalid_layer_path"
        }
        sandbox_runtime_layerstack::LayerProtectedDropReason::CommandScratchPath => {
            "command_scratch_path"
        }
    }
}

fn content_fingerprint_value(
    fingerprint: &sandbox_runtime_layerstack::ContentFingerprint,
) -> Value {
    match fingerprint {
        sandbox_runtime_layerstack::ContentFingerprint::Absent => json!({
            "kind": "absent",
        }),
        sandbox_runtime_layerstack::ContentFingerprint::File { digest, executable } => json!({
            "kind": "file",
            "digest": digest,
            "executable": executable,
        }),
        sandbox_runtime_layerstack::ContentFingerprint::Symlink { target } => json!({
            "kind": "symlink",
            "target": target,
        }),
        sandbox_runtime_layerstack::ContentFingerprint::Directory => json!({
            "kind": "directory",
        }),
    }
}
