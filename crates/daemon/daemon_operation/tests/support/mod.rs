use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use command::process::{
    CommandProcess, CommandProcessExit, CommandProcessSpawn, CommandProcessSpec,
};
use command::yield_wait_loop::WaitOutcome;
use daemon_operation::command::{
    CommandLaunchDriver, CommandOperationService, CommandServiceError,
};
use daemon_operation::workspace_session::WorkspaceSessionService;
use workspace::{
    CaptureChangesRequest, CapturedWorkspaceChanges, CreateWorkspaceRequest,
    DestroyWorkspaceRequest, DestroyWorkspaceResult, LatestSnapshotRequest, LayerStackSnapshotRef,
    LeaseId, ReadonlySnapshotHandle, RemountWorkspaceRequest, RemountWorkspaceResult,
    WorkspaceEntry, WorkspaceError, WorkspaceHandle, WorkspaceProfile, WorkspaceRuntimeHooks,
    WorkspaceRuntimeService, WorkspaceSessionId,
};

pub struct TestServices {
    pub workspace: Arc<WorkspaceSessionService>,
    pub command: Arc<CommandOperationService>,
}

#[derive(Default)]
pub struct FakeWorkspaceService {
    create_results: Mutex<VecDeque<Result<WorkspaceHandle, WorkspaceError>>>,
    destroy_results: Mutex<VecDeque<Result<DestroyWorkspaceResult, WorkspaceError>>>,
    create_requests: Mutex<Vec<CreateWorkspaceRequest>>,
    destroy_calls: Mutex<Vec<WorkspaceSessionId>>,
}

#[derive(Debug, Default)]
pub struct FakeLaunchDriver {
    outcomes: Mutex<VecDeque<WaitOutcome<CommandProcessExit>>>,
    spawn_errors: Mutex<VecDeque<CommandServiceError>>,
    spawn_observations: Mutex<Vec<SpawnObservation>>,
}

#[derive(Debug, Clone)]
pub struct SpawnObservation {
    pub spec_id: String,
    pub spec_command: String,
    pub spec_cwd: Option<PathBuf>,
    pub spec_timeout_seconds: Option<f64>,
    pub workspace_entry: WorkspaceEntry,
    pub request_path: PathBuf,
    pub output_path: PathBuf,
    pub final_path: PathBuf,
    pub transcript_path: PathBuf,
}

impl FakeLaunchDriver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_outcome(&self, outcome: WaitOutcome<CommandProcessExit>) {
        self.outcomes
            .lock()
            .expect("test operation succeeds")
            .push_back(outcome);
    }

    pub fn push_spawn_error(&self, error: CommandServiceError) {
        self.spawn_errors
            .lock()
            .expect("test operation succeeds")
            .push_back(error);
    }

    pub fn spawn_observations(&self) -> Vec<SpawnObservation> {
        self.spawn_observations
            .lock()
            .expect("test operation succeeds")
            .clone()
    }
}

impl CommandLaunchDriver for FakeLaunchDriver {
    fn spawn(
        &self,
        spec: CommandProcessSpec,
        workspace_entry: WorkspaceEntry,
        config: &command::CommandConfig,
    ) -> Result<CommandProcess, CommandServiceError> {
        if let Some(error) = self
            .spawn_errors
            .lock()
            .expect("test operation succeeds")
            .pop_front()
        {
            return Err(error);
        }
        let parts =
            CommandProcessSpawn::prepare(&spec.id, workspace_entry, config).map_err(|error| {
                CommandServiceError::CommandIo {
                    command_session_id: daemon_operation::command::CommandSessionId(
                        spec.id.clone(),
                    ),
                    error: error.to_string(),
                }
            })?;
        self.spawn_observations
            .lock()
            .expect("test operation succeeds")
            .push(SpawnObservation {
                spec_id: spec.id.clone(),
                spec_command: spec.command.clone(),
                spec_cwd: spec.cwd.clone(),
                spec_timeout_seconds: spec.timeout_seconds,
                workspace_entry: parts.workspace_entry.clone(),
                request_path: parts.request_path.clone(),
                output_path: parts.output_path.clone(),
                final_path: parts.final_path.clone(),
                transcript_path: parts.transcript_path.clone(),
            });
        Ok(CommandProcess::inactive_with_artifacts_for_test(
            spec,
            parts.output_path,
            parts.final_path,
            parts.transcript_path,
        ))
    }

    fn wait_for_initial_yield(
        &self,
        _process: &CommandProcess,
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

impl FakeWorkspaceService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_create_result(&self, result: Result<WorkspaceHandle, WorkspaceError>) {
        self.create_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    pub fn create_requests(&self) -> Vec<CreateWorkspaceRequest> {
        self.create_requests
            .lock()
            .expect("test operation succeeds")
            .clone()
    }

    pub fn destroy_calls(&self) -> Vec<WorkspaceSessionId> {
        self.destroy_calls
            .lock()
            .expect("test operation succeeds")
            .clone()
    }
}

impl FakeWorkspaceService {
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
        self.destroy_results
            .lock()
            .expect("test operation succeeds")
            .pop_front()
            .unwrap_or_else(|| Ok(destroy_result(&handle)))
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

pub fn fake_workspace_runtime(fake: Arc<FakeWorkspaceService>) -> Arc<WorkspaceRuntimeService> {
    Arc::new(WorkspaceRuntimeService::from_hooks_for_test(
        WorkspaceRuntimeHooks {
            create_workspace: Box::new({
                let fake = Arc::clone(&fake);
                move |request| fake.create_workspace(request)
            }),
            capture_changes: Box::new({
                let fake = Arc::clone(&fake);
                move |handle, request| fake.capture_changes(handle, request)
            }),
            remount_workspace: Box::new({
                let fake = Arc::clone(&fake);
                move |handle, request| fake.remount_workspace(handle, request)
            }),
            destroy_workspace: Box::new({
                let fake = Arc::clone(&fake);
                move |handle, request| fake.destroy_workspace(handle, request)
            }),
            latest_snapshot: Box::new(move |request| fake.latest_snapshot(request)),
        },
    ))
}

pub fn build_services(fake: Arc<FakeWorkspaceService>) -> TestServices {
    build_services_with_launch_driver(fake, Arc::new(FakeLaunchDriver::new()))
}

pub fn build_services_with_launch_driver(
    fake: Arc<FakeWorkspaceService>,
    launch_driver: Arc<dyn CommandLaunchDriver>,
) -> TestServices {
    let workspace = Arc::new(WorkspaceSessionService::new(fake_workspace_runtime(fake)));
    let command = Arc::new(CommandOperationService::with_launch_driver_for_test(
        Arc::clone(&workspace),
        test_command_config(),
        launch_driver,
    ));
    TestServices { workspace, command }
}

pub fn create_request(workspace_root: PathBuf) -> CreateWorkspaceRequest {
    CreateWorkspaceRequest {
        workspace_root,
        layer_stack_root: PathBuf::from("/layers"),
        profile: WorkspaceProfile::HostCompatible,
    }
}

pub fn workspace_handle(
    workspace_session_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    profile: WorkspaceProfile,
) -> WorkspaceHandle {
    let base_dir = test_launch_base_dir();
    WorkspaceHandle::holder_backed_for_test(
        WorkspaceSessionId(workspace_session_id.to_owned()),
        workspace_root,
        profile,
        test_snapshot(lease_id),
        base_dir.join("upper"),
        base_dir.join("work"),
        None,
    )
}

pub fn workspace_handle_without_launch(
    workspace_session_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    profile: WorkspaceProfile,
) -> WorkspaceHandle {
    WorkspaceHandle::without_launch_for_test(
        WorkspaceSessionId(workspace_session_id.to_owned()),
        workspace_root,
        profile,
        test_snapshot(lease_id),
    )
}

pub fn workspace_handle_unavailable_launch(
    workspace_session_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    profile: WorkspaceProfile,
) -> WorkspaceHandle {
    let base_dir = test_launch_base_dir();
    WorkspaceHandle::unavailable_for_test(
        WorkspaceSessionId(workspace_session_id.to_owned()),
        workspace_root,
        profile,
        test_snapshot(lease_id),
        base_dir.join("upper"),
        base_dir.join("work"),
        None,
    )
}

pub fn destroy_result(handle: &WorkspaceHandle) -> DestroyWorkspaceResult {
    DestroyWorkspaceResult {
        workspace_session_id: handle.id.clone(),
        evicted_upperdir_bytes: 0,
        lifetime_s: 0.0,
        lease_released: Some(true),
        lease_release_error: None,
        active_leases_after: 0,
    }
}

pub fn success_exit(stdout: &str) -> CommandProcessExit {
    CommandProcessExit {
        status: "completed".to_owned(),
        exit_code: 0,
        signal: None,
        runner_result: None,
        stdout: stdout.to_owned(),
        elapsed_s: 0.1,
        kill: None,
    }
}

fn test_command_config() -> command::CommandConfig {
    command::CommandConfig {
        scratch_root: std::env::temp_dir().join(format!(
            "operation-service-command-test-{}-{}",
            std::process::id(),
            unique_suffix()
        )),
    }
}

fn test_launch_base_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "operation-service-workspace-launch-{}",
        unique_suffix()
    ))
}

fn test_snapshot(lease_id: &str) -> LayerStackSnapshotRef {
    LayerStackSnapshotRef {
        lease_id: LeaseId(lease_id.to_owned()),
        manifest_version: 1,
        root_hash: "root".to_owned(),
        layer_paths: vec![PathBuf::from("/lower/one")],
    }
}

fn unique_suffix() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
