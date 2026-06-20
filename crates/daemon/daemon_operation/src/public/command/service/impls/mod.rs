mod cancel;
mod exec_command;
mod poll;
mod read_command_lines;
mod write_command_stdin;

use serde_json::{json, Value};

use crate::command::{
    CommandFinalizedMetadata, CommandLinesOutput, CommandPollOutput, CommandServiceError,
    CommandStatus, CommandStream, CommandTranscriptRow, CommandYield,
};
use crate::operation::{OperationEntry, OperationRequest, OperationResponse, OperationSpec};

pub(crate) const OPERATIONS: &[OperationEntry] = &[
    OperationEntry::new(&exec_command::SPEC, exec_command::dispatch),
    OperationEntry::new(&write_command_stdin::SPEC, write_command_stdin::dispatch),
    OperationEntry::new(&poll::SPEC, poll::dispatch),
    OperationEntry::new(&read_command_lines::SPEC, read_command_lines::dispatch),
    OperationEntry::new(&cancel::SPEC, cancel::dispatch),
];

pub(crate) const SPECS: &[&OperationSpec] = &[
    &exec_command::SPEC,
    &write_command_stdin::SPEC,
    &poll::SPEC,
    &read_command_lines::SPEC,
    &cancel::SPEC,
];

pub(super) fn command_yield_response(
    request: &OperationRequest<'_>,
    result: Result<CommandYield, CommandServiceError>,
) -> OperationResponse {
    match result {
        Ok(output) if output.status == CommandStatus::Running => {
            OperationResponse::running(request, command_yield_value(output))
        }
        Ok(output) => OperationResponse::ok(request, command_yield_value(output)),
        Err(error) => OperationResponse::service_error(request, error),
    }
}

pub(super) fn command_poll_response(
    request: &OperationRequest<'_>,
    result: Result<CommandPollOutput, CommandServiceError>,
) -> OperationResponse {
    match result {
        Ok(output) if output.status == CommandStatus::Running => {
            OperationResponse::running(request, command_poll_value(output))
        }
        Ok(output) => OperationResponse::ok(request, command_poll_value(output)),
        Err(error) => OperationResponse::service_error(request, error),
    }
}

pub(super) fn command_lines_response(
    request: &OperationRequest<'_>,
    result: Result<CommandLinesOutput, CommandServiceError>,
) -> OperationResponse {
    match result {
        Ok(output) if output.status == CommandStatus::Running => {
            OperationResponse::running(request, command_lines_value(output))
        }
        Ok(output) => OperationResponse::ok(request, command_lines_value(output)),
        Err(error) => OperationResponse::service_error(request, error),
    }
}

fn command_yield_value(output: CommandYield) -> Value {
    json!({
        "command_session_id": output.command_session_id.map(|command_session_id| command_session_id.0),
        "status": status_name(output.status),
        "exit_code": output.exit_code,
        "output": { "stdout": output.output.stdout },
        "finalized": finalized_value(output.finalized.as_ref()),
    })
}

fn command_poll_value(output: CommandPollOutput) -> Value {
    json!({
        "command_session_id": output.command_session_id.0,
        "status": status_name(output.status),
        "exit_code": output.exit_code,
        "output": { "stdout": output.output.stdout },
        "finalized": finalized_value(output.finalized.as_ref()),
    })
}

fn command_lines_value(output: CommandLinesOutput) -> Value {
    json!({
        "command_session_id": output.command_session_id.0,
        "status": status_name(output.status),
        "exit_code": output.exit_code,
        "start_offset": output.start_offset,
        "end_offset": output.end_offset,
        "total_lines": output.total_lines,
        "truncated_before": output.truncated_before,
        "output_truncated": output.output_truncated,
        "output": output.output.into_iter().map(transcript_row_value).collect::<Vec<_>>(),
    })
}

fn status_name(status: CommandStatus) -> &'static str {
    match status {
        CommandStatus::Running => "running",
        CommandStatus::Completed => "completed",
        CommandStatus::Failed => "failed",
    }
}

fn transcript_row_value(row: CommandTranscriptRow) -> Value {
    json!({
        "offset": row.offset,
        "stream": stream_name(row.stream),
        "text": row.text,
    })
}

fn stream_name(stream: CommandStream) -> &'static str {
    match stream {
        CommandStream::Stdout => "stdout",
        CommandStream::Stderr => "stderr",
    }
}

fn finalized_value(finalized: Option<&CommandFinalizedMetadata>) -> Value {
    finalized.map_or(Value::Null, |_| {
        json!({
            "policy": "session",
            "outcome": "session_complete",
        })
    })
}
