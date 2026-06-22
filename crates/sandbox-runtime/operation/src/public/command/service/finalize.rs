use crate::command::{
    CommandFinalizedMetadata, CommandLifecycleState, CommandServiceError, CommandSessionId,
    CommandStatus, CommandTerminalResult, CommandTranscriptStore, CommandWorkspaceOwnership,
    CompletedCommandRecord, FinalizationState, RetainedCommandTranscript,
};
use crate::workspace_crate::{DestroyWorkspaceRequest, WorkspaceSessionId};

use super::CommandOperationService;
use tracing::{field, Span};

#[derive(Debug, Clone)]
pub(crate) struct ActiveCompletionRecord {
    command_session_id: CommandSessionId,
    workspace_session_id: WorkspaceSessionId,
    workspace_ownership: CommandWorkspaceOwnership,
    started_at: std::time::Instant,
    transcript: CommandTranscriptStore,
}

impl CommandOperationService {
    pub(crate) fn complete_terminal_command(
        &self,
        command_session_id: CommandSessionId,
        process_exit: ::sandbox_runtime_command::process::CommandProcessExit,
    ) -> Result<CommandTerminalResult, CommandServiceError> {
        let span = tracing::info_span!(
            "command.finalize",
            status = field::Empty,
            error_kind = field::Empty,
            exit_code = process_exit.exit_code,
            killed = process_exit.kill.is_some(),
            cgroup_final_sample = process_exit.cgroup_final_sample.is_some(),
            cgroup_cleanup_error = process_exit
                .cgroup_cleanup
                .as_ref()
                .and_then(|cleanup| cleanup.last_cleanup_error.as_ref())
                .is_some(),
        );
        let _span_guard = span.enter();
        let result = self.complete_terminal_command_inner(command_session_id, process_exit);
        record_terminal_result(&span, &result);
        result
    }

    fn complete_terminal_command_inner(
        &self,
        command_session_id: CommandSessionId,
        process_exit: ::sandbox_runtime_command::process::CommandProcessExit,
    ) -> Result<CommandTerminalResult, CommandServiceError> {
        let record = self.begin_terminal_completion(&command_session_id)?;
        tracing::info!(
            name: "cgroup_monitor.final_summary",
            boundary = "command_finalization_handoff",
            target_kind = "command",
            sample_available = process_exit.cgroup_final_sample.is_some(),
            cleanup_available = process_exit.cgroup_cleanup.is_some(),
            cleanup_error = process_exit
                .cgroup_cleanup
                .as_ref()
                .and_then(|cleanup| cleanup.last_cleanup_error.as_ref())
                .is_some(),
            exit_code = process_exit.exit_code,
            killed = process_exit.kill.is_some(),
        );
        self.workspace().cgroup_monitor().record_command_final(
            &record.workspace_session_id,
            &command_session_id.0,
            process_exit.cgroup_final_sample.clone(),
            process_exit.cgroup_cleanup.clone(),
        );
        let result = terminal_result(&process_exit);

        let finalized = match self.apply_workspace_completion_policy(&record) {
            Ok(finalized) => finalized,
            Err(error) => {
                return self.fail_completion(&command_session_id, error.to_string(), result, None);
            }
        };

        match self.complete_command_record(record, result.clone(), finalized) {
            Ok(()) => Ok(result),
            Err(error) => {
                self.fail_completion(&command_session_id, error.to_string(), result, None)
            }
        }
    }

    fn apply_workspace_completion_policy(
        &self,
        record: &ActiveCompletionRecord,
    ) -> Result<Option<CommandFinalizedMetadata>, CommandServiceError> {
        match &record.workspace_ownership {
            CommandWorkspaceOwnership::ExistingSession => Ok(None),
            CommandWorkspaceOwnership::OneShot { handler } => {
                self.workspace().destroy_session(
                    handler.as_ref().clone(),
                    DestroyWorkspaceRequest::default(),
                )?;
                Ok(None)
            }
        }
    }

    fn begin_terminal_completion(
        &self,
        command_session_id: &CommandSessionId,
    ) -> Result<ActiveCompletionRecord, CommandServiceError> {
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
                finalized: finalized.clone(),
            });
        }
        let record = ActiveCompletionRecord {
            command_session_id: active.command_session_id.clone(),
            workspace_session_id: active.workspace_session_id.clone(),
            workspace_ownership: active.workspace_ownership.clone(),
            started_at: active.started_at,
            transcript: active.transcript.clone(),
        };
        drop(active);

        self.mark_active_completion(
            command_session_id,
            CommandLifecycleState::Finalizing,
            FinalizationState::InProgress,
        )?;
        Ok(record)
    }

    fn complete_command_record(
        &self,
        record: ActiveCompletionRecord,
        result: CommandTerminalResult,
        finalized: Option<CommandFinalizedMetadata>,
    ) -> Result<(), CommandServiceError> {
        let command_session_id = record.command_session_id.clone();
        let completed = CompletedCommandRecord {
            command_session_id: command_session_id.clone(),
            workspace_session_id: record.workspace_session_id,
            started_at: record.started_at,
            result,
            transcript: RetainedCommandTranscript {
                transcript_path: record.transcript.transcript_path,
            },
            finalization: FinalizationState::Complete,
            finalized,
        };
        let _ = self.process_store().complete_active(completed)?;
        Ok(())
    }

    fn mark_active_completion(
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

    fn fail_completion<T>(
        &self,
        command_session_id: &CommandSessionId,
        error: String,
        mut result: CommandTerminalResult,
        finalized_override: Option<CommandFinalizedMetadata>,
    ) -> Result<T, CommandServiceError> {
        result.status = CommandStatus::Error;
        let finalized = finalized_override.or_else(|| {
            self.process_store()
                .active(command_session_id)
                .and_then(|active| retained_finalized_metadata(&active.finalization))
        });
        self.process_store().fail_active(
            command_session_id,
            error.clone(),
            result,
            finalized.clone(),
        )?;
        Err(CommandServiceError::CommandFinalizationFailed {
            command_session_id: command_session_id.clone(),
            error,
            finalized: finalized.map(Box::new),
        })
    }
}

fn record_terminal_result(
    span: &Span,
    result: &Result<CommandTerminalResult, CommandServiceError>,
) {
    match result {
        Ok(result) => {
            span.record("status", result.status.as_str());
            if let Some(exit_code) = result.exit_code {
                span.record("exit_code", exit_code);
            }
        }
        Err(error) => {
            span.record("status", "error");
            span.record("error_kind", error.kind());
        }
    }
}

fn terminal_result(
    process_exit: &::sandbox_runtime_command::process::CommandProcessExit,
) -> CommandTerminalResult {
    CommandTerminalResult {
        status: terminal_status(process_exit),
        exit_code: Some(process_exit.exit_code),
        stdout: process_exit.stdout.clone(),
        command_total_time_seconds: process_exit.elapsed_s,
    }
}

fn terminal_status(
    process_exit: &::sandbox_runtime_command::process::CommandProcessExit,
) -> CommandStatus {
    match process_exit.status.as_str() {
        "ok" => CommandStatus::Ok,
        "timed_out" => CommandStatus::TimedOut,
        "cancelled" => CommandStatus::Cancelled,
        _ => CommandStatus::Error,
    }
}

fn retained_finalized_metadata(state: &FinalizationState) -> Option<CommandFinalizedMetadata> {
    match state {
        FinalizationState::Failed { finalized, .. } => {
            finalized.as_ref().map(|metadata| metadata.as_ref().clone())
        }
        FinalizationState::NotStarted
        | FinalizationState::InProgress
        | FinalizationState::Complete => None,
    }
}
