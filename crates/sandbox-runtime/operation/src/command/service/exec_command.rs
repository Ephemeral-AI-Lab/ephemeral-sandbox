use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sandbox_runtime_command::process::{CommandProcess, CommandProcessSpec};

use crate::command::service::CommandCompletionPromise;
use crate::command::service::CommandOperationService;
use crate::command::{
    ActiveCommandProcess, CancellationState, CommandLifecycleState, CommandServiceError,
    CommandSessionId, CommandTranscriptStore, CommandWorkspaceOwnership, CommandYield,
    ExecCommandInput, FinalizationState,
};
use crate::namespace_execution::{
    BeginNamespaceExecution, CompleteNamespaceExecution, NamespaceExecutionId,
    NamespaceExecutionTerminalStatus,
};
use crate::observability::{measure_optional_if, span_keys, OperationTrace};
use crate::workspace_crate::{WorkspaceEntry, WorkspaceSessionId};
use crate::workspace_session::WorkspaceSessionHandler;

use super::core::WorkspaceLifecycleAdmission;

impl CommandOperationService {
    pub fn exec_command(
        &self,
        input: ExecCommandInput,
        trace: Option<&OperationTrace>,
    ) -> Result<CommandYield, CommandServiceError> {
        self.exec_command_with_origin_request_id(input, trace, None)
    }

    pub(crate) fn exec_command_with_origin_request_id(
        &self,
        input: ExecCommandInput,
        trace: Option<&OperationTrace>,
        origin_request_id: Option<String>,
    ) -> Result<CommandYield, CommandServiceError> {
        if input.cmd.trim().is_empty() {
            return Err(CommandServiceError::InvalidCommand {
                message: "cmd must be non-empty".to_owned(),
            });
        }

        self.exec_validated_command(input, trace, origin_request_id)
    }

    fn exec_validated_command(
        &self,
        input: ExecCommandInput,
        trace: Option<&OperationTrace>,
        origin_request_id: Option<String>,
    ) -> Result<CommandYield, CommandServiceError> {
        let existing_session_admission = input
            .workspace_session_id
            .is_some()
            .then(|| self.begin_workspace_lifecycle_admission());
        let workspace =
            measure_optional_if(trace, span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE, || {
                self.resolve_exec_workspace(&input, trace)
            })?;
        let admission_guard =
            self.command_admission_guard(&workspace, existing_session_admission)?;
        let command_session_id = self.process_store().allocate_command_session_id();
        let reservation = match self.process_store().try_reserve() {
            Ok(reservation) => reservation,
            Err(error) => {
                return Err(self.cleanup_workspace_start_failure(
                    &command_session_id,
                    workspace,
                    None,
                    error,
                ));
            }
        };
        let namespace_execution_id = self
            .namespace_execution_store()
            .allocate_namespace_execution_id();
        let _ = self.namespace_execution_store().begin_namespace_execution(
            namespace_execution_id.clone(),
            BeginNamespaceExecution {
                workspace_session_id: workspace.workspace_session_id.clone(),
                operation_name: "exec_command".to_owned(),
                request_id: origin_request_id.clone(),
            },
        );
        let started =
            match self.start_command_process(&command_session_id, &input, &workspace, trace) {
                Ok(started) => {
                    let _ = self
                        .namespace_execution_store()
                        .mark_namespace_execution_running(&namespace_execution_id);
                    started
                }
                Err(error) => {
                    self.complete_namespace_start_failure(&namespace_execution_id, &error);
                    return Err(self.cleanup_workspace_start_failure(
                        &command_session_id,
                        workspace,
                        None,
                        error,
                    ));
                }
            };

        if self.process_store().active(&command_session_id).is_some() {
            started.process.cancel_process();
            let error = CommandServiceError::DuplicateCommandSessionId {
                command_session_id: command_session_id.clone(),
            };
            self.complete_namespace_start_failure(&namespace_execution_id, &error);
            return Err(self.cleanup_workspace_start_failure(
                &command_session_id,
                workspace,
                Some(&started.process),
                error,
            ));
        }
        let completion = CommandCompletionPromise::new(
            command_session_id.clone(),
            self.completion_sender().clone(),
            origin_request_id,
        );
        let (record, process_for_rollback) = started.into_active_record(
            command_session_id.clone(),
            namespace_execution_id.clone(),
            &workspace,
            completion.clone(),
        );
        if let Err(error) = self.process_store().insert_active(reservation, record) {
            process_for_rollback.cancel_process();
            self.complete_namespace_start_failure(&namespace_execution_id, &error);
            return Err(self.cleanup_workspace_start_failure(
                &command_session_id,
                workspace,
                Some(process_for_rollback.as_ref()),
                error,
            ));
        }
        self.launch_driver()
            .start_completion_watcher(completion, Arc::clone(&process_for_rollback));
        drop(admission_guard);

        self.initial_exec_yield(command_session_id, input.yield_time_ms)
    }

    fn resolve_exec_workspace(
        &self,
        input: &ExecCommandInput,
        trace: Option<&OperationTrace>,
    ) -> Result<ResolvedExecWorkspace, CommandServiceError> {
        let handler = if let Some(workspace_session_id) = &input.workspace_session_id {
            measure_optional_if(
                trace,
                span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE_EXISTING_SESSION,
                || self.resolve_workspace_session(workspace_session_id.clone()),
            )?
        } else {
            measure_optional_if(
                trace,
                span_keys::COMMAND_EXEC_WORKSPACE_CREATE_ONE_SHOT_SESSION,
                || self.create_one_shot_workspace_session(),
            )?
        };
        let ownership = if input.workspace_session_id.is_some() {
            CommandWorkspaceOwnership::ExistingSession
        } else {
            CommandWorkspaceOwnership::OneShot {
                handler: Box::new(handler.clone()),
            }
        };
        Ok(ResolvedExecWorkspace::new(handler, ownership))
    }

    fn command_admission_guard<'a>(
        &'a self,
        workspace: &ResolvedExecWorkspace,
        existing_session_admission: Option<WorkspaceLifecycleAdmission<'a>>,
    ) -> Result<WorkspaceLifecycleAdmission<'a>, CommandServiceError> {
        let guard = existing_session_admission
            .unwrap_or_else(|| self.begin_workspace_lifecycle_admission());
        self.ensure_workspace_session_not_remount_pending(&workspace.workspace_session_id)?;
        Ok(guard)
    }

    fn start_command_process(
        &self,
        command_session_id: &CommandSessionId,
        input: &ExecCommandInput,
        workspace: &ResolvedExecWorkspace,
        trace: Option<&OperationTrace>,
    ) -> Result<StartedCommand, CommandServiceError> {
        measure_optional_if(trace, span_keys::COMMAND_EXEC_PROCESS_START, || {
            workspace.entry().and_then(|entry| {
                self.launch_driver()
                    .spawn(
                        CommandProcessSpec {
                            id: command_session_id.0.clone(),
                            command: input.cmd.clone(),
                            cwd: None,
                            timeout_seconds: input.timeout_ms.map(timeout_ms_to_seconds),
                        },
                        entry,
                        self.config(),
                    )
                    .map(StartedCommand::new)
            })
        })
    }

    fn initial_exec_yield(
        &self,
        command_session_id: CommandSessionId,
        yield_time_ms: Option<u64>,
    ) -> Result<CommandYield, CommandServiceError> {
        self.wait_for_command_yield(command_session_id, yield_time_ms.unwrap_or(1000), 0, false)
    }
}

impl CommandOperationService {
    fn cleanup_workspace_start_failure(
        &self,
        command_session_id: &CommandSessionId,
        workspace: ResolvedExecWorkspace,
        process: Option<&CommandProcess>,
        error: CommandServiceError,
    ) -> CommandServiceError {
        let error = if let Some(process) = process {
            cleanup_process_artifacts_after_start_failure(command_session_id, process, error)
        } else {
            error
        };
        self.cleanup_unstarted_workspace(command_session_id, workspace, error)
    }

    fn complete_namespace_start_failure(
        &self,
        namespace_execution_id: &NamespaceExecutionId,
        error: &CommandServiceError,
    ) {
        let _ = self
            .namespace_execution_store()
            .complete_namespace_execution(
                namespace_execution_id,
                CompleteNamespaceExecution {
                    terminal_status: NamespaceExecutionTerminalStatus::Error,
                    exit_code: None,
                    error_kind: Some("command_start_failed".to_owned()),
                    error_message: Some(namespace_start_failure_error_message(error).to_owned()),
                },
            );
    }

    fn cleanup_unstarted_workspace(
        &self,
        command_session_id: &CommandSessionId,
        workspace: ResolvedExecWorkspace,
        error: CommandServiceError,
    ) -> CommandServiceError {
        match workspace.ownership {
            CommandWorkspaceOwnership::ExistingSession => error,
            CommandWorkspaceOwnership::OneShot { handler } => {
                match self.destroy_one_shot_workspace_session(*handler) {
                    Ok(_) => error,
                    Err(cleanup_error) => CommandServiceError::OneShotWorkspaceCleanupFailed {
                        command_session_id: command_session_id.clone(),
                        command_error: Box::new(error),
                        cleanup_error: cleanup_error.to_string(),
                    },
                }
            }
        }
    }
}

struct ResolvedExecWorkspace {
    handler: WorkspaceSessionHandler,
    workspace_session_id: WorkspaceSessionId,
    ownership: CommandWorkspaceOwnership,
    workspace_root: PathBuf,
}

impl ResolvedExecWorkspace {
    fn new(handler: WorkspaceSessionHandler, ownership: CommandWorkspaceOwnership) -> Self {
        let workspace_session_id = handler.workspace_session_id.clone();
        let workspace_root = handler.handle.workspace_root.clone();
        Self {
            handler,
            workspace_session_id,
            ownership,
            workspace_root,
        }
    }

    fn entry(&self) -> Result<WorkspaceEntry, CommandServiceError> {
        self.handler
            .handle
            .entry()
            .map_err(|error| CommandServiceError::InvalidCommand {
                message: error.to_string(),
            })
    }
}

struct StartedCommand {
    process: CommandProcess,
    transcript_path: Option<PathBuf>,
}

impl StartedCommand {
    fn new(process: CommandProcess) -> Self {
        let transcript_path = process.transcript_path().map(std::path::Path::to_path_buf);
        Self {
            process,
            transcript_path,
        }
    }

    fn into_active_record(
        self,
        command_session_id: CommandSessionId,
        namespace_execution_id: NamespaceExecutionId,
        workspace: &ResolvedExecWorkspace,
        completion: CommandCompletionPromise,
    ) -> (ActiveCommandProcess, Arc<CommandProcess>) {
        let process = Arc::new(self.process);
        let process_for_rollback = Arc::clone(&process);
        let record = ActiveCommandProcess {
            command_session_id,
            namespace_execution_id,
            workspace_session_id: workspace.workspace_session_id.clone(),
            workspace_ownership: workspace.ownership.clone(),
            workspace_root: workspace.workspace_root.clone(),
            started_at: Instant::now(),
            process,
            completion: completion.clone(),
            transcript: CommandTranscriptStore {
                transcript_path: self.transcript_path,
            },
            next_snapshot_offset: 0,
            lifecycle_state: CommandLifecycleState::Running,
            cancellation: CancellationState::None,
            remount_cancellation: None,
            remount_switch_state: None,
            finalization: FinalizationState::NotStarted,
        };
        (record, process_for_rollback)
    }
}

fn timeout_ms_to_seconds(timeout_ms: u64) -> f64 {
    timeout_ms as f64 / 1000.0
}

fn cleanup_process_artifacts_after_start_failure(
    command_session_id: &CommandSessionId,
    process: &CommandProcess,
    error: CommandServiceError,
) -> CommandServiceError {
    match process.cleanup_artifacts_after_start_failure() {
        Ok(()) => error,
        Err(cleanup_error) => CommandServiceError::CommandArtifactCleanupFailed {
            command_session_id: command_session_id.clone(),
            command_error: Box::new(error),
            artifact_dir: process.artifact_dir(),
            cleanup_error: cleanup_error.to_string(),
        },
    }
}

fn namespace_start_failure_error_message(_error: &CommandServiceError) -> &'static str {
    "command start failed before namespace execution started"
}
