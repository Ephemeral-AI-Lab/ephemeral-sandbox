use sandbox_runtime_command::CommandExecution;

use crate::command::service::helpers::finalize_message;
use crate::command::service::{execution_id, CommandOperationService};
use crate::command::{CommandOutput, CommandServiceError, WriteCommandStdinInput};
use crate::workspace_crate::WorkspaceSessionId;

impl CommandOperationService {
    pub fn write_command_stdin(
        &self,
        input: WriteCommandStdinInput,
    ) -> Result<CommandOutput, CommandServiceError> {
        let command_session_id = input.command_session_id;
        let yield_time_ms = input.yield_time_ms.unwrap_or(1000);
        let is_kill_input = is_kill_input(&input.stdin);
        let id = execution_id(&command_session_id);

        let target = self.engine().with_value(&id, |command| {
            if !command.is_finished() {
                return WriteTarget::Live {
                    workspace_session_id: command.workspace_session_id().clone(),
                    start_offset: command.output_len(),
                };
            }
            match command.terminal_result() {
                Some(Err(error)) => WriteTarget::FinalizationFailed(finalize_message(&error)),
                _ => WriteTarget::AlreadyCompleted,
            }
        });
        let (workspace_session_id, start_offset) = match target {
            None => return Err(CommandServiceError::CommandNotFound { command_session_id }),
            Some(WriteTarget::AlreadyCompleted) => {
                return Err(CommandServiceError::CommandAlreadyCompleted { command_session_id });
            }
            Some(WriteTarget::FinalizationFailed(error)) => {
                return Err(CommandServiceError::CommandFinalizationFailed {
                    command_session_id,
                    error,
                });
            }
            Some(WriteTarget::Live {
                workspace_session_id,
                start_offset,
            }) => (workspace_session_id, start_offset),
        };

        if !is_kill_input {
            self.ensure_workspace_session_not_remount_pending(&workspace_session_id)?;
        }

        if is_kill_input {
            // Clone the cancel action out from under the registry lock, then kill
            // with no lock held (the kill blocks for a SIGTERM grace period).
            match self
                .engine()
                .with_value(&id, CommandExecution::cancel_handle)
            {
                Some(cancel) => cancel(),
                None => return Err(CommandServiceError::CommandNotFound { command_session_id }),
            }
        } else {
            match self
                .engine()
                .with_value(&id, |command| command.write_stdin(input.stdin.as_bytes()))
            {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    return Err(CommandServiceError::CommandIo {
                        command_session_id,
                        error: error.to_string(),
                    });
                }
                None => return Err(CommandServiceError::CommandNotFound { command_session_id }),
            }
        }

        let wait_time_ms = if is_kill_input { 1000 } else { yield_time_ms };
        self.wait_for_command_yield(command_session_id, wait_time_ms, start_offset, true)
    }
}

enum WriteTarget {
    Live {
        workspace_session_id: WorkspaceSessionId,
        start_offset: u64,
    },
    AlreadyCompleted,
    FinalizationFailed(String),
}

fn is_kill_input(stdin: &str) -> bool {
    stdin.contains('\u{3}') || stdin.contains('\u{4}')
}
