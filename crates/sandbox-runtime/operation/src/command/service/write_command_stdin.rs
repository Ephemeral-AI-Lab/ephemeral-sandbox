use crate::command::service::r#yield::{command_not_found, finalize_message};
use crate::command::service::CommandOperationService;
use crate::command::{CommandOutput, CommandServiceError, WriteCommandStdinInput};

impl CommandOperationService {
    pub fn write_command_stdin(
        &self,
        input: WriteCommandStdinInput,
    ) -> Result<CommandOutput, CommandServiceError> {
        let command_session_id = input.command_session_id;
        let yield_time_ms = input.yield_time_ms.unwrap_or(1000);
        let is_kill_input = is_kill_input(&input.stdin);
        let id = command_session_id.clone();

        let target = self.engine().with_value(&id, |command| {
            if !command.exec.is_finished() {
                return WriteTarget::Live;
            }
            match command.exec.resolved() {
                Some(Err(error)) => WriteTarget::FinalizationFailed(finalize_message(&error)),
                _ => WriteTarget::AlreadyCompleted,
            }
        });
        match target {
            None => return command_not_found(command_session_id),
            Some(WriteTarget::AlreadyCompleted) => {
                return Err(CommandServiceError::CommandAlreadyCompleted { command_session_id });
            }
            Some(WriteTarget::FinalizationFailed(error)) => {
                return Err(CommandServiceError::CommandFinalizationFailed {
                    command_session_id,
                    error,
                });
            }
            Some(WriteTarget::Live) => {}
        }

        if is_kill_input {
            match self
                .engine()
                .with_value(&id, |command| command.exec.cancel())
            {
                Some(()) => {}
                None => return command_not_found(command_session_id),
            }
        } else {
            match self.engine().with_value(&id, |command| {
                command.exec.write_stdin(input.stdin.as_bytes())
            }) {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    return Err(CommandServiceError::CommandIo {
                        command_session_id,
                        error: error.to_string(),
                    });
                }
                None => return command_not_found(command_session_id),
            }
        }

        let wait_time_ms = if is_kill_input { 1000 } else { yield_time_ms };
        self.wait_for_command_yield(command_session_id, wait_time_ms, true)
    }
}

enum WriteTarget {
    Live,
    AlreadyCompleted,
    FinalizationFailed(String),
}

fn is_kill_input(stdin: &str) -> bool {
    stdin.contains('\u{3}') || stdin.contains('\u{4}')
}
