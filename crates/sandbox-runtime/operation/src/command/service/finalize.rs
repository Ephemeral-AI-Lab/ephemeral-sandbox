use crate::command::{
    CommandFinalizedMetadata, CommandLifecycleState, CommandProcessStore, CommandServiceError,
    CommandSessionId, CommandStatus, CommandTerminalResult, CommandTranscriptStore,
    CommandWorkspaceOwnership, CompletedCommandRecord, FinalizationState,
    RetainedCommandTranscript,
};
use crate::namespace_execution::{
    CompleteNamespaceExecution, NamespaceExecutionId, NamespaceExecutionStore,
    NamespaceExecutionTerminalStatus,
};
use crate::observability::{measure_optional, OperationTrace};
use crate::workspace_crate::{DestroyWorkspaceRequest, WorkspaceSessionId};
use crate::workspace_session::WorkspaceSessionService;

#[derive(Debug, Clone)]
pub(crate) struct ActiveCompletionRecord {
    command_session_id: CommandSessionId,
    namespace_execution_id: NamespaceExecutionId,
    workspace_session_id: WorkspaceSessionId,
    workspace_ownership: CommandWorkspaceOwnership,
    started_at: std::time::Instant,
    transcript: CommandTranscriptStore,
    next_snapshot_offset: u64,
}

pub(crate) struct CommandCompletionOutcome {
    pub(crate) workspace_session_id: Option<WorkspaceSessionId>,
    pub(crate) result: Result<CommandTerminalResult, CommandServiceError>,
}

pub(crate) fn complete_terminal_command_with_services(
    workspace: &WorkspaceSessionService,
    process_store: &CommandProcessStore,
    namespace_execution: &NamespaceExecutionStore,
    command_session_id: CommandSessionId,
    process_exit: ::sandbox_runtime_command::process::CommandProcessExit,
    trace: Option<&OperationTrace>,
) -> CommandCompletionOutcome {
    measure_optional(trace, "complete_terminal_command_with_services", || {
        complete_terminal_command_inner(
            workspace,
            process_store,
            namespace_execution,
            command_session_id,
            process_exit,
            trace,
        )
    })
}

fn complete_terminal_command_inner(
    workspace: &WorkspaceSessionService,
    process_store: &CommandProcessStore,
    namespace_execution: &NamespaceExecutionStore,
    command_session_id: CommandSessionId,
    process_exit: ::sandbox_runtime_command::process::CommandProcessExit,
    trace: Option<&OperationTrace>,
) -> CommandCompletionOutcome {
    let record = match begin_terminal_completion(process_store, &command_session_id) {
        Ok(record) => record,
        Err(error) => {
            return CommandCompletionOutcome {
                workspace_session_id: None,
                result: Err(error),
            };
        }
    };
    let workspace_session_id = Some(record.workspace_session_id.clone());
    let result = terminal_result(&process_exit);
    complete_namespace_execution(namespace_execution, &record.namespace_execution_id, &result);

    let finalized = match measure_optional(trace, "apply_workspace_completion_policy", || {
        apply_workspace_completion_policy(workspace, &record)
    }) {
        Ok(finalized) => finalized,
        Err(error) => {
            return CommandCompletionOutcome {
                workspace_session_id,
                result: fail_completion(
                    process_store,
                    &command_session_id,
                    error.to_string(),
                    result,
                    None,
                ),
            };
        }
    };

    let completion_result = match measure_optional(trace, "complete_command_record", || {
        complete_command_record(process_store, record, result.clone(), finalized)
    }) {
        Ok(()) => Ok(result),
        Err(error) => fail_completion(
            process_store,
            &command_session_id,
            error.to_string(),
            result,
            None,
        ),
    };
    CommandCompletionOutcome {
        workspace_session_id,
        result: completion_result,
    }
}

fn apply_workspace_completion_policy(
    workspace: &WorkspaceSessionService,
    record: &ActiveCompletionRecord,
) -> Result<Option<CommandFinalizedMetadata>, CommandServiceError> {
    match &record.workspace_ownership {
        CommandWorkspaceOwnership::ExistingSession => Ok(None),
        CommandWorkspaceOwnership::OneShot { handler } => {
            workspace
                .destroy_session(handler.as_ref().clone(), DestroyWorkspaceRequest::default())?;
            Ok(None)
        }
    }
}

fn begin_terminal_completion(
    process_store: &CommandProcessStore,
    command_session_id: &CommandSessionId,
) -> Result<ActiveCompletionRecord, CommandServiceError> {
    let active = process_store.active(command_session_id).ok_or_else(|| {
        CommandServiceError::CommandNotFound {
            command_session_id: command_session_id.clone(),
        }
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
        namespace_execution_id: active.namespace_execution_id.clone(),
        workspace_session_id: active.workspace_session_id.clone(),
        workspace_ownership: active.workspace_ownership.clone(),
        started_at: active.started_at,
        transcript: active.transcript.clone(),
        next_snapshot_offset: active.next_snapshot_offset,
    };
    drop(active);

    mark_active_completion(
        process_store,
        command_session_id,
        CommandLifecycleState::Finalizing,
        FinalizationState::InProgress,
    )?;
    Ok(record)
}

fn complete_command_record(
    process_store: &CommandProcessStore,
    record: ActiveCompletionRecord,
    result: CommandTerminalResult,
    finalized: Option<CommandFinalizedMetadata>,
) -> Result<(), CommandServiceError> {
    let command_session_id = record.command_session_id.clone();
    let completed = CompletedCommandRecord {
        command_session_id: command_session_id.clone(),
        workspace_session_id: record.workspace_session_id,
        namespace_execution_id: record.namespace_execution_id,
        started_at: record.started_at,
        result,
        transcript: RetainedCommandTranscript {
            transcript_path: record.transcript.transcript_path,
        },
        next_snapshot_offset: record.next_snapshot_offset,
        finalization: FinalizationState::Complete,
        finalized,
    };
    let _ = process_store.complete_active(completed)?;
    Ok(())
}

fn complete_namespace_execution(
    namespace_execution: &NamespaceExecutionStore,
    namespace_execution_id: &NamespaceExecutionId,
    result: &CommandTerminalResult,
) {
    let _ = namespace_execution.complete_namespace_execution(
        namespace_execution_id,
        CompleteNamespaceExecution {
            terminal_status: namespace_terminal_status(result.status),
            exit_code: result.exit_code,
            error_kind: None,
            error_message: None,
        },
    );
}

fn mark_active_completion(
    process_store: &CommandProcessStore,
    command_session_id: &CommandSessionId,
    lifecycle_state: CommandLifecycleState,
    finalization: FinalizationState,
) -> Result<(), CommandServiceError> {
    process_store
        .update_active(command_session_id, |active| {
            active.lifecycle_state = lifecycle_state;
            active.finalization = finalization;
        })
        .ok_or_else(|| CommandServiceError::CommandNotFound {
            command_session_id: command_session_id.clone(),
        })
}

fn fail_completion<T>(
    process_store: &CommandProcessStore,
    command_session_id: &CommandSessionId,
    error: String,
    mut result: CommandTerminalResult,
    finalized_override: Option<CommandFinalizedMetadata>,
) -> Result<T, CommandServiceError> {
    result.status = CommandStatus::Error;
    let finalized = finalized_override.or_else(|| {
        process_store
            .active(command_session_id)
            .and_then(|active| retained_finalized_metadata(&active.finalization))
    });
    process_store.fail_active(command_session_id, error.clone(), result, finalized.clone())?;
    Err(CommandServiceError::CommandFinalizationFailed {
        command_session_id: command_session_id.clone(),
        error,
        finalized: finalized.map(Box::new),
    })
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

const fn namespace_terminal_status(status: CommandStatus) -> NamespaceExecutionTerminalStatus {
    match status {
        CommandStatus::Ok => NamespaceExecutionTerminalStatus::Ok,
        CommandStatus::Error | CommandStatus::Running => NamespaceExecutionTerminalStatus::Error,
        CommandStatus::TimedOut => NamespaceExecutionTerminalStatus::TimedOut,
        CommandStatus::Cancelled => NamespaceExecutionTerminalStatus::Cancelled,
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
