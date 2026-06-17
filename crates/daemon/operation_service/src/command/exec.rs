use std::time::Instant;

use crate::command::{
    ActiveCommandProcess, CancellationState, CommandCallContext, CommandFinalizePolicy, CommandId,
    CommandLifecycleState, CommandOutputSnapshot, CommandServiceError, CommandStatus,
    CommandTraceOrigin, CommandTranscriptStore, CommandYield, ExecCommandInput, FinalizationState,
};
use crate::workspace_crate::{DestroyWorkspaceRequest, NetworkMode, WorkspaceId};
use crate::workspace_manager::WorkspaceSessionHandler;

use super::service::CommandOperationService;

impl CommandOperationService {
    pub fn exec_command(
        &self,
        input: ExecCommandInput,
        workspace: Option<WorkspaceSessionHandler>,
        context: CommandCallContext,
    ) -> Result<CommandYield, CommandServiceError> {
        if input.cmd.trim().is_empty() {
            return Err(CommandServiceError::InvalidCommand {
                message: "cmd must be non-empty".to_owned(),
            });
        }
        if input.caller_id != context.caller_id {
            return Err(CommandServiceError::InvalidCommand {
                message: "exec caller must match command call context".to_owned(),
            });
        }
        if let Some(handler) = &workspace {
            if handler.handle.workspace_root != input.workspace_root {
                return Err(CommandServiceError::WorkspaceRootMismatch {
                    expected: handler.handle.workspace_root.clone(),
                    actual: input.workspace_root.clone(),
                });
            }
        }

        let command_id = self.process_store().allocate_command_id();
        let reservation = self.process_store().try_reserve()?;
        let is_session_command = workspace.is_some();
        let handler = match workspace {
            Some(handler) => handler,
            None => self.workspace().create_private_workspace(
                context.caller_id.clone(),
                input.workspace_root.clone(),
                NetworkMode::Host,
            )?,
        };
        let workspace_id = handler.workspace_id.clone();
        let finalize_policy = finalize_policy(is_session_command, &workspace_id);

        if let Err(error) = self
            .registry()
            .bind(command_id.clone(), workspace_id.clone())
        {
            return Err(self.cleanup_one_shot_workspace_after_start_failure(
                &command_id,
                is_session_command,
                handler,
                error,
            ));
        }
        let record = ActiveCommandProcess {
            command_id: command_id.clone(),
            caller_id: context.caller_id.clone(),
            workspace_id,
            process: ::command::CommandProcess::new(::command::CommandProcessSpec {
                id: command_id.0.clone(),
                caller_id: context.caller_id.0.clone(),
                command: input.cmd,
                timeout_seconds: input.timeout_seconds,
            }),
            transcript: CommandTranscriptStore {
                transcript_path: Some(
                    self.config()
                        .scratch_root
                        .join(&command_id.0)
                        .join("transcript.log"),
                ),
            },
            finalize_policy,
            lifecycle_state: CommandLifecycleState::Running,
            cancellation: CancellationState::None,
            finalization: FinalizationState::NotStarted,
            trace_origin: CommandTraceOrigin,
            started_at: Instant::now(),
        };
        if let Err(error) = self.process_store().insert_active(reservation, record) {
            let _ = self.registry().unbind(&command_id);
            return Err(self.cleanup_one_shot_workspace_after_start_failure(
                &command_id,
                is_session_command,
                handler,
                error,
            ));
        }

        Ok(CommandYield {
            command_id: Some(command_id),
            status: CommandStatus::Running,
            exit_code: None,
            output: CommandOutputSnapshot::default(),
            finalized: None,
        })
    }

    fn cleanup_one_shot_workspace_after_start_failure(
        &self,
        command_id: &CommandId,
        is_session_command: bool,
        handler: WorkspaceSessionHandler,
        error: CommandServiceError,
    ) -> CommandServiceError {
        if is_session_command {
            return error;
        }

        match self
            .workspace()
            .destroy(handler, DestroyWorkspaceRequest::default())
        {
            Ok(_) => error,
            Err(cleanup_error) => CommandServiceError::OneShotWorkspaceCleanupFailed {
                command_id: command_id.clone(),
                command_error: Box::new(error),
                cleanup_error,
            },
        }
    }
}

fn finalize_policy(is_session_command: bool, workspace_id: &WorkspaceId) -> CommandFinalizePolicy {
    if is_session_command {
        CommandFinalizePolicy::Session {
            workspace_id: workspace_id.clone(),
        }
    } else {
        CommandFinalizePolicy::OneShotPublishThenDestroy {
            workspace_id: workspace_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use crate::command::{
        ActiveCommandProcess, CancellationState, CommandCallContext, CommandFinalizePolicy,
        CommandId, CommandLifecycleState, CommandProcessStore, CommandServiceError,
        CommandTraceOrigin, CommandTranscriptStore, ExecCommandInput, FinalizationState,
        OperationTraceContext,
    };
    use crate::workspace_crate::{
        BaseRevision, CallerId, CaptureChangesRequest, CapturedWorkspaceChanges,
        CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult,
        LatestSnapshotRequest, LayerStackSnapshotRef, LeaseId, NetworkMode, ReadonlySnapshotHandle,
        RemountWorkspaceRequest, RemountWorkspaceResult, WorkspaceError, WorkspaceHandle,
        WorkspaceId, WorkspaceService,
    };
    use crate::workspace_manager::{WorkspaceManagerService, WorkspaceSessionHandler};

    use super::CommandOperationService;

    struct FakeWorkspaceService {
        create_results: Mutex<VecDeque<Result<WorkspaceHandle, WorkspaceError>>>,
        create_requests: Mutex<Vec<CreateWorkspaceRequest>>,
        destroy_calls: Mutex<Vec<WorkspaceId>>,
    }

    impl FakeWorkspaceService {
        fn new() -> Self {
            Self {
                create_results: Mutex::new(VecDeque::new()),
                create_requests: Mutex::new(Vec::new()),
                destroy_calls: Mutex::new(Vec::new()),
            }
        }

        fn push_create_result(&self, result: Result<WorkspaceHandle, WorkspaceError>) {
            self.create_results
                .lock()
                .expect("test operation succeeds")
                .push_back(result);
        }

        fn create_requests(&self) -> Vec<CreateWorkspaceRequest> {
            self.create_requests
                .lock()
                .expect("test operation succeeds")
                .clone()
        }

        fn destroy_calls(&self) -> Vec<WorkspaceId> {
            self.destroy_calls
                .lock()
                .expect("test operation succeeds")
                .clone()
        }
    }

    impl WorkspaceService for FakeWorkspaceService {
        fn create_workspace(
            &self,
            request: CreateWorkspaceRequest,
        ) -> Result<WorkspaceHandle, WorkspaceError> {
            self.create_requests
                .lock()
                .expect("test operation succeeds")
                .push(request);
            self.create_results
                .lock()
                .expect("test operation succeeds")
                .pop_front()
                .unwrap_or_else(|| {
                    Err(WorkspaceError::Setup {
                        step: "create result not configured".to_owned(),
                    })
                })
        }

        fn capture_changes(
            &self,
            _handle: &WorkspaceHandle,
            _request: CaptureChangesRequest,
        ) -> Result<CapturedWorkspaceChanges, WorkspaceError> {
            Err(WorkspaceError::Capture {
                message: "capture result not configured".to_owned(),
            })
        }

        fn remount_workspace(
            &self,
            _handle: &WorkspaceHandle,
            _request: RemountWorkspaceRequest,
        ) -> Result<RemountWorkspaceResult, WorkspaceError> {
            Err(WorkspaceError::Setup {
                step: "remount result not configured".to_owned(),
            })
        }

        fn destroy_workspace(
            &self,
            handle: WorkspaceHandle,
            _request: DestroyWorkspaceRequest,
        ) -> Result<DestroyWorkspaceResult, WorkspaceError> {
            self.destroy_calls
                .lock()
                .expect("test operation succeeds")
                .push(handle.id.clone());
            Ok(DestroyWorkspaceResult {
                workspace_id: handle.id,
                owner: handle.owner,
                evicted_upperdir_bytes: 0,
                lifetime_s: 0.0,
                lease_released: Some(true),
                lease_release_error: None,
                active_leases_after: 0,
            })
        }

        fn latest_snapshot(
            &self,
            _request: LatestSnapshotRequest,
        ) -> Result<ReadonlySnapshotHandle, WorkspaceError> {
            Err(WorkspaceError::SnapshotAcquire {
                source: "latest snapshot not configured".to_owned(),
            })
        }
    }

    fn command_service(
        fake: Arc<FakeWorkspaceService>,
        process_store: CommandProcessStore,
    ) -> CommandOperationService {
        let workspace = Arc::new(WorkspaceManagerService::new(fake));
        CommandOperationService::with_process_store_for_test(
            workspace,
            command::CommandConfig::default(),
            process_store,
        )
    }

    fn exec_input(caller_id: &str, workspace_root: PathBuf) -> ExecCommandInput {
        ExecCommandInput {
            caller_id: CallerId(caller_id.to_owned()),
            workspace_root,
            workspace_id: None,
            cmd: "printf ok".to_owned(),
            cwd: None,
            timeout_seconds: None,
            yield_time_ms: Some(0),
        }
    }

    fn context(caller_id: &str) -> CommandCallContext {
        CommandCallContext {
            caller_id: CallerId(caller_id.to_owned()),
            trace: OperationTraceContext,
        }
    }

    fn workspace_handle(
        workspace_id: &str,
        caller_id: &str,
        workspace_root: PathBuf,
    ) -> WorkspaceHandle {
        let snapshot = LayerStackSnapshotRef {
            lease_id: LeaseId("lease-1".to_owned()),
            manifest_version: 1,
            root_hash: "root".to_owned(),
            layer_paths: vec![PathBuf::from("/lower/one")],
        };
        WorkspaceHandle {
            id: WorkspaceId(workspace_id.to_owned()),
            owner: CallerId(caller_id.to_owned()),
            workspace_root,
            network: NetworkMode::Host,
            base_revision: BaseRevision {
                version: 1,
                root_hash: "root".to_owned(),
                layer_count: 1,
            },
            snapshot,
        }
    }

    fn session_handler(workspace_id: &str, caller_id: &str) -> WorkspaceSessionHandler {
        let workspace_root = PathBuf::from("/workspace/session");
        let fake = Arc::new(FakeWorkspaceService::new());
        fake.push_create_result(Ok(workspace_handle(
            workspace_id,
            caller_id,
            workspace_root.clone(),
        )));
        let workspace = WorkspaceManagerService::new(fake);
        workspace
            .create_private_workspace(
                CallerId(caller_id.to_owned()),
                workspace_root,
                NetworkMode::Host,
            )
            .expect("test session create succeeds")
    }

    fn active_record(command_id: CommandId, workspace_id: WorkspaceId) -> ActiveCommandProcess {
        let caller_id = CallerId("caller-1".to_owned());
        ActiveCommandProcess {
            command_id: command_id.clone(),
            caller_id: caller_id.clone(),
            workspace_id: workspace_id.clone(),
            process: command::CommandProcess::new(command::CommandProcessSpec {
                id: command_id.0.clone(),
                caller_id: caller_id.0.clone(),
                command: "cat".to_owned(),
                timeout_seconds: None,
            }),
            transcript: CommandTranscriptStore::default(),
            finalize_policy: CommandFinalizePolicy::Session { workspace_id },
            lifecycle_state: CommandLifecycleState::Running,
            cancellation: CancellationState::None,
            finalization: FinalizationState::NotStarted,
            trace_origin: CommandTraceOrigin,
            started_at: Instant::now(),
        }
    }

    #[test]
    fn command_exec_rejects_direct_handler_root_mismatch_before_command_allocation() {
        let fake = Arc::new(FakeWorkspaceService::new());
        let service = command_service(fake, CommandProcessStore::new());

        let error = service
            .exec_command(
                exec_input("caller-1", PathBuf::from("/workspace/other")),
                Some(session_handler("workspace-session", "caller-1")),
                context("caller-1"),
            )
            .expect_err("direct command service rejects root mismatch");

        assert!(matches!(
            error,
            CommandServiceError::WorkspaceRootMismatch { expected, actual }
                if expected.as_path() == Path::new("/workspace/session")
                    && actual.as_path() == Path::new("/workspace/other")
        ));
        assert_eq!(
            service.process_store().allocate_command_id(),
            CommandId("cmd_1".to_owned())
        );
    }

    #[test]
    fn command_exec_admission_failure_does_not_create_private_workspace() {
        let fake = Arc::new(FakeWorkspaceService::new());
        let service = command_service(Arc::clone(&fake), CommandProcessStore::with_max_active(0));

        let error = service
            .exec_command(
                exec_input("caller-1", PathBuf::from("/workspace/one-shot")),
                None,
                context("caller-1"),
            )
            .expect_err("admission limit rejects before create");

        assert!(matches!(
            error,
            CommandServiceError::CommandAdmissionLimit { active: 0, max: 0 }
        ));
        assert!(fake.create_requests().is_empty());
        assert!(fake.destroy_calls().is_empty());
    }

    #[test]
    fn command_exec_create_failure_does_not_register_one_shot_command() {
        let fake = Arc::new(FakeWorkspaceService::new());
        let service = command_service(Arc::clone(&fake), CommandProcessStore::new());

        let error = service
            .exec_command(
                exec_input("caller-1", PathBuf::from("/workspace/one-shot")),
                None,
                context("caller-1"),
            )
            .expect_err("workspace create failure rejects exec");

        assert!(matches!(error, CommandServiceError::WorkspaceManager(_)));
        assert_eq!(fake.create_requests().len(), 1);
        assert!(fake.destroy_calls().is_empty());
        assert!(service
            .registry()
            .workspace_for(&CommandId("cmd_1".to_owned()))
            .is_none());
    }

    #[test]
    fn command_exec_bind_failure_destroys_created_one_shot_workspace() {
        let fake = Arc::new(FakeWorkspaceService::new());
        fake.push_create_result(Ok(workspace_handle(
            "workspace-one-shot",
            "caller-1",
            PathBuf::from("/workspace/one-shot"),
        )));
        let service = command_service(Arc::clone(&fake), CommandProcessStore::new());
        service
            .registry()
            .bind(
                CommandId("cmd_1".to_owned()),
                WorkspaceId("workspace-existing".to_owned()),
            )
            .expect("seed duplicate registry binding");

        let error = service
            .exec_command(
                exec_input("caller-1", PathBuf::from("/workspace/one-shot")),
                None,
                context("caller-1"),
            )
            .expect_err("duplicate bind rejects exec");

        assert!(matches!(
            error,
            CommandServiceError::DuplicateCommandId { command_id }
                if command_id == CommandId("cmd_1".to_owned())
        ));
        assert_eq!(
            fake.destroy_calls(),
            vec![WorkspaceId("workspace-one-shot".to_owned())]
        );
    }

    #[test]
    fn command_exec_active_insert_failure_destroys_one_shot_and_unbinds_registry() {
        let fake = Arc::new(FakeWorkspaceService::new());
        fake.push_create_result(Ok(workspace_handle(
            "workspace-one-shot",
            "caller-1",
            PathBuf::from("/workspace/one-shot"),
        )));
        let service = command_service(Arc::clone(&fake), CommandProcessStore::new());
        let command_id = CommandId("cmd_1".to_owned());
        let reservation = service
            .process_store()
            .try_reserve()
            .expect("seed reservation succeeds");
        service
            .process_store()
            .insert_active(
                reservation,
                active_record(
                    command_id.clone(),
                    WorkspaceId("workspace-existing".to_owned()),
                ),
            )
            .expect("seed active command");

        let error = service
            .exec_command(
                exec_input("caller-1", PathBuf::from("/workspace/one-shot")),
                None,
                context("caller-1"),
            )
            .expect_err("duplicate active insert rejects exec");

        assert!(matches!(
            error,
            CommandServiceError::DuplicateCommandId { command_id: duplicate }
                if duplicate == command_id
        ));
        assert_eq!(
            fake.destroy_calls(),
            vec![WorkspaceId("workspace-one-shot".to_owned())]
        );
        assert!(service.registry().workspace_for(&command_id).is_none());
    }
}
