use sandbox_runtime_namespace_execution::{transcript_window, NamespaceExecutionId};

use crate::command::service::render::{command_output, command_status};
use crate::command::service::CommandOperationService;
use crate::command::{CommandExecValue, CommandOutput, CommandStatus, ReadCommandLinesInput};

impl CommandOperationService {
    #[must_use]
    pub fn read_command_lines(&self, input: ReadCommandLinesInput) -> CommandOutput {
        let command_session_id = input.command_session_id;
        let start_offset = input.start_offset.unwrap_or(0);
        let limit = input
            .limit
            .unwrap_or(self.config().read_lines_default)
            .clamp(1, self.config().read_lines_max);

        self.engine()
            .with_value(&command_session_id, |command| {
                read_command_window(command, &command_session_id, start_offset, limit)
            })
            .or_else(|| {
                self.terminal_drains().with(&command_session_id, |record| {
                    let window = record.window(start_offset, limit);
                    let elapsed = record.elapsed_seconds();
                    let (status, exit_code, command_total_time_seconds) =
                        if record.completion.wait_timeout(std::time::Duration::ZERO) {
                            match &record.result {
                                Ok(result) => (
                                    command_status(result.status),
                                    Some(result.exit_code),
                                    result.command_total_time_seconds,
                                ),
                                Err(_) => (CommandStatus::Error, None, elapsed),
                            }
                        } else {
                            (CommandStatus::Running, None, elapsed)
                        };
                    let mut output = command_output(
                        window,
                        Some(command_session_id.clone()),
                        status,
                        exit_code,
                        elapsed,
                        command_total_time_seconds,
                    );
                    output.workspace_session_id = Some(record.workspace_session_id.clone());
                    if let Some(outcome) = record.finalize_outcome.get() {
                        output.publish_rejected = outcome.publish_reject_class;
                        output.finalization_failed = outcome.finalization_failure_class;
                        output.finalization_attempts = outcome.finalization_attempts;
                    }
                    output
                })
            })
            .unwrap_or_else(|| empty_terminal_output(command_session_id))
    }
}

fn read_command_window(
    command: &CommandExecValue,
    command_session_id: &NamespaceExecutionId,
    start_offset: u64,
    limit: usize,
) -> CommandOutput {
    let window = command.transcript_window(start_offset, limit);
    let elapsed = command.elapsed_seconds();
    let (status, exit_code, command_total_time_seconds) = match command.exec.resolved() {
        None => (CommandStatus::Running, None, elapsed),
        Some(Ok(result)) => (
            command_status(result.status),
            Some(result.exit_code),
            result.command_total_time_seconds,
        ),
        Some(Err(_)) => (CommandStatus::Error, None, elapsed),
    };
    let mut output = command_output(
        window,
        Some(command_session_id.clone()),
        status,
        exit_code,
        elapsed,
        command_total_time_seconds,
    );
    output.workspace_session_id = Some(command.workspace_session_id.clone());
    if let Some(outcome) = command.finalize_outcome.get() {
        output.publish_rejected = outcome.publish_reject_class;
        output.finalization_failed = outcome.finalization_failure_class;
        output.finalization_attempts = outcome.finalization_attempts;
    }
    output
}

fn empty_terminal_output(command_session_id: NamespaceExecutionId) -> CommandOutput {
    command_output(
        transcript_window(None, 0, 1, 0),
        Some(command_session_id),
        CommandStatus::Ok,
        None,
        0.0,
        0.0,
    )
}
