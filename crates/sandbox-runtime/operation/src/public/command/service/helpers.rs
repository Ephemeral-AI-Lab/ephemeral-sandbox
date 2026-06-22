use super::core::CommandOperationService;

use sandbox_runtime_command::process::CommandProcessExit;
use sandbox_runtime_command::yield_wait_loop::WaitOutcome;

use crate::command::{
    CommandOutputSnapshot, CommandServiceError, CommandSessionId, CommandStatus, CommandYield,
};
use crate::workspace_crate::WorkspaceSessionId;

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

    pub(crate) fn command_yield_from_wait_outcome(
        &self,
        command_session_id: CommandSessionId,
        outcome: WaitOutcome<CommandProcessExit>,
        include_terminal_command_session_id: bool,
    ) -> Result<CommandYield, CommandServiceError> {
        match outcome {
            WaitOutcome::Running(stdout) => self.running_command_yield(command_session_id, stdout),
            WaitOutcome::Completed(process_exit) => self.completed_command_yield(
                command_session_id,
                process_exit,
                include_terminal_command_session_id,
            ),
        }
    }

    pub(crate) fn completed_command_yield(
        &self,
        command_session_id: CommandSessionId,
        process_exit: CommandProcessExit,
        include_command_session_id: bool,
    ) -> Result<CommandYield, CommandServiceError> {
        let snapshot = self.active_command_output_snapshot(&command_session_id)?;
        let result = self.complete_terminal_command(command_session_id.clone(), process_exit)?;
        let completed = self.completed_command(&command_session_id)?;
        let command_session_id =
            (include_command_session_id || snapshot.has_more_output).then_some(command_session_id);
        Ok(command_yield(
            command_session_id,
            result.status,
            result.exit_code,
            completed.started_at.elapsed().as_secs_f64(),
            result.command_total_time_seconds,
            snapshot.output,
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
                let has_more_output = window.output_truncated;
                let output = super::transcript::command_output_snapshot(window);
                active.next_snapshot_offset = output.end_offset;
                let elapsed = active.started_at.elapsed().as_secs_f64();
                TimedCommandOutputSnapshot {
                    output,
                    has_more_output,
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
        if self.workspace().is_remount_pending(workspace_session_id) {
            return Err(CommandServiceError::WorkspaceSessionRemountPending {
                workspace_session_id: workspace_session_id.clone(),
            });
        }
        if self.workspace().is_remount_blocked(workspace_session_id) {
            return Err(CommandServiceError::WorkspaceSessionRemountBlocked {
                workspace_session_id: workspace_session_id.clone(),
            });
        }
        Ok(())
    }
}

struct TimedCommandOutputSnapshot {
    output: CommandOutputSnapshot,
    has_more_output: bool,
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
