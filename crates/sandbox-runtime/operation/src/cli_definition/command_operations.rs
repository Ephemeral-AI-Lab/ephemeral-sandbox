use serde_json::{json, Value};

use crate::cli_definition::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationFamilySpec, CliOperationSpec, CliSpec,
};
use crate::command::{
    CommandFinalizedMetadata, CommandLinesOutput, CommandPublishStatus, CommandServiceError,
    CommandSessionId, CommandStatus, CommandYield, ExecCommandInput, ReadCommandLinesInput,
    WriteCommandStdinInput,
};
use crate::observability::{measure_optional, OperationTrace};
use crate::operation::OperationEntry;
use crate::workspace_crate::WorkspaceSessionId;
use crate::SandboxRuntimeOperations;
use sandbox_protocol::{Request, Response};

pub(crate) const COMMAND_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "command",
    title: "Command",
    summary: "Run, interact with, and inspect commands.",
    description: "Run, interact with, and inspect commands inside the active sandbox runtime.",
};

const EXEC_COMMAND_SPEC: CliOperationSpec = CliOperationSpec {
    name: "exec_command",
    family: "command",
    summary: "Start a command in a workspace.",
    description: "Start a shell command inside an existing workspace session when workspace_session_id is provided, otherwise create a one-shot host-compatible workspace and destroy it when the command reaches terminal state. If the command is still running after the initial wait, the response includes a command_session_id that can be used with write_command_stdin or read_command_lines.",
    args: EXEC_COMMAND_ARGS,
    cli: Some(EXEC_COMMAND_CLI),
    related: &["write_command_stdin", "read_command_lines"],
};

const EXEC_COMMAND_ARGS: &[ArgSpec] = &[
    ArgSpec::optional(
        "workspace_session_id",
        ArgKind::String,
        "Existing workspace session id to run inside. Omit to run in a one-shot workspace.",
        None,
        Some(ArgCliSpec {
            flag: Some("--workspace-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "cmd",
        ArgKind::String,
        "Shell command text.",
        Some(ArgCliSpec {
            flag: None,
            positional: Some("COMMAND"),
        }),
    ),
    ArgSpec::optional(
        "timeout_ms",
        ArgKind::Integer,
        "Command timeout in milliseconds.",
        None,
        Some(ArgCliSpec {
            flag: Some("--timeout-ms"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "yield_time_ms",
        ArgKind::Integer,
        "Initial output wait in milliseconds.",
        None,
        Some(ArgCliSpec {
            flag: Some("--yield-time-ms"),
            positional: None,
        }),
    ),
];

const EXEC_COMMAND_CLI: CliSpec = CliSpec {
    path: &["runtime", "exec_command"],
    usage: "sandbox-cli runtime exec_command [--workspace-session-id ID] COMMAND",
    examples: &[
        "sandbox-cli runtime exec_command pwd",
        "sandbox-cli runtime exec_command --workspace-session-id ws-1 pwd",
        "sandbox-cli runtime exec_command --workspace-session-id ws-1 --yield-time-ms 0 \"sleep 30\"",
    ],
};

const WRITE_STDIN_SPEC: CliOperationSpec = CliOperationSpec {
    name: "write_command_stdin",
    family: "command",
    summary: "Write text to a running command stdin.",
    description: "Append text to the stdin stream of a running command session and return a bounded output yield.",
    args: WRITE_STDIN_ARGS,
    cli: Some(WRITE_STDIN_CLI),
    related: &["exec_command", "read_command_lines"],
};

const WRITE_STDIN_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "command_session_id",
        ArgKind::String,
        "Command session id returned by exec_command.",
        Some(ArgCliSpec {
            flag: Some("--command-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "stdin",
        ArgKind::String,
        "Text to write to stdin.",
        Some(ArgCliSpec {
            flag: None,
            positional: Some("TEXT"),
        }),
    ),
    ArgSpec::optional(
        "yield_time_ms",
        ArgKind::Integer,
        "Output wait after writing stdin.",
        None,
        Some(ArgCliSpec {
            flag: Some("--yield-time-ms"),
            positional: None,
        }),
    ),
];

const WRITE_STDIN_CLI: CliSpec = CliSpec {
    path: &["runtime", "write_command_stdin"],
    usage: "sandbox-cli runtime write_command_stdin --command-session-id ID TEXT",
    examples: &["sandbox-cli runtime write_command_stdin --command-session-id cmd-1 hello"],
};

const READ_LINES_SPEC: CliOperationSpec = CliOperationSpec {
    name: "read_command_lines",
    family: "command",
    summary: "Read command output by line offset.",
    description: "Read rendered command output for a command session using stable line offsets.",
    args: READ_LINES_ARGS,
    cli: Some(READ_LINES_CLI),
    related: &["exec_command", "write_command_stdin"],
};

const READ_LINES_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "command_session_id",
        ArgKind::String,
        "Command session id returned by exec_command.",
        Some(ArgCliSpec {
            flag: Some("--command-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "start_offset",
        ArgKind::Integer,
        "First transcript line offset. Defaults to 0.",
        None,
        Some(ArgCliSpec {
            flag: Some("--start-offset"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "limit",
        ArgKind::Integer,
        "Maximum transcript rows to return. Defaults to 200; maximum 1000.",
        None,
        Some(ArgCliSpec {
            flag: Some("--limit"),
            positional: None,
        }),
    ),
];

const READ_LINES_CLI: CliSpec = CliSpec {
    path: &["runtime", "read_command_lines"],
    usage: "sandbox-cli runtime read_command_lines --command-session-id ID [--start-offset N] [--limit N]",
    examples: &[
        "sandbox-cli runtime read_command_lines --command-session-id cmd-1 --start-offset 0 --limit 100",
    ],
};

const EXEC_COMMAND: OperationEntry = OperationEntry::cli(&EXEC_COMMAND_SPEC, dispatch_exec_command);
const WRITE_COMMAND_STDIN: OperationEntry =
    OperationEntry::cli(&WRITE_STDIN_SPEC, dispatch_write_command_stdin);
const READ_COMMAND_LINES: OperationEntry =
    OperationEntry::cli(&READ_LINES_SPEC, dispatch_read_command_lines);

const OPERATIONS: &[OperationEntry] = &[EXEC_COMMAND, WRITE_COMMAND_STDIN, READ_COMMAND_LINES];

pub(crate) fn operation_entries() -> &'static [OperationEntry] {
    OPERATIONS
}

fn dispatch_exec_command(
    operations: &SandboxRuntimeOperations,
    request: &Request,
    trace: Option<&OperationTrace>,
) -> Response {
    let input = match parse_exec_command_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    let origin_request_id = Some(request.request_id.clone());
    command_yield_response(measure_optional(
        trace,
        "CommandOperationService::exec_command",
        || {
            operations
                .command
                .exec_command_with_origin_request_id(input, trace, origin_request_id)
        },
    ))
}

fn parse_exec_command_input(request: &Request) -> Result<ExecCommandInput, Response> {
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
    request: &Request,
    trace: Option<&OperationTrace>,
) -> Response {
    let input = match parse_write_command_stdin_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    command_yield_response(measure_optional(
        trace,
        "CommandOperationService::write_command_stdin",
        || operations.command.write_command_stdin(input),
    ))
}

fn parse_write_command_stdin_input(request: &Request) -> Result<WriteCommandStdinInput, Response> {
    Ok(WriteCommandStdinInput {
        command_session_id: CommandSessionId(request.required_string("command_session_id")?),
        stdin: request.required_string("stdin")?,
        yield_time_ms: request.optional_u64("yield_time_ms")?,
    })
}

fn dispatch_read_command_lines(
    operations: &SandboxRuntimeOperations,
    request: &Request,
    trace: Option<&OperationTrace>,
) -> Response {
    let input = match parse_read_command_lines_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    command_lines_response(measure_optional(
        trace,
        "CommandOperationService::read_command_lines",
        || operations.command.read_command_lines(input),
    ))
}

fn parse_read_command_lines_input(request: &Request) -> Result<ReadCommandLinesInput, Response> {
    Ok(ReadCommandLinesInput {
        command_session_id: CommandSessionId(request.required_string("command_session_id")?),
        start_offset: request.optional_u64("start_offset")?,
        limit: request.optional_usize("limit")?,
    })
}

fn command_yield_response(result: Result<CommandYield, CommandServiceError>) -> Response {
    match result {
        Ok(output) if output.status == CommandStatus::Running => {
            Response::running(command_yield_value(output))
        }
        Ok(output) => Response::ok(command_yield_value(output)),
        Err(error) => command_service_error_response(error),
    }
}

fn command_lines_response(result: Result<CommandLinesOutput, CommandServiceError>) -> Response {
    match result {
        Ok(output) if output.status == CommandStatus::Running => {
            Response::running(command_lines_value(output))
        }
        Ok(output) => Response::ok(command_lines_value(output)),
        Err(error) => command_service_error_response(error),
    }
}

fn command_service_error_response(error: CommandServiceError) -> Response {
    let details = command_error_details(&error);
    Response::fault_with_details("operation_failed", error.to_string(), details)
}

fn command_error_details(error: &CommandServiceError) -> Value {
    match error {
        CommandServiceError::CommandFinalizationFailed {
            command_session_id,
            finalized,
            ..
        } => json!({
            "command_session_id": command_session_id.0.as_str(),
            "finalized": finalized
                .as_ref()
                .map(|metadata| finalized_value(Some(metadata.as_ref())))
                .unwrap_or(Value::Null),
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

fn command_yield_value(output: CommandYield) -> Value {
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
    value
}

fn command_lines_value(output: CommandLinesOutput) -> Value {
    json!({
        "command_session_id": output.command_session_id.0,
        "status": status_name(output.status),
        "exit_code": output.exit_code,
        "wall_time_seconds": output.wall_time_seconds,
        "command_total_time_seconds": output.command_total_time_seconds,
        "start_offset": output.start_offset,
        "end_offset": output.end_offset,
        "total_lines": output.total_lines,
        "original_token_count": output.original_token_count,
        "output": output.output,
    })
}

fn status_name(status: CommandStatus) -> &'static str {
    status.as_str()
}

fn finalized_value(finalized: Option<&CommandFinalizedMetadata>) -> Value {
    finalized.map_or(Value::Null, |finalized| {
        json!({
            "policy": "session",
            "outcome": "session_complete",
            "publish": finalized.publish.as_ref().map(|publish| {
                json!({
                    "status": publish_status_name(publish.status),
                    "rejection": publish.rejection.as_deref().map(publish_reject_value),
                    "revision": publish.revision.as_ref().map(|revision| {
                        json!({
                            "manifest_version": revision.manifest_version,
                            "root_hash": revision.root_hash.as_str(),
                            "layer_count": revision.layer_count,
                        })
                    }),
                    "layer_paths": publish.layer_paths.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                })
            }),
        })
    })
}

fn publish_status_name(status: CommandPublishStatus) -> &'static str {
    match status {
        CommandPublishStatus::Published => "published",
        CommandPublishStatus::NoOp => "no_op",
        CommandPublishStatus::Rejected => "rejected",
        CommandPublishStatus::Skipped => "skipped",
    }
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
        sandbox_runtime_layerstack::PublishRejectReason::GitMutationForbidden => {
            "git_mutation_forbidden"
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
