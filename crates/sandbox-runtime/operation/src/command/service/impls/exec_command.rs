use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sandbox_runtime_command::process::{CommandProcess, CommandProcessSpec};

use super::command_yield_response;
use crate::command::service::CommandCompletionPromise;
use crate::command::service::CommandOperationService;
use crate::command::{
    ActiveCommandProcess, CancellationState, CommandLifecycleState, CommandServiceError,
    CommandSessionId, CommandTranscriptStore, CommandWorkspaceOwnership, CommandYield,
    ExecCommandInput, FinalizationState,
};
use crate::observability::{measure_optional, measure_optional_if, span_keys, OperationTrace};
use crate::operation::{ArgCliSpec, ArgKind, ArgSpec, CliOperationSpec, CliSpec};
use crate::workspace_crate::{WorkspaceEntry, WorkspaceSessionId};
use crate::workspace_session::WorkspaceSessionHandler;
use crate::SandboxRuntimeOperations;
use sandbox_protocol::{Request, Response};

use super::super::core::WorkspaceLifecycleAdmission;

pub(crate) const SPEC: CliOperationSpec = CliOperationSpec {
    name: "exec_command",
    family: "command",
    summary: "Start a command in a workspace.",
    description: "Start a shell command inside an existing workspace session when workspace_session_id is provided, otherwise create a one-shot host-compatible workspace and destroy it when the command reaches terminal state. If the command is still running after the initial wait, the response includes a command_session_id that can be used with write_command_stdin or read_command_lines.",
    args: EXEC_COMMAND_ARGS,
    cli: Some(EXEC_COMMAND_CLI),
    related: &[
        "write_command_stdin",
        "read_command_lines",
    ],
};

const EXEC_COMMAND_ARGS: &[ArgSpec] = &[
    ArgSpec::optional(
        "workspace_session_id",
        ArgKind::String,
        "Existing workspace session id to run inside. Omit to run in a one-shot workspace.",
        None,
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
        "timeout_ms",
        ArgKind::Integer,
        "Command timeout in milliseconds.",
        None,
        Some(ArgCliSpec {
            flag: Some("--timeout-ms"),
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
    usage: "sandbox-cli runtime exec_command [--workspace-session-id ID] COMMAND",
    examples: &[
        "sandbox-cli runtime exec_command pwd",
        "sandbox-cli runtime exec_command --workspace-session-id ws-1 pwd",
        "sandbox-cli runtime exec_command --workspace-session-id ws-1 --yield-time-ms 0 \"sleep 30\"",
    ],
};

pub(crate) fn dispatch(
    operations: &SandboxRuntimeOperations,
    request: &Request,
    trace: Option<&OperationTrace>,
) -> Response {
    let input = match parse_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    let origin_request_id = trace.is_some().then(|| request.request_id.clone());
    command_yield_response(measure_optional(
        trace,
        "CommandOperationService::exec_command",
        || {
            operations
                .command
                .exec_command_with_origin_request_id(input, trace, origin_request_id)
        },
    ))
}

fn parse_input(request: &Request) -> Result<ExecCommandInput, Response> {
    Ok(ExecCommandInput {
        workspace_session_id: request
            .optional_string("workspace_session_id")?
            .filter(|workspace_session_id| !workspace_session_id.is_empty())
            .map(WorkspaceSessionId),
        cmd: request.required_string("cmd")?,
        timeout_ms: request.optional_u64("timeout_ms")?,
        yield_time_ms: request.optional_u64("yield_time_ms")?,
    })
}

impl CommandOperationService {
    pub fn exec_command(
        &self,
        input: ExecCommandInput,
        trace: Option<&OperationTrace>,
    ) -> Result<CommandYield, CommandServiceError> {
        self.exec_command_with_origin_request_id(input, trace, None)
    }

    fn exec_command_with_origin_request_id(
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
        let started =
            match self.start_command_process(&command_session_id, &input, &workspace, trace) {
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
        let completion = CommandCompletionPromise::new(
            command_session_id.clone(),
            self.completion_sender().clone(),
            origin_request_id,
        );
        let (record, process_for_rollback) =
            started.into_active_record(command_session_id.clone(), &workspace, completion.clone());
        if let Err(error) = self.process_store().insert_active(reservation, record) {
            process_for_rollback.cancel_process();
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
        workspace: &ResolvedExecWorkspace,
        completion: CommandCompletionPromise,
    ) -> (ActiveCommandProcess, Arc<CommandProcess>) {
        let process = Arc::new(self.process);
        let process_for_rollback = Arc::clone(&process);
        let record = ActiveCommandProcess {
            command_session_id,
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
