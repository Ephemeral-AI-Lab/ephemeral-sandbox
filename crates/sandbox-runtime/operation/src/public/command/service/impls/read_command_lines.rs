use super::command_lines_response;
use crate::command::service::transcript::command_lines_output;
use crate::command::service::CommandOperationService;
use crate::command::{
    CommandLinesOutput, CommandServiceError, CommandSessionId, CommandStatus, ReadCommandLinesInput,
};
use crate::operation::{ArgCliSpec, ArgKind, ArgSpec, CliOperationSpec, CliSpec};
use crate::SandboxRuntimeOperations;
use sandbox_protocol::{Request, Response};
use tracing::{field, Span};

pub(crate) const SPEC: CliOperationSpec = CliOperationSpec {
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

pub(crate) fn dispatch(operations: &SandboxRuntimeOperations, request: &Request) -> Response {
    let input = match parse_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    command_lines_response(operations.command.read_command_lines(input))
}

fn parse_input(request: &Request) -> Result<ReadCommandLinesInput, Response> {
    Ok(ReadCommandLinesInput {
        command_session_id: CommandSessionId(request.required_string("command_session_id")?),
        start_offset: request.optional_u64("start_offset")?,
        limit: request.optional_usize("limit")?,
    })
}

impl CommandOperationService {
    pub fn read_command_lines(
        &self,
        input: ReadCommandLinesInput,
    ) -> Result<CommandLinesOutput, CommandServiceError> {
        let span = tracing::info_span!(
            "runtime.read_command_lines",
            requested_start_offset = input.start_offset.unwrap_or(0),
            requested_limit = input.limit.unwrap_or(200),
            status = field::Empty,
            error_kind = field::Empty,
            exit_code = field::Empty,
            start_offset = field::Empty,
            end_offset = field::Empty,
            total_lines = field::Empty,
        );
        let _span_guard = span.enter();
        let result = self.read_command_lines_inner(input);
        record_command_lines_result(&span, &result);
        result
    }

    fn read_command_lines_inner(
        &self,
        input: ReadCommandLinesInput,
    ) -> Result<CommandLinesOutput, CommandServiceError> {
        let command_session_id = input.command_session_id;
        let start_offset = input.start_offset.unwrap_or(0);
        let limit = validate_read_limit(input.limit)?;
        if let Some(active) = self.active_command_or_none(&command_session_id)? {
            if active.process.process_group_id().is_some() {
                if let Some(process_exit) = active.process.take_exit() {
                    drop(active);
                    self.complete_terminal_command(command_session_id.clone(), process_exit)?;
                    let completed = self.completed_command(&command_session_id)?;
                    return completed_command_lines_output(
                        completed,
                        command_session_id,
                        start_offset,
                        limit,
                    );
                }
            }
            let transcript = active.transcript.clone();
            let elapsed = active.started_at.elapsed().as_secs_f64();
            drop(active);
            return Ok(command_lines_output(
                transcript.window(start_offset, limit),
                command_session_id,
                CommandStatus::Running,
                None,
                elapsed,
                elapsed,
            ));
        }

        let completed = self.completed_command(&command_session_id)?;
        completed_command_lines_output(completed, command_session_id, start_offset, limit)
    }
}

fn record_command_lines_result(
    span: &Span,
    result: &Result<CommandLinesOutput, CommandServiceError>,
) {
    match result {
        Ok(output) => {
            span.record("status", output.status.as_str());
            if let Some(exit_code) = output.exit_code {
                span.record("exit_code", exit_code);
            }
            span.record("start_offset", output.start_offset);
            span.record("end_offset", output.end_offset);
            span.record("total_lines", output.total_lines);
        }
        Err(error) => {
            span.record("status", "error");
            span.record("error_kind", error.kind());
        }
    }
}

fn validate_read_limit(limit: Option<usize>) -> Result<usize, CommandServiceError> {
    match limit.unwrap_or(200) {
        0 => Err(CommandServiceError::InvalidCommand {
            message: "limit must be positive".to_owned(),
        }),
        limit if limit > 1000 => Err(CommandServiceError::InvalidCommand {
            message: "limit must be at most 1000".to_owned(),
        }),
        limit => Ok(limit),
    }
}

fn completed_command_lines_output(
    completed: crate::command::CompletedCommandRecord,
    command_session_id: CommandSessionId,
    start_offset: u64,
    limit: usize,
) -> Result<CommandLinesOutput, CommandServiceError> {
    Ok(command_lines_output(
        completed
            .transcript
            .window(&command_session_id, start_offset, limit)?,
        command_session_id,
        completed.result.status,
        completed.result.exit_code,
        completed.started_at.elapsed().as_secs_f64(),
        completed.result.command_total_time_seconds,
    ))
}
