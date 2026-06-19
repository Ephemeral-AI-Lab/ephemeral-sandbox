use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use command::process::{
    CommandProcess, CommandProcessExit, CommandProcessSpawn, CommandProcessSpec,
};
use command::yield_wait_loop::WaitOutcome;
use operation_service::command::{
    CommandLaunchDriver, CommandOperationService, CommandServiceError,
};
use operation_service::workspace_manager::WorkspaceManagerService;
use operation_service::workspace_remount::{WorkspaceRemountOptions, WorkspaceRemountService};
use operation_service::OperationServices;
use workspace::{
    BaseRevision, CallerId, CaptureChangesRequest, CapturedWorkspaceChanges,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LatestSnapshotRequest,
    LayerStackSnapshotRef, LeaseId, NetworkMode, ReadonlySnapshotHandle, RemountWorkspaceRequest,
    RemountWorkspaceResult, WorkspaceError, WorkspaceHandle, WorkspaceLaunchContext,
    WorkspaceLaunchNamespaceFds, WorkspaceService,
};

pub struct TestServices {
    pub workspace: Arc<WorkspaceManagerService>,
    pub command: Arc<CommandOperationService>,
    pub services: OperationServices,
}

#[derive(Default)]
pub struct FakeWorkspaceService {
    create_results: Mutex<VecDeque<Result<WorkspaceHandle, WorkspaceError>>>,
    destroy_results: Mutex<VecDeque<Result<DestroyWorkspaceResult, WorkspaceError>>>,
    create_requests: Mutex<Vec<CreateWorkspaceRequest>>,
    destroy_calls: Mutex<Vec<WorkspaceId>>,
}

use workspace::WorkspaceId;

#[derive(Debug, Default)]
pub struct FakeLaunchDriver {
    outcomes: Mutex<VecDeque<WaitOutcome<CommandProcessExit>>>,
    spawn_errors: Mutex<VecDeque<CommandServiceError>>,
    spawn_observations: Mutex<Vec<SpawnObservation>>,
}

#[derive(Debug, Clone)]
pub struct SpawnObservation {
    pub spec_id: String,
    pub spec_caller_id: String,
    pub run_request: serde_json::Value,
    pub request_path: PathBuf,
    pub output_path: PathBuf,
    pub final_path: PathBuf,
    pub transcript_path: PathBuf,
    pub transcript_timestamp_timezone: String,
    pub output_drain_grace_ms: u64,
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
        parts: CommandProcessSpawn<'_>,
    ) -> Result<CommandProcess, CommandServiceError> {
        if let Some(error) = self
            .spawn_errors
            .lock()
            .expect("test operation succeeds")
            .pop_front()
        {
            return Err(error);
        }
        self.spawn_observations
            .lock()
            .expect("test operation succeeds")
            .push(SpawnObservation {
                spec_id: spec.id.clone(),
                spec_caller_id: spec.caller_id.clone(),
                run_request: parts.run_request.clone(),
                request_path: parts.request_path.clone(),
                output_path: parts.output_path.clone(),
                final_path: parts.final_path.clone(),
                transcript_path: parts.transcript_path.clone(),
                transcript_timestamp_timezone: parts.transcript_timestamp_timezone.to_owned(),
                output_drain_grace_ms: parts.output_drain_grace_ms,
            });
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

    pub fn destroy_calls(&self) -> Vec<WorkspaceId> {
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

pub fn build_services(fake: Arc<FakeWorkspaceService>) -> TestServices {
    build_services_with_launch_driver(fake, Arc::new(FakeLaunchDriver::new()))
}

pub fn build_services_with_launch_driver(
    fake: Arc<FakeWorkspaceService>,
    launch_driver: Arc<dyn CommandLaunchDriver>,
) -> TestServices {
    let workspace = Arc::new(WorkspaceManagerService::new(fake));
    let command = Arc::new(CommandOperationService::with_launch_driver_for_test(
        Arc::clone(&workspace),
        test_command_config(),
        launch_driver,
    ));
    let remount = Arc::new(WorkspaceRemountService::new(
        Arc::clone(&workspace),
        Arc::clone(&command),
        WorkspaceRemountOptions::default(),
    ));
    let services = OperationServices::new(Arc::clone(&workspace), Arc::clone(&command), remount);

    TestServices {
        workspace,
        command,
        services,
    }
}

pub fn create_request(caller_id: &str, workspace_root: PathBuf) -> CreateWorkspaceRequest {
    CreateWorkspaceRequest {
        caller_id: CallerId(caller_id.to_owned()),
        workspace_root,
        layer_stack_root: PathBuf::from("/layers"),
        network: NetworkMode::Host,
    }
}

pub fn assert_private_create_request(
    request: &CreateWorkspaceRequest,
    caller_id: &str,
    workspace_root: &PathBuf,
    network: NetworkMode,
) {
    assert_eq!(request.caller_id, CallerId(caller_id.to_owned()));
    assert_eq!(&request.workspace_root, workspace_root);
    assert_eq!(&request.layer_stack_root, workspace_root);
    assert_eq!(request.network, network);
}

pub fn workspace_handle(
    workspace_id: &str,
    caller_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    network: NetworkMode,
) -> WorkspaceHandle {
    workspace_handle_with_launch(
        workspace_id,
        caller_id,
        lease_id,
        workspace_root,
        network,
        Some(test_launch_context()),
    )
}

pub fn workspace_handle_with_launch(
    workspace_id: &str,
    caller_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    network: NetworkMode,
    launch: Option<WorkspaceLaunchContext>,
) -> WorkspaceHandle {
    let snapshot = LayerStackSnapshotRef {
        lease_id: LeaseId(lease_id.to_owned()),
        manifest_version: 1,
        root_hash: "root".to_owned(),
        layer_paths: vec![PathBuf::from("/lower/one")],
    };
    WorkspaceHandle {
        id: WorkspaceId(workspace_id.to_owned()),
        owner: CallerId(caller_id.to_owned()),
        workspace_root,
        network,
        base_revision: BaseRevision {
            version: 1,
            root_hash: "root".to_owned(),
            layer_count: 1,
        },
        snapshot,
        launch,
    }
}

pub fn destroy_result(handle: &WorkspaceHandle) -> DestroyWorkspaceResult {
    DestroyWorkspaceResult {
        workspace_id: handle.id.clone(),
        owner: handle.owner.clone(),
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
        ..command::CommandConfig::default()
    }
}

fn test_launch_context() -> WorkspaceLaunchContext {
    let root = std::env::temp_dir().join(format!(
        "operation-service-workspace-launch-{}",
        unique_suffix()
    ));
    WorkspaceLaunchContext {
        upperdir: root.join("upper"),
        workdir: root.join("work"),
        namespace_fds: Some(WorkspaceLaunchNamespaceFds {
            user: Some(10),
            mnt: Some(11),
            pid: Some(12),
            net: None,
        }),
        cgroup_path: None,
    }
}

fn unique_suffix() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
