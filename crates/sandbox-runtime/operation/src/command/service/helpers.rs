use super::core::CommandOperationService;

use crate::command::{
    CommandOutputSnapshot, CommandServiceError, CommandSessionId, CommandStatus, CommandYield,
    CompletedCommandRecord,
};
use crate::workspace_crate::WorkspaceSessionId;

use super::completion::{wait_for_completed_record, CommandCompletionWaitOutcome};
use super::transcript::command_output_snapshot;

impl CommandOperationService {
    pub(crate) fn running_command_yield(
        &self,
        command_session_id: CommandSessionId,
        _observed_output: String,
    ) -> Result<CommandYield, CommandServiceError> {
        let snapshot = self.active_command_output_snapshot(&command_session_id)?;
        Ok(command_yield(
            Some(command_session_id),
            CommandStatus::Running,
            None,
            snapshot.wall_time_seconds,
            snapshot.command_total_time_seconds,
            snapshot.output,
        ))
    }

    pub(crate) fn wait_for_command_yield(
        &self,
        command_session_id: CommandSessionId,
        yield_time_ms: u64,
        start_offset: u64,
        include_terminal_command_session_id: bool,
    ) -> Result<CommandYield, CommandServiceError> {
        let (process, completion) = match self.active_command_or_none(&command_session_id)? {
            Some(active) => (
                std::sync::Arc::clone(&active.process),
                active.completion.clone(),
            ),
            None => {
                let completed = self.completed_command(&command_session_id)?;
                return self.completed_command_yield(
                    command_session_id,
                    completed,
                    include_terminal_command_session_id,
                );
            }
        };
        let outcome = self.launch_driver().wait_for_command_yield(
            process.as_ref(),
            &completion,
            yield_time_ms,
            start_offset,
        );
        match outcome {
            CommandCompletionWaitOutcome::Running => self.running_or_completed_command_yield(
                command_session_id,
                include_terminal_command_session_id,
            ),
            CommandCompletionWaitOutcome::Completed => {
                let completed =
                    wait_for_completed_record(self.process_store(), &command_session_id)?;
                self.completed_command_yield(
                    command_session_id,
                    completed,
                    include_terminal_command_session_id,
                )
            }
        }
    }

    fn running_or_completed_command_yield(
        &self,
        command_session_id: CommandSessionId,
        include_command_session_id: bool,
    ) -> Result<CommandYield, CommandServiceError> {
        if let Some(completed) = self.process_store().completed(&command_session_id) {
            return self.completed_command_yield(
                command_session_id,
                completed,
                include_command_session_id,
            );
        }
        self.running_command_yield(command_session_id, String::new())
    }

    fn completed_command_yield(
        &self,
        command_session_id: CommandSessionId,
        completed: CompletedCommandRecord,
        include_command_session_id: bool,
    ) -> Result<CommandYield, CommandServiceError> {
        let window = sandbox_runtime_command::transcript_window(
            completed.transcript.transcript_path.as_deref(),
            completed.next_snapshot_offset,
            usize::MAX,
        );
        let has_more_output = window.output_truncated;
        let snapshot = command_output_snapshot(window);
        let returned_command_session_id =
            (include_command_session_id || has_more_output).then_some(command_session_id);
        Ok(command_yield(
            returned_command_session_id,
            completed.result.status,
            completed.result.exit_code,
            completed.started_at.elapsed().as_secs_f64(),
            completed.result.command_total_time_seconds,
            snapshot,
        ))
    }

    fn active_command_output_snapshot(
        &self,
        command_session_id: &CommandSessionId,
    ) -> Result<TimedCommandOutputSnapshot, CommandServiceError> {
        self.process_store()
            .update_active(command_session_id, |active| {
                let start_offset = active.next_snapshot_offset;
                let window = active.transcript.window(start_offset, usize::MAX);
                let output = super::transcript::command_output_snapshot(window);
                active.next_snapshot_offset = output.end_offset;
                let elapsed = active.started_at.elapsed().as_secs_f64();
                TimedCommandOutputSnapshot {
                    output,
                    wall_time_seconds: elapsed,
                    command_total_time_seconds: elapsed,
                }
            })
            .ok_or_else(|| CommandServiceError::CommandNotFound {
                command_session_id: command_session_id.clone(),
            })
    }

    pub(crate) fn ensure_workspace_session_not_remount_pending(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<(), CommandServiceError> {
        if self.workspace_remount_pending(workspace_session_id) {
            return Err(CommandServiceError::WorkspaceSessionRemountPending {
                workspace_session_id: workspace_session_id.clone(),
            });
        }
        if self.workspace_remount_blocked(workspace_session_id) {
            return Err(CommandServiceError::WorkspaceSessionRemountBlocked {
                workspace_session_id: workspace_session_id.clone(),
            });
        }
        Ok(())
    }
}

struct TimedCommandOutputSnapshot {
    output: CommandOutputSnapshot,
    wall_time_seconds: f64,
    command_total_time_seconds: f64,
}

pub(crate) fn command_yield(
    command_session_id: Option<CommandSessionId>,
    status: CommandStatus,
    exit_code: Option<i64>,
    wall_time_seconds: f64,
    command_total_time_seconds: f64,
    output: CommandOutputSnapshot,
) -> CommandYield {
    CommandYield {
        command_session_id,
        status,
        exit_code,
        wall_time_seconds,
        command_total_time_seconds,
        start_offset: output.start_offset,
        end_offset: output.end_offset,
        total_lines: output.total_lines,
        original_token_count: output.original_token_count,
        output: output.output,
    }
}
