use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sandbox_runtime_namespace_execution::{
    NamespaceExecutionError, NamespaceExecutionId, NamespaceTarget,
};

use crate::command::service::exec::ExecCommand;
use crate::command::service::CommandOperationService;
use crate::command::{
    CommandExecValue, CommandOutput, CommandServiceError, CommandTerminalResult, ExecCommandInput,
};
use crate::timing;
use crate::workspace_crate::{DestroyWorkspaceRequest, WorkspaceEntry, WorkspaceSessionId};
use crate::workspace_session::{WorkspaceSessionHandler, WorkspaceSessionService};

impl CommandOperationService {
    pub fn exec_command(
        &self,
        input: ExecCommandInput,
    ) -> Result<CommandOutput, CommandServiceError> {
        if input.cmd.trim().is_empty() {
            return Err(CommandServiceError::InvalidCommand {
                message: "cmd must be non-empty".to_owned(),
            });
        }
        let total_started = Instant::now();
        let existing_lifecycle_guard = input
            .workspace_session_id
            .is_some()
            .then(|| self.lock_session_lifecycle());
        let resolve_started = Instant::now();
        let workspace = self.resolve_exec_workspace(&input)?;
        timing::duration("runtime.exec.resolve_workspace", resolve_started);
        let lifecycle_guard =
            existing_lifecycle_guard.unwrap_or_else(|| self.lock_session_lifecycle());

        let id = self.engine().allocate_id();

        let prepare_started = Instant::now();
        let (entry, transcript_path) = match workspace
            .entry()
            .and_then(|entry| self.prepare_transcript_path(&id).map(|path| (entry, path)))
        {
            Ok(pair) => pair,
            Err(error) => return Err(self.fail_command_start(&id, workspace, error)),
        };
        timing::duration("runtime.exec.prepare", prepare_started);

        let started_at = Instant::now();
        let exec_command = ExecCommand {
            command: input.cmd.clone(),
            timeout_seconds: input.timeout_ms.map(|ms| ms as f64 / 1000.0),
            transcript_path: transcript_path.clone(),
            started_at,
        };
        let on_complete = workspace.finalize_closure(self.workspace_handle().clone());
        let target = NamespaceTarget::from(entry);

        let cgroup_procs_path = workspace
            .cgroup_path
            .as_ref()
            .map(|cgroup| cgroup.join("cgroup.procs"));
        let spawn_started = Instant::now();
        let exec = self.engine().run_shell_interactive(
            exec_command,
            target,
            id.clone(),
            on_complete,
            cgroup_procs_path,
        );
        timing::duration("runtime.exec.spawn_interactive", spawn_started);
        let exec = match exec {
            Ok(exec) => exec,
            Err(error) => {
                let error = CommandServiceError::CommandIo {
                    command_session_id: id.clone(),
                    error: error.to_string(),
                };
                self.cleanup_transcript_dir(&id);
                return Err(self.fail_command_start(&id, workspace, error));
            }
        };

        self.engine().attach(
            &id,
            CommandExecValue::new(
                exec,
                transcript_path,
                workspace.workspace_session_id.clone(),
                started_at,
                "exec_command",
            ),
        );
        drop(lifecycle_guard);

        let yield_started = Instant::now();
        let output =
            self.wait_for_command_yield(id.clone(), input.yield_time_ms.unwrap_or(1000), 0, false);
        timing::duration("runtime.exec.wait_yield", yield_started);
        timing::duration("runtime.exec.total", total_started);
        output
    }

    fn resolve_exec_workspace(
        &self,
        input: &ExecCommandInput,
    ) -> Result<ResolvedExecWorkspace, CommandServiceError> {
        let handler = if let Some(workspace_session_id) = &input.workspace_session_id {
            self.resolve_workspace_session(workspace_session_id.clone())?
        } else {
            self.create_one_shot_workspace_session()?
        };
        Ok(ResolvedExecWorkspace {
            workspace_session_id: handler.workspace_session_id.clone(),
            cgroup_path: handler.cgroup_path.clone(),
            handler,
            one_shot: input.workspace_session_id.is_none(),
        })
    }

    fn prepare_transcript_path(
        &self,
        id: &NamespaceExecutionId,
    ) -> Result<PathBuf, CommandServiceError> {
        let command_dir = self.config().scratch_root.join(&id.0);
        std::fs::create_dir_all(&command_dir).map_err(|error| CommandServiceError::CommandIo {
            command_session_id: id.clone(),
            error: error.to_string(),
        })?;
        Ok(command_dir.join("transcript.log"))
    }

    fn cleanup_transcript_dir(&self, id: &NamespaceExecutionId) {
        let command_dir = self.config().scratch_root.join(&id.0);
        let _ = std::fs::remove_dir_all(command_dir);
    }

    fn fail_command_start(
        &self,
        id: &NamespaceExecutionId,
        workspace: ResolvedExecWorkspace,
        error: CommandServiceError,
    ) -> CommandServiceError {
        if !workspace.one_shot {
            return error;
        }
        match self.destroy_one_shot_workspace_session(workspace.handler) {
            Ok(_) => error,
            Err(cleanup_error) => CommandServiceError::OneShotSessionCleanupFailed {
                command_session_id: id.clone(),
                command_error: Box::new(error),
                cleanup_error: cleanup_error.to_string(),
            },
        }
    }
}

struct ResolvedExecWorkspace {
    handler: WorkspaceSessionHandler,
    workspace_session_id: WorkspaceSessionId,
    one_shot: bool,
    cgroup_path: Option<PathBuf>,
}

impl ResolvedExecWorkspace {
    fn entry(&self) -> Result<WorkspaceEntry, CommandServiceError> {
        self.handler
            .handle
            .entry()
            .map_err(|error| CommandServiceError::InvalidCommand {
                message: error.to_string(),
            })
    }

    /// Build the engine `on_complete` closure: once the child reaches a terminal
    /// state, apply the one-shot teardown policy. The one-shot decision is read
    /// from `self.one_shot` exactly once; teardown errors stay internal to
    /// finalization and never surface in the command result.
    fn finalize_closure(
        &self,
        workspace: Arc<WorkspaceSessionService>,
    ) -> impl FnOnce(&Result<CommandTerminalResult, NamespaceExecutionError>) + Send + 'static {
        let one_shot_handler = self.one_shot.then(|| self.handler.clone());
        move |_result| {
            if let Some(handler) = one_shot_handler {
                let _ = workspace.destroy_session(handler, DestroyWorkspaceRequest::default());
            }
        }
    }
}
