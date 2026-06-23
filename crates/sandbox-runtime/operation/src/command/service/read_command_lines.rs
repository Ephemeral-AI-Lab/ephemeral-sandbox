use crate::command::service::transcript::command_lines_output;
use crate::command::service::CommandOperationService;
use crate::command::{
    CommandLinesOutput, CommandServiceError, CommandSessionId, CommandStatus, ReadCommandLinesInput,
};

impl CommandOperationService {
    pub fn read_command_lines(
        &self,
        input: ReadCommandLinesInput,
    ) -> Result<CommandLinesOutput, CommandServiceError> {
        let command_session_id = input.command_session_id;
        let start_offset = input.start_offset.unwrap_or(0);
        let limit = validate_read_limit(input.limit)?;
        if let Some(active) = self.active_command_or_none(&command_session_id)? {
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
