use std::path::PathBuf;
use std::sync::{Arc, MutexGuard};

use sandbox_runtime_command::process::{CommandProcess, CommandProcessExit, CommandProcessSpec};
use sandbox_runtime_command::yield_wait_loop::WaitOutcome;

use super::command_yield_response;
use crate::command::service::CommandOperationService;
use crate::command::{
    ActiveCommandProcess, CancellationState, CommandLifecycleState, CommandOutputSnapshot,
    CommandServiceError, CommandSessionId, CommandTranscriptStore, CommandYield, ExecCommandInput,
    FinalizationState,
};
use crate::operation::{ArgCliSpec, ArgKind, ArgSpec, CliSpec, OperationFamily, OperationSpec};
use crate::workspace_crate::{WorkspaceEntry, WorkspaceSessionId};
use crate::workspace_session::WorkspaceSessionHandler;
use crate::SandboxRuntimeOperations;
use sandbox_protocol::{Request, Response};

pub(crate) const SPEC: OperationSpec = OperationSpec {
    name: "exec_command",
    family: OperationFamily::Command,
    summary: "Start a command in a workspace.",
    args: EXEC_COMMAND_ARGS,
    cli: Some(EXEC_COMMAND_CLI),
};

const EXEC_COMMAND_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "workspace_session_id",
        ArgKind::String,
        "Workspace session id to run inside.",
        Some(ArgCliSpec {
            flag: Some("--workspace-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "cmd",
        ArgKind::String,
        "Shell command text.",
        Some(ArgCliSpec {
            flag: None,
            positional: Some("COMMAND"),
        }),
    ),
    ArgSpec::optional(
        "timeout_seconds",
        ArgKind::Float,
        "Command timeout in seconds.",
        None,
        Some(ArgCliSpec {
            flag: Some("--timeout-seconds"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "yield_time_ms",
        ArgKind::Integer,
        "Initial output wait in milliseconds.",
        None,
        Some(ArgCliSpec {
            flag: Some("--yield-time-ms"),
            positional: None,
        }),
    ),
];

const EXEC_COMMAND_CLI: CliSpec = CliSpec {
    path: &["runtime", "exec_command"],
    usage: "sandbox runtime --sandbox-id ID exec_command --workspace-session-id ID COMMAND",
    examples: &["sandbox runtime --sandbox-id sbox-1 exec_command --workspace-session-id ws-1 pwd"],
};

pub(crate) fn dispatch(operations: &SandboxRuntimeOperations, request: &Request) -> Response {
    let input = match parse_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    command_yield_response(operations.command.exec_command(input))
}

fn parse_input(request: &Request) -> Result<ExecCommandInput, Response> {
    Ok(ExecCommandInput {
        workspace_session_id: WorkspaceSessionId(request.required_string("workspace_session_id")?),
        cmd: request.required_string("cmd")?,
        timeout_seconds: request.optional_f64("timeout_seconds")?,
        yield_time_ms: request.optional_u64("yield_time_ms")?,
    })
}

impl CommandOperationService {
    pub fn exec_command(
        &self,
        input: ExecCommandInput,
    ) -> Result<CommandYield, CommandServiceError> {
        if input.cmd.trim().is_empty() {
            return Err(CommandServiceError::InvalidCommand {
                message: "cmd must be non-empty".to_owned(),
            });
        }

        self.exec_validated_command(input)
    }

    fn exec_validated_command(
        &self,
        input: ExecCommandInput,
    ) -> Result<CommandYield, CommandServiceError> {
        let workspace = self.resolve_exec_workspace(&input)?;
        let admission_guard = self.command_admission_guard(&workspace)?;
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
        let started = match self.start_command_process(&command_session_id, &input, &workspace) {
            Ok(started) => started,
            Err(error) => {
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
            return Err(self.cleanup_workspace_start_failure(
                &command_session_id,
                workspace,
                Some(&started.process),
                CommandServiceError::DuplicateCommandSessionId {
                    command_session_id: command_session_id.clone(),
                },
            ));
        }
        let (record, process_for_rollback) =
            started.into_active_record(command_session_id.clone(), &workspace);
        if let Err(error) = self.process_store().insert_active(reservation, record) {
            process_for_rollback.cancel_process();
            return Err(self.cleanup_workspace_start_failure(
                &command_session_id,
                workspace,
                Some(process_for_rollback.as_ref()),
                error,
            ));
        }
        drop(admission_guard);

        self.initial_exec_yield(command_session_id, input.yield_time_ms)
    }

    fn resolve_exec_workspace(
        &self,
        input: &ExecCommandInput,
    ) -> Result<ResolvedExecWorkspace, CommandServiceError> {
        let handler = self
            .workspace()
            .resolve_session(input.workspace_session_id.clone())?;
        Ok(ResolvedExecWorkspace::new(handler))
    }

    fn command_admission_guard(
        &self,
        workspace: &ResolvedExecWorkspace,
    ) -> Result<Option<MutexGuard<'_, ()>>, CommandServiceError> {
        let guard = self.lock_remount_admission();
        self.ensure_workspace_session_not_remount_pending(&workspace.workspace_session_id)?;
        Ok(Some(guard))
    }

    fn start_command_process(
        &self,
        command_session_id: &CommandSessionId,
        input: &ExecCommandInput,
        workspace: &ResolvedExecWorkspace,
    ) -> Result<StartedCommand, CommandServiceError> {
        let process = self.launch_driver().spawn(
            CommandProcessSpec {
                id: command_session_id.0.clone(),
                command: input.cmd.clone(),
                cwd: None,
                timeout_seconds: input.timeout_seconds,
            },
            workspace.entry()?,
            self.config(),
        )?;
        Ok(StartedCommand::new(process))
    }

    fn initial_exec_yield(
        &self,
        command_session_id: CommandSessionId,
        yield_time_ms: Option<u64>,
    ) -> Result<CommandYield, CommandServiceError> {
        let wait_ms = yield_time_ms.unwrap_or(1000);
        let process = self
            .process_store()
            .active_process(&command_session_id)
            .ok_or_else(|| CommandServiceError::CommandNotFound {
                command_session_id: command_session_id.clone(),
            })?;
        let outcome = self
            .launch_driver()
            .wait_for_initial_yield(process.as_ref(), wait_ms, 0);

        match outcome {
            WaitOutcome::Running(stdout) => {
                Ok(Self::running_command_yield(command_session_id, stdout))
            }
            WaitOutcome::Completed(process_exit) => {
                self.completed_initial_exec_yield(command_session_id, process_exit)
            }
        }
    }

    fn completed_initial_exec_yield(
        &self,
        command_session_id: CommandSessionId,
        process_exit: CommandProcessExit,
    ) -> Result<CommandYield, CommandServiceError> {
        let result = self.finalize_command(command_session_id.clone(), process_exit)?;
        let finalized = self
            .process_store()
            .completed(&command_session_id)
            .and_then(|completed| completed.finalized);
        Ok(CommandYield {
            command_session_id: Some(command_session_id),
            status: result.status,
            exit_code: result.exit_code,
            output: CommandOutputSnapshot {
                stdout: result.stdout,
            },
            finalized,
        })
    }

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
        drop(workspace);
        error
    }
}

struct ResolvedExecWorkspace {
    handler: WorkspaceSessionHandler,
    workspace_session_id: WorkspaceSessionId,
    workspace_root: PathBuf,
}

impl ResolvedExecWorkspace {
    fn new(handler: WorkspaceSessionHandler) -> Self {
        let workspace_session_id = handler.workspace_session_id.clone();
        let workspace_root = handler.handle.workspace_root.clone();
        Self {
            handler,
            workspace_session_id,
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
        workspace: &ResolvedExecWorkspace,
    ) -> (ActiveCommandProcess, Arc<CommandProcess>) {
        let process = Arc::new(self.process);
        let process_for_rollback = Arc::clone(&process);
        let record = ActiveCommandProcess {
            command_session_id,
            workspace_session_id: workspace.workspace_session_id.clone(),
            workspace_root: workspace.workspace_root.clone(),
            process,
            transcript: CommandTranscriptStore {
                transcript_path: self.transcript_path,
            },
            lifecycle_state: CommandLifecycleState::Running,
            cancellation: CancellationState::None,
            remount_cancellation: None,
            remount_switch_state: None,
            finalization: FinalizationState::NotStarted,
        };
        (record, process_for_rollback)
    }
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
