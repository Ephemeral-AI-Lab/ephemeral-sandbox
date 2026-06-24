use sandbox_runtime_command::CommandExecution;

use crate::command::service::helpers::{command_status, finalize_message};
use crate::command::service::transcript::command_output;
use crate::command::service::{execution_id, CommandOperationService};
use crate::command::{
    CommandOutput, CommandServiceError, CommandSessionId, CommandStatus, ReadCommandLinesInput,
};

impl CommandOperationService {
    pub fn read_command_lines(
        &self,
        input: ReadCommandLinesInput,
    ) -> Result<CommandOutput, CommandServiceError> {
        let command_session_id = input.command_session_id;
        let start_offset = input.start_offset.unwrap_or(0);
        let limit = validate_read_limit(input.limit)?;
        let id = execution_id(&command_session_id);

        let read = self.engine().with_value(&id, |command| {
            read_command_window(command, &command_session_id, start_offset, limit)
        });
        match read {
            Some(result) => result,
            None => Err(CommandServiceError::CommandNotFound { command_session_id }),
        }
    }
}

fn read_command_window(
    command: &CommandExecution,
    command_session_id: &CommandSessionId,
    start_offset: u64,
    limit: usize,
) -> Result<CommandOutput, CommandServiceError> {
    if !command.is_finished() {
        let window = command.transcript_window(start_offset, limit);
        let elapsed = command.elapsed_seconds();
        return Ok(command_output(
            window,
            Some(command_session_id.clone()),
            CommandStatus::Running,
            None,
            elapsed,
            elapsed,
        ));
    }
    match command.terminal_result() {
        Some(Ok(result)) => {
            let window = command
                .required_transcript_window(start_offset, limit)
                .map_err(|error| CommandServiceError::CommandTranscriptUnavailable {
                    command_session_id: command_session_id.clone(),
                    path: None,
                    error,
                })?;
            let elapsed = command.elapsed_seconds();
            Ok(command_output(
                window,
                Some(command_session_id.clone()),
                command_status(result.status),
                Some(result.exit_code),
                elapsed,
                result.command_total_time_seconds,
            ))
        }
        Some(Err(error)) => Err(CommandServiceError::CommandFinalizationFailed {
            command_session_id: command_session_id.clone(),
            error: finalize_message(&error),
        }),
        None => Err(CommandServiceError::CommandNotFound {
            command_session_id: command_session_id.clone(),
        }),
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
