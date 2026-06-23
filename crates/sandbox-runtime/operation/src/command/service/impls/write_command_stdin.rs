use std::sync::Arc;
use std::time::Instant;

use crate::command::service::CommandOperationService;
use crate::command::{
    CancellationState, CommandLifecycleState, CommandServiceError, CommandYield,
    WriteCommandStdinInput,
};

impl CommandOperationService {
    pub fn write_command_stdin(
        &self,
        input: WriteCommandStdinInput,
    ) -> Result<CommandYield, CommandServiceError> {
        let command_session_id = input.command_session_id;
        let yield_time_ms = input.yield_time_ms.unwrap_or(1000);
        let (process, workspace_session_id) = {
            let active = self.active_command(&command_session_id)?;
            (
                Arc::clone(&active.process),
                active.workspace_session_id.clone(),
            )
        };
        let is_kill_input = is_kill_input(&input.stdin);
        if !is_kill_input {
            self.ensure_workspace_session_not_remount_pending(&workspace_session_id)?;
        }
        let start_offset = process.transcript_len();
        if is_kill_input {
            self.process_store()
                .update_active(&command_session_id, |active| {
                    active.process.cancel_process();
                    active.lifecycle_state = CommandLifecycleState::Cancelled;
                    active.cancellation = CancellationState::Requested {
                        requested_at: Instant::now(),
                    };
                })
                .ok_or_else(|| CommandServiceError::CommandNotFound {
                    command_session_id: command_session_id.clone(),
                })?;
        } else {
            process.write_process_stdin(&input.stdin).map_err(|error| {
                CommandServiceError::CommandIo {
                    command_session_id: command_session_id.clone(),
                    error: error.to_string(),
                }
            })?;
        }

        let wait_time_ms = if is_kill_input { 1000 } else { yield_time_ms };
        self.wait_for_command_yield(command_session_id, wait_time_ms, start_offset, true)
    }
}

fn is_kill_input(stdin: &str) -> bool {
    stdin.contains('\u{3}') || stdin.contains('\u{4}')
}
