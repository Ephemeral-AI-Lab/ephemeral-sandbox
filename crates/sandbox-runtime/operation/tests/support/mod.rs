#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sandbox_runtime::command::test_support::command_service_with_launch_driver_and_async_trace_sink;
use sandbox_runtime::command::{
    CommandCompletionPromise, CommandCompletionWaitOutcome, CommandLaunchDriver,
    CommandOperationService, CommandServiceError,
};
use sandbox_runtime::workspace_session::WorkspaceSessionService;
use sandbox_runtime::AsyncTraceSink;
use sandbox_runtime_command::process::{
    CommandProcess, CommandProcessExit, CommandProcessSpawn, CommandProcessSpec,
};
use sandbox_runtime_workspace::{
    CaptureChangesRequest, CapturedWorkspaceChanges, CreateWorkspaceRequest,
    DestroyWorkspaceRequest, DestroyWorkspaceResult, LayerStackSnapshotRef, LeaseId,
    ReadonlySnapshotHandle, RemountWorkspaceRequest, RemountWorkspaceResult, WorkspaceEntry,
    WorkspaceError, WorkspaceHandle, WorkspaceProfile, WorkspaceRuntimeHooks,
    WorkspaceRuntimeService, WorkspaceSessionId,
};

pub(crate) struct TestServices {
    pub(crate) workspace: Arc<WorkspaceSessionService>,
    pub(crate) command: Arc<CommandOperationService>,
}

#[derive(Default)]
pub(crate) struct FakeWorkspaceService {
    create_results: Mutex<VecDeque<Result<WorkspaceHandle, WorkspaceError>>>,
    capture_results: Mutex<VecDeque<Result<CapturedWorkspaceChanges, WorkspaceError>>>,
    remount_results: Mutex<VecDeque<Result<RemountWorkspaceResult, WorkspaceError>>>,
    destroy_results: Mutex<VecDeque<Result<DestroyWorkspaceResult, WorkspaceError>>>,
    create_requests: Mutex<Vec<CreateWorkspaceRequest>>,
    capture_calls: Mutex<Vec<WorkspaceSessionId>>,
    remount_calls: Mutex<Vec<WorkspaceSessionId>>,
    destroy_calls: Mutex<Vec<WorkspaceSessionId>>,
}

#[derive(Debug, Default)]
pub(crate) struct FakeLaunchDriver {
    outcomes: Mutex<VecDeque<ScriptedCommandYield>>,
    spawn_errors: Mutex<VecDeque<CommandServiceError>>,
    spawn_observations: Mutex<Vec<SpawnObservation>>,
}

#[derive(Debug, Clone)]
pub(crate) enum ScriptedCommandYield {
    Completed(CommandProcessExit),
    Running(String),
}

#[derive(Debug, Clone)]
pub(crate) struct SpawnObservation {
    pub(crate) spec_id: String,
    pub(crate) spec_command: String,
    pub(crate) spec_cwd: Option<PathBuf>,
    pub(crate) spec_timeout_seconds: Option<f64>,
    pub(crate) workspace_entry: WorkspaceEntry,
    pub(crate) transcript_path: PathBuf,
}

impl FakeLaunchDriver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push_outcome(&self, outcome: ScriptedCommandYield) {
        self.outcomes
            .lock()
            .expect("test operation succeeds")
            .push_back(outcome);
    }

    pub(crate) fn push_spawn_error(&self, error: CommandServiceError) {
        self.spawn_errors
            .lock()
            .expect("test operation succeeds")
            .push_back(error);
    }

    pub(crate) fn spawn_observations(&self) -> Vec<SpawnObservation> {
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
        config: &sandbox_runtime_command::CommandConfig,
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
                    command_session_id: sandbox_runtime::command::CommandSessionId(spec.id.clone()),
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
                transcript_path: parts.transcript_path.clone(),
            });
        Ok(CommandProcess::inactive_with_transcript_for_test(
            spec,
            parts.transcript_path,
        ))
    }

    fn start_completion_watcher(
        &self,
        _completion: CommandCompletionPromise,
        _process: Arc<CommandProcess>,
    ) {
    }

    fn wait_for_command_yield(
        &self,
        process: &CommandProcess,
        completion: &CommandCompletionPromise,
        _yield_time_ms: u64,
        _start_offset: u64,
    ) -> CommandCompletionWaitOutcome {
        let outcome = self
            .outcomes
            .lock()
            .expect("test operation succeeds")
            .pop_front()
            .unwrap_or_else(|| ScriptedCommandYield::Running(String::new()));
        match &outcome {
            ScriptedCommandYield::Running(output) => write_transcript_output(process, output),
            ScriptedCommandYield::Completed(exit) => {
                write_transcript_output(process, &exit.stdout);
                completion.resolve(exit.clone());
            }
        }
        match outcome {
            ScriptedCommandYield::Running(_) => CommandCompletionWaitOutcome::Running,
            ScriptedCommandYield::Completed(_) => CommandCompletionWaitOutcome::Completed,
        }
    }
}

impl FakeWorkspaceService {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push_create_result(&self, result: Result<WorkspaceHandle, WorkspaceError>) {
        self.create_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    pub(crate) fn push_capture_result(
        &self,
        result: Result<CapturedWorkspaceChanges, WorkspaceError>,
    ) {
        self.capture_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    pub(crate) fn push_remount_result(
        &self,
        result: Result<RemountWorkspaceResult, WorkspaceError>,
    ) {
        self.remount_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    pub(crate) fn push_destroy_result(
        &self,
        result: Result<DestroyWorkspaceResult, WorkspaceError>,
    ) {
        self.destroy_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    pub(crate) fn create_requests(&self) -> Vec<CreateWorkspaceRequest> {
        self.create_requests
            .lock()
            .expect("test operation succeeds")
            .clone()
    }

    pub(crate) fn destroy_calls(&self) -> Vec<WorkspaceSessionId> {
        self.destroy_calls
            .lock()
            .expect("test operation succeeds")
            .clone()
    }

    pub(crate) fn capture_calls(&self) -> Vec<WorkspaceSessionId> {
        self.capture_calls
            .lock()
            .expect("test operation succeeds")
            .clone()
    }

    pub(crate) fn remount_calls(&self) -> Vec<WorkspaceSessionId> {
        self.remount_calls
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
        handle: &WorkspaceHandle,
        _request: CaptureChangesRequest,
    ) -> Result<CapturedWorkspaceChanges, WorkspaceError> {
        self.capture_calls
            .lock()
            .expect("test operation succeeds")
            .push(handle.id.clone());
        self.capture_results
            .lock()
            .expect("test operation succeeds")
            .pop_front()
            .unwrap_or_else(|| {
                Err(WorkspaceError::Capture {
                    message: "capture result not configured".to_owned(),
                })
            })
    }

    fn remount_workspace(
        &self,
        handle: &WorkspaceHandle,
        _request: RemountWorkspaceRequest,
    ) -> Result<RemountWorkspaceResult, WorkspaceError> {
        self.remount_calls
            .lock()
            .expect("test operation succeeds")
            .push(handle.id.clone());
        self.remount_results
            .lock()
            .expect("test operation succeeds")
            .pop_front()
            .unwrap_or_else(|| {
                Err(WorkspaceError::Setup {
                    step: "remount result not configured".to_owned(),
                })
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

    fn latest_snapshot(&self) -> Result<ReadonlySnapshotHandle, WorkspaceError> {
        Err(WorkspaceError::SnapshotAcquire {
            source: "latest snapshot not configured".to_owned(),
        })
    }
}

pub(crate) fn fake_workspace_runtime(
    fake: Arc<FakeWorkspaceService>,
) -> Arc<WorkspaceRuntimeService> {
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
            latest_snapshot: Box::new(move || fake.latest_snapshot()),
        },
    ))
}

pub(crate) fn build_services(fake: Arc<FakeWorkspaceService>) -> TestServices {
    build_services_with_launch_driver(fake, Arc::new(FakeLaunchDriver::new()))
}

pub(crate) fn build_services_with_launch_driver(
    fake: Arc<FakeWorkspaceService>,
    launch_driver: Arc<dyn CommandLaunchDriver>,
) -> TestServices {
    build_services_with_launch_driver_and_async_trace_sink(fake, launch_driver, None)
}

pub(crate) fn build_services_with_launch_driver_and_async_trace_sink(
    fake: Arc<FakeWorkspaceService>,
    launch_driver: Arc<dyn CommandLaunchDriver>,
    async_trace_sink: Option<AsyncTraceSink>,
) -> TestServices {
    let workspace = Arc::new(WorkspaceSessionService::new(fake_workspace_runtime(fake)));
    let command = Arc::new(command_service_with_launch_driver_and_async_trace_sink(
        Arc::clone(&workspace),
        test_command_config(),
        launch_driver,
        async_trace_sink,
    ));
    TestServices { workspace, command }
}

pub(crate) fn create_request() -> CreateWorkspaceRequest {
    CreateWorkspaceRequest {
        profile: WorkspaceProfile::HostCompatible,
    }
}

pub(crate) fn workspace_handle(
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
    )
}

pub(crate) fn workspace_handle_without_launch(
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

pub(crate) fn workspace_handle_unavailable_launch(
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
    )
}

pub(crate) fn destroy_result(handle: &WorkspaceHandle) -> DestroyWorkspaceResult {
    DestroyWorkspaceResult {
        workspace_session_id: handle.id.clone(),
        evicted_upperdir_bytes: 4096,
        lifetime_s: 12.5,
        lease_released: Some(true),
        lease_release_error: None,
        active_leases_after: 3,
    }
}

pub(crate) fn success_exit(stdout: &str) -> CommandProcessExit {
    CommandProcessExit {
        status: "ok".to_owned(),
        exit_code: 0,
        signal: None,
        stdout: stdout.to_owned(),
        elapsed_s: 0.1,
        kill: None,
    }
}

fn write_transcript_output(process: &CommandProcess, output: &str) {
    if output.is_empty() {
        return;
    }
    let Some(path) = process.transcript_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write as _;
        let _ = file.write_all(output.as_bytes());
    }
}

fn test_command_config() -> sandbox_runtime_command::CommandConfig {
    sandbox_runtime_command::CommandConfig {
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
        manifest: test_manifest(),
        layer_paths: vec![PathBuf::from("/lower/one")],
    }
}

pub(crate) fn test_manifest() -> sandbox_runtime_layerstack::Manifest {
    sandbox_runtime_layerstack::Manifest::new(
        1,
        vec![sandbox_runtime_layerstack::LayerRef {
            layer_id: "L000001-test".to_owned(),
            path: "layers/L000001-test".to_owned(),
        }],
        sandbox_runtime_layerstack::MANIFEST_SCHEMA_VERSION,
    )
    .expect("test manifest is valid")
}

fn unique_suffix() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
