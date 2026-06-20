use std::time::Instant;

use crate::command::{
    CommandFinalizedMetadata, CommandLifecycleState, CommandServiceError, CommandSessionId,
    CommandStatus, CommandTerminalResult, CommandTranscriptStore, CompletedCommandRecord,
    FinalizationState, RetainedCommandTranscript,
};
use crate::workspace_crate::WorkspaceSessionId;

use super::CommandOperationService;

#[derive(Debug, Clone)]
pub(crate) struct ActiveFinalizationRecord {
    command_session_id: CommandSessionId,
    workspace_session_id: WorkspaceSessionId,
    transcript: CommandTranscriptStore,
}

impl CommandOperationService {
    pub(crate) fn finalize_command(
        &self,
        command_session_id: CommandSessionId,
        process_exit: ::command::process::CommandProcessExit,
    ) -> Result<CommandTerminalResult, CommandServiceError> {
        let record = self.begin_finalization(&command_session_id)?;
        let result = terminal_result(&process_exit);
        let finalized = self.finalize_session_command(&record, &process_exit);

        let finalized = match finalized {
            Ok(finalized) => finalized,
            Err(error) => {
                return self.fail_finalization(&command_session_id, error.to_string());
            }
        };

        match self.complete_finalized_command(record, result.clone(), finalized) {
            Ok(()) => Ok(result),
            Err(error) => self.fail_finalization(&command_session_id, error.to_string()),
        }
    }

    fn finalize_session_command(
        &self,
        _record: &ActiveFinalizationRecord,
        _process_exit: &::command::process::CommandProcessExit,
    ) -> Result<CommandFinalizedMetadata, CommandServiceError> {
        Ok(CommandFinalizedMetadata)
    }

    fn begin_finalization(
        &self,
        command_session_id: &CommandSessionId,
    ) -> Result<ActiveFinalizationRecord, CommandServiceError> {
        let active = self
            .process_store()
            .active(command_session_id)
            .ok_or_else(|| CommandServiceError::CommandNotFound {
                command_session_id: command_session_id.clone(),
            })?;
        if let FinalizationState::Failed { error, finalized } = &active.finalization {
            return Err(CommandServiceError::CommandFinalizationFailed {
                command_session_id: command_session_id.clone(),
                error: error.clone(),
                finalized: finalized.clone().map(Box::new),
            });
        }
        let record = ActiveFinalizationRecord {
            command_session_id: active.command_session_id.clone(),
            workspace_session_id: active.workspace_session_id.clone(),
            transcript: active.transcript.clone(),
        };
        drop(active);

        self.mark_active_finalization(
            command_session_id,
            CommandLifecycleState::Finalizing,
            FinalizationState::InProgress,
        )?;
        Ok(record)
    }

    fn complete_finalized_command(
        &self,
        record: ActiveFinalizationRecord,
        result: CommandTerminalResult,
        finalized: CommandFinalizedMetadata,
    ) -> Result<(), CommandServiceError> {
        let command_session_id = record.command_session_id.clone();
        let completed = CompletedCommandRecord {
            command_session_id: command_session_id.clone(),
            workspace_session_id: record.workspace_session_id,
            result,
            transcript: RetainedCommandTranscript {
                transcript_path: record.transcript.transcript_path,
            },
            finalization: FinalizationState::Complete,
            finalized: Some(finalized),
            completed_at: Instant::now(),
        };
        let _ = self.process_store().complete_active(completed)?;
        Ok(())
    }

    fn mark_active_finalization(
        &self,
        command_session_id: &CommandSessionId,
        lifecycle_state: CommandLifecycleState,
        finalization: FinalizationState,
    ) -> Result<(), CommandServiceError> {
        self.process_store()
            .update_active(command_session_id, |active| {
                active.lifecycle_state = lifecycle_state;
                active.finalization = finalization;
            })
            .ok_or_else(|| CommandServiceError::CommandNotFound {
                command_session_id: command_session_id.clone(),
            })
    }

    fn fail_finalization<T>(
        &self,
        command_session_id: &CommandSessionId,
        error: String,
    ) -> Result<T, CommandServiceError> {
        let finalized = self
            .process_store()
            .update_active(command_session_id, |active| {
                let finalized = retained_finalized_metadata(&active.finalization);
                active.lifecycle_state = CommandLifecycleState::FinalizationFailed;
                active.finalization = FinalizationState::Failed {
                    error: error.clone(),
                    finalized: finalized.clone(),
                };
                finalized
            });
        Err(CommandServiceError::CommandFinalizationFailed {
            command_session_id: command_session_id.clone(),
            error,
            finalized: finalized.flatten().map(Box::new),
        })
    }
}

fn terminal_result(process_exit: &::command::process::CommandProcessExit) -> CommandTerminalResult {
    CommandTerminalResult {
        status: if process_exit_succeeded(process_exit) {
            CommandStatus::Completed
        } else {
            CommandStatus::Failed
        },
        exit_code: Some(process_exit.exit_code),
        stdout: process_exit.stdout.clone(),
    }
}

fn process_exit_succeeded(process_exit: &::command::process::CommandProcessExit) -> bool {
    process_exit.kill.is_none() && process_exit.exit_code == 0
}

fn retained_finalized_metadata(state: &FinalizationState) -> Option<CommandFinalizedMetadata> {
    match state {
        FinalizationState::Failed { finalized, .. } => finalized.clone(),
        FinalizationState::NotStarted
        | FinalizationState::InProgress
        | FinalizationState::Complete => None,
    }
}
