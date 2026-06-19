use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use command::process::{
    CommandProcess, CommandProcessExit, CommandProcessSpawn, CommandProcessSpec,
};
use command::yield_wait_loop::WaitOutcome;

use crate::command::{
    ActiveCommandProcess, CancellationState, CommandCallContext, CommandFinalizePolicy, CommandId,
    CommandLifecycleState, CommandOutputSnapshot, CommandServiceError, CommandStatus,
    CommandTraceOrigin, CommandTranscriptStore, CommandYield, ExecCommandInput, FinalizationState,
};
use crate::workspace_crate::{
    DestroyWorkspaceRequest, WorkspaceCommandRunRequest, WorkspaceId, WorkspaceProfile,
};
use crate::workspace_manager::WorkspaceSessionHandler;

use super::service::CommandOperationService;

impl CommandOperationService {
    pub fn exec_command(
        &self,
        input: ExecCommandInput,
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

        let mode = match input.workspace_id.clone() {
            Some(workspace_id) => {
                let handler = self
                    .workspace()
                    .resolve(workspace_id, context.caller_id.clone())?;
                validate_workspace_root(&input, &handler)?;
                ExecCommandMode::Session {
                    handler: Box::new(handler),
                }
            }
            None => ExecCommandMode::OneShot,
        };

        self.exec_resolved_command(input, mode, context)
    }

    fn exec_resolved_command(
        &self,
        input: ExecCommandInput,
        mode: ExecCommandMode,
        context: CommandCallContext,
    ) -> Result<CommandYield, CommandServiceError> {
        let is_session_command = mode.is_session();
        let handler = match mode {
            ExecCommandMode::Session { handler } => *handler,
            ExecCommandMode::OneShot => self.workspace().create_private_workspace(
                context.caller_id.clone(),
                input.workspace_root.clone(),
                WorkspaceProfile::HostCompatible,
            )?,
        };
        let admission_guard = if is_session_command {
            Some(self.lock_remount_admission())
        } else {
            None
        };
        if is_session_command && self.workspace().is_remount_pending(&handler.workspace_id) {
            return Err(CommandServiceError::WorkspaceRemountPending {
                workspace_id: handler.workspace_id.clone(),
            });
        }
        let command_id = self.process_store().allocate_command_id();
        let reservation = match self.process_store().try_reserve() {
            Ok(reservation) => reservation,
            Err(error) => {
                return Err(self.cleanup_start_failure(
                    &command_id,
                    is_session_command,
                    handler,
                    None,
                    error,
                ));
            }
        };
        let workspace_id = handler.workspace_id.clone();
        let workspace_root = handler.handle.workspace_root.clone();
        let finalize_policy = finalize_policy(is_session_command, &workspace_id);
        let launch = match self.prepare_launch_context(&input, &handler, &command_id) {
            Ok(launch) => launch,
            Err(error) => {
                return Err(self.cleanup_start_failure(
                    &command_id,
                    is_session_command,
                    handler,
                    None,
                    error,
                ));
            }
        };
        let process = match self.spawn_command_process(&command_id, &input, &launch) {
            Ok(process) => process,
            Err(error) => {
                return Err(self.cleanup_start_failure(
                    &command_id,
                    is_session_command,
                    handler,
                    Some(&launch),
                    error,
                ));
            }
        };

        if let Err(error) = self
            .registry()
            .bind(command_id.clone(), workspace_id.clone())
        {
            process.cancel_process();
            return Err(self.cleanup_start_failure(
                &command_id,
                is_session_command,
                handler,
                Some(&launch),
                error,
            ));
        }
        if self.process_store().active(&command_id).is_some() {
            let _ = self.registry().unbind(&command_id);
            process.cancel_process();
            return Err(self.cleanup_start_failure(
                &command_id,
                is_session_command,
                handler,
                Some(&launch),
                CommandServiceError::DuplicateCommandId {
                    command_id: command_id.clone(),
                },
            ));
        }
        let process = Arc::new(process);
        let process_for_rollback = Arc::clone(&process);
        let record = ActiveCommandProcess {
            command_id: command_id.clone(),
            caller_id: context.caller_id.clone(),
            workspace_id,
            workspace_root,
            process,
            transcript: CommandTranscriptStore {
                transcript_path: Some(launch.transcript_path.clone()),
            },
            finalize_policy,
            lifecycle_state: CommandLifecycleState::Running,
            cancellation: CancellationState::None,
            remount_cancellation: None,
            remount_switch_state: None,
            finalization: FinalizationState::NotStarted,
            trace_origin: CommandTraceOrigin,
            started_at: Instant::now(),
        };
        if let Err(error) = self.process_store().insert_active(reservation, record) {
            let _ = self.registry().unbind(&command_id);
            process_for_rollback.cancel_process();
            return Err(self.cleanup_start_failure(
                &command_id,
                is_session_command,
                handler,
                Some(&launch),
                error,
            ));
        }
        drop(admission_guard);

        self.initial_exec_yield(command_id, input.yield_time_ms)
    }

    fn prepare_launch_context(
        &self,
        input: &ExecCommandInput,
        handler: &WorkspaceSessionHandler,
        command_id: &CommandId,
    ) -> Result<PreparedCommandLaunch, CommandServiceError> {
        let command_dir = self.config().scratch_root.join(&command_id.0);
        fs::create_dir_all(&command_dir).map_err(|error| CommandServiceError::CommandIo {
            command_id: command_id.clone(),
            error: format!("prepare command artifact directory: {error}"),
        })?;
        let run_request = handler
            .handle
            .command_run_request(WorkspaceCommandRunRequest {
                command_id: command_id.0.clone(),
                caller_id: input.caller_id.0.clone(),
                command: input.cmd.clone(),
                cwd: input.cwd.clone(),
                timeout_seconds: input.timeout_seconds,
            })
            .map_err(|error| CommandServiceError::InvalidCommand {
                message: error.to_string(),
            })?;
        Ok(PreparedCommandLaunch::new(command_dir, run_request))
    }

    fn spawn_command_process(
        &self,
        command_id: &CommandId,
        input: &ExecCommandInput,
        launch: &PreparedCommandLaunch,
    ) -> Result<CommandProcess, CommandServiceError> {
        self.launch_driver().spawn(
            CommandProcessSpec {
                id: command_id.0.clone(),
                caller_id: input.caller_id.0.clone(),
                command: input.cmd.clone(),
                timeout_seconds: input.timeout_seconds,
            },
            CommandProcessSpawn {
                run_request: launch.run_request.clone(),
                request_path: launch.request_path.clone(),
                output_path: launch.output_path.clone(),
                final_path: launch.final_path.clone(),
                transcript_path: launch.transcript_path.clone(),
                transcript_timestamp_timezone: &self.config().transcript_timestamp_timezone,
                output_drain_grace_ms: self.config().output_drain_grace_ms,
            },
        )
    }

    fn initial_exec_yield(
        &self,
        command_id: CommandId,
        yield_time_ms: Option<u64>,
    ) -> Result<CommandYield, CommandServiceError> {
        let wait_ms = yield_time_ms.unwrap_or(self.config().default_yield_time_ms);
        let process = self
            .process_store()
            .active_process(&command_id)
            .ok_or_else(|| CommandServiceError::CommandNotFound {
                command_id: command_id.clone(),
            })?;
        let outcome = self.launch_driver().wait_for_initial_yield(
            process.as_ref(),
            self.config(),
            wait_ms,
            0,
        );

        match outcome {
            WaitOutcome::Running(stdout) => Ok(CommandYield {
                command_id: Some(command_id),
                status: CommandStatus::Running,
                exit_code: None,
                output: CommandOutputSnapshot { stdout },
                finalized: None,
            }),
            WaitOutcome::Completed(process_exit) => {
                self.completed_initial_exec_yield(command_id, process_exit)
            }
        }
    }

    fn completed_initial_exec_yield(
        &self,
        command_id: CommandId,
        process_exit: CommandProcessExit,
    ) -> Result<CommandYield, CommandServiceError> {
        let result = self.finalize_command(command_id.clone(), process_exit)?;
        let finalized = self
            .process_store()
            .completed(&command_id)
            .and_then(|completed| completed.finalized);
        Ok(CommandYield {
            command_id: Some(command_id),
            status: result.status,
            exit_code: result.exit_code,
            output: CommandOutputSnapshot {
                stdout: result.stdout,
            },
            finalized,
        })
    }

    fn cleanup_start_failure(
        &self,
        command_id: &CommandId,
        is_session_command: bool,
        handler: WorkspaceSessionHandler,
        launch: Option<&PreparedCommandLaunch>,
        error: CommandServiceError,
    ) -> CommandServiceError {
        let error = if let Some(launch) = launch {
            launch.cleanup_artifacts_after_start_failure(command_id, error)
        } else {
            error
        };
        self.cleanup_one_shot_workspace_after_start_failure(
            command_id,
            is_session_command,
            handler,
            error,
        )
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

enum ExecCommandMode {
    Session {
        handler: Box<WorkspaceSessionHandler>,
    },
    OneShot,
}

impl ExecCommandMode {
    fn is_session(&self) -> bool {
        matches!(self, Self::Session { .. })
    }
}

struct PreparedCommandLaunch {
    command_dir: PathBuf,
    run_request: serde_json::Value,
    request_path: PathBuf,
    output_path: PathBuf,
    final_path: PathBuf,
    transcript_path: PathBuf,
}

impl PreparedCommandLaunch {
    fn new(command_dir: PathBuf, run_request: serde_json::Value) -> Self {
        Self {
            command_dir: command_dir.clone(),
            run_request,
            request_path: command_dir.join("runner-request.json"),
            output_path: command_dir.join("runner-result.json"),
            final_path: command_dir.join("final.json"),
            transcript_path: command_dir.join("transcript.log"),
        }
    }

    fn cleanup_artifacts_after_start_failure(
        &self,
        command_id: &CommandId,
        error: CommandServiceError,
    ) -> CommandServiceError {
        match fs::remove_dir_all(&self.command_dir) {
            Ok(()) => error,
            Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => error,
            Err(cleanup_error) => CommandServiceError::CommandArtifactCleanupFailed {
                command_id: command_id.clone(),
                command_error: Box::new(error),
                artifact_dir: self.command_dir.clone(),
                cleanup_error: cleanup_error.to_string(),
            },
        }
    }
}

fn validate_workspace_root(
    input: &ExecCommandInput,
    handler: &WorkspaceSessionHandler,
) -> Result<(), CommandServiceError> {
    if handler.handle.workspace_root != input.workspace_root {
        return Err(CommandServiceError::WorkspaceRootMismatch {
            expected: handler.handle.workspace_root.clone(),
            actual: input.workspace_root.clone(),
        });
    }

    Ok(())
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use command::process::{
        CommandProcess, CommandProcessExit, CommandProcessSpawn, CommandProcessSpec,
    };
    use command::yield_wait_loop::WaitOutcome;

    use crate::command::{
        ActiveCommandProcess, CancellationState, CommandCallContext, CommandFinalizePolicy,
        CommandId, CommandLaunchDriver, CommandLifecycleState, CommandProcessStore,
        CommandServiceError, CommandTraceOrigin, CommandTranscriptStore, ExecCommandInput,
        FinalizationState, OperationTraceContext,
    };
    use crate::workspace_crate::{
        CallerId, CaptureChangesRequest, CapturedWorkspaceChanges, CreateWorkspaceRequest,
        DestroyWorkspaceRequest, DestroyWorkspaceResult, LatestSnapshotRequest,
        LayerStackSnapshotRef, LeaseId, ReadonlySnapshotHandle, RemountWorkspaceRequest,
        RemountWorkspaceResult, WorkspaceError, WorkspaceHandle, WorkspaceId, WorkspaceProfile,
        WorkspaceService,
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

    #[derive(Debug, Default)]
    struct FakeLaunchDriver {
        outcomes: Mutex<VecDeque<WaitOutcome<CommandProcessExit>>>,
        spawn_errors: Mutex<VecDeque<CommandServiceError>>,
    }

    impl FakeLaunchDriver {
        fn new() -> Self {
            Self::default()
        }
    }

    impl CommandLaunchDriver for FakeLaunchDriver {
        fn spawn(
            &self,
            spec: CommandProcessSpec,
            _parts: CommandProcessSpawn<'_>,
        ) -> Result<CommandProcess, CommandServiceError> {
            if let Some(error) = self
                .spawn_errors
                .lock()
                .expect("test operation succeeds")
                .pop_front()
            {
                return Err(error);
            }
            Ok(CommandProcess::inactive_for_test(spec))
        }

        fn wait_for_initial_yield(
            &self,
            _process: &CommandProcess,
            _config: &command::CommandConfig,
            _yield_time_ms: u64,
            _start_offset: u64,
        ) -> WaitOutcome<CommandProcessExit> {
            self.outcomes
                .lock()
                .expect("test operation succeeds")
                .pop_front()
                .unwrap_or_else(|| WaitOutcome::Running(String::new()))
        }
    }

    fn command_service(
        fake: Arc<FakeWorkspaceService>,
        process_store: CommandProcessStore,
    ) -> CommandOperationService {
        let workspace = Arc::new(WorkspaceManagerService::new(fake));
        CommandOperationService::with_process_store_and_launch_driver_for_test(
            workspace,
            test_command_config(),
            process_store,
            Arc::new(FakeLaunchDriver::new()),
        )
    }

    fn exec_input(
        caller_id: &str,
        workspace_root: PathBuf,
        workspace_id: Option<WorkspaceId>,
    ) -> ExecCommandInput {
        ExecCommandInput {
            caller_id: CallerId(caller_id.to_owned()),
            workspace_root,
            workspace_id,
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
        let base_dir =
            std::env::temp_dir().join(format!("operation-service-exec-launch-{}", unique_suffix()));
        WorkspaceHandle::holder_backed_for_test(
            WorkspaceId(workspace_id.to_owned()),
            CallerId(caller_id.to_owned()),
            workspace_root,
            WorkspaceProfile::HostCompatible,
            snapshot,
            base_dir.join("upper"),
            base_dir.join("work"),
            None,
        )
    }

    fn test_command_config() -> command::CommandConfig {
        command::CommandConfig {
            scratch_root: std::env::temp_dir().join(format!(
                "operation-service-exec-test-{}-{}",
                std::process::id(),
                unique_suffix()
            )),
            ..command::CommandConfig::default()
        }
    }

    fn unique_suffix() -> u64 {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn create_service_session(
        service: &CommandOperationService,
        fake: &Arc<FakeWorkspaceService>,
        workspace_id: &str,
        caller_id: &str,
        workspace_root: PathBuf,
    ) -> WorkspaceSessionHandler {
        fake.push_create_result(Ok(workspace_handle(
            workspace_id,
            caller_id,
            workspace_root.clone(),
        )));
        service
            .workspace()
            .create_private_workspace(
                CallerId(caller_id.to_owned()),
                workspace_root,
                WorkspaceProfile::HostCompatible,
            )
            .expect("test session create succeeds")
    }

    fn active_record(command_id: CommandId, workspace_id: WorkspaceId) -> ActiveCommandProcess {
        let caller_id = CallerId("caller-1".to_owned());
        ActiveCommandProcess {
            command_id: command_id.clone(),
            caller_id: caller_id.clone(),
            workspace_id: workspace_id.clone(),
            workspace_root: PathBuf::from("/workspace"),
            process: Arc::new(command::CommandProcess::inactive_for_test(
                command::CommandProcessSpec {
                    id: command_id.0.clone(),
                    caller_id: caller_id.0.clone(),
                    command: "cat".to_owned(),
                    timeout_seconds: None,
                },
            )),
            transcript: CommandTranscriptStore::default(),
            finalize_policy: CommandFinalizePolicy::Session { workspace_id },
            lifecycle_state: CommandLifecycleState::Running,
            cancellation: CancellationState::None,
            remount_cancellation: None,
            remount_switch_state: None,
            finalization: FinalizationState::NotStarted,
            trace_origin: CommandTraceOrigin,
            started_at: Instant::now(),
        }
    }

    #[test]
    fn command_exec_rejects_resolved_session_root_mismatch_before_command_allocation() {
        let fake = Arc::new(FakeWorkspaceService::new());
        let service = command_service(Arc::clone(&fake), CommandProcessStore::new());
        let _handler = create_service_session(
            &service,
            &fake,
            "workspace-session",
            "caller-1",
            PathBuf::from("/workspace/session"),
        );

        let error = service
            .exec_command(
                exec_input(
                    "caller-1",
                    PathBuf::from("/workspace/other"),
                    Some(WorkspaceId("workspace-session".to_owned())),
                ),
                context("caller-1"),
            )
            .expect_err("command service rejects root mismatch");

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
    fn command_exec_resolves_workspace_id_with_canonical_session() {
        let fake = Arc::new(FakeWorkspaceService::new());
        let service = command_service(Arc::clone(&fake), CommandProcessStore::new());
        let canonical = create_service_session(
            &service,
            &fake,
            "workspace-session",
            "caller-1",
            PathBuf::from("/workspace/session"),
        );

        let output = service
            .exec_command(
                exec_input(
                    "caller-1",
                    PathBuf::from("/workspace/session"),
                    Some(canonical.workspace_id),
                ),
                context("caller-1"),
            )
            .expect("exec resolves canonical workspace session");

        assert_eq!(output.command_id, Some(CommandId("cmd_1".to_owned())));
        assert!(fake.destroy_calls().is_empty());
    }

    #[test]
    fn command_exec_rejects_unknown_workspace_id_before_command_allocation() {
        let fake = Arc::new(FakeWorkspaceService::new());
        let service = command_service(Arc::clone(&fake), CommandProcessStore::new());

        let error = service
            .exec_command(
                exec_input(
                    "caller-1",
                    PathBuf::from("/workspace/session"),
                    Some(WorkspaceId("workspace-session".to_owned())),
                ),
                context("caller-1"),
            )
            .expect_err("command service rejects unknown workspace id");

        assert!(matches!(error, CommandServiceError::WorkspaceManager(_)));
        assert!(fake.create_requests().is_empty());
        assert_eq!(
            service.process_store().allocate_command_id(),
            CommandId("cmd_1".to_owned())
        );
    }

    #[test]
    fn command_exec_admission_failure_destroys_created_one_shot_workspace() {
        let fake = Arc::new(FakeWorkspaceService::new());
        fake.push_create_result(Ok(workspace_handle(
            "workspace-one-shot",
            "caller-1",
            PathBuf::from("/workspace/one-shot"),
        )));
        let service = command_service(Arc::clone(&fake), CommandProcessStore::with_max_active(0));

        let error = service
            .exec_command(
                exec_input("caller-1", PathBuf::from("/workspace/one-shot"), None),
                context("caller-1"),
            )
            .expect_err("admission limit rejects before create");

        assert!(matches!(
            error,
            CommandServiceError::CommandAdmissionLimit { active: 0, max: 0 }
        ));
        assert_eq!(fake.create_requests().len(), 1);
        assert_eq!(
            fake.destroy_calls(),
            vec![WorkspaceId("workspace-one-shot".to_owned())]
        );
    }

    #[test]
    fn command_exec_create_failure_does_not_register_one_shot_command() {
        let fake = Arc::new(FakeWorkspaceService::new());
        let service = command_service(Arc::clone(&fake), CommandProcessStore::new());

        let error = service
            .exec_command(
                exec_input("caller-1", PathBuf::from("/workspace/one-shot"), None),
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
                exec_input("caller-1", PathBuf::from("/workspace/one-shot"), None),
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
                exec_input("caller-1", PathBuf::from("/workspace/one-shot"), None),
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
