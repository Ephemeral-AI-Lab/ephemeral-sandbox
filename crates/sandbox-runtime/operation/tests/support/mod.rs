#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

mod fake_launcher;
pub use fake_launcher::{FakeLauncher, FakeRunnerScript};
use sandbox_observability::{Observer, SpanRegistry};
use sandbox_runtime_namespace_execution::{NamespaceExecutionEngine, NamespaceExecutionError};
use sandbox_runtime_namespace_process::runner::protocol::{NamespaceRunnerRequest, RunResult};

use sandbox_runtime::command::{CommandOperationService, CommandServiceError};
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::workspace_session::WorkspaceSessionService;
use sandbox_runtime_workspace::{
    CaptureChangesRequest, CapturedWorkspaceChanges, CreateWorkspaceRequest,
    DestroyWorkspaceRequest, DestroyWorkspaceResult, LayerStackSnapshotRef, LeaseId,
    NetworkProfile, ReadonlySnapshotHandle, WorkspaceError, WorkspaceHandle, WorkspaceRuntimeHooks,
    WorkspaceRuntimeService, WorkspaceSessionId,
};

const MAX_ACTIVE_COMMANDS: usize = 256;
const SETUP_TIMEOUT_S: f64 = 30.0;

pub(crate) struct TestServices {
    pub(crate) workspace: Arc<WorkspaceSessionService>,
    pub(crate) command: Arc<CommandOperationService>,
}

#[derive(Default)]
pub(crate) struct FakeWorkspaceService {
    create_results: Mutex<VecDeque<Result<WorkspaceHandle, WorkspaceError>>>,
    capture_results: Mutex<VecDeque<Result<CapturedWorkspaceChanges, WorkspaceError>>>,
    destroy_results: Mutex<VecDeque<Result<DestroyWorkspaceResult, WorkspaceError>>>,
    create_requests: Mutex<Vec<CreateWorkspaceRequest>>,
    capture_calls: Mutex<Vec<WorkspaceSessionId>>,
    destroy_calls: Mutex<Vec<WorkspaceSessionId>>,
}

/// A scripted command outcome (kept for the suites that script per yield). It is
/// translated to a `FakeRunnerScript` applied by the engine's fake launcher.
#[derive(Debug, Clone)]
pub(crate) enum ScriptedCommandYield {
    Completed(ScriptedCommandExit),
    Running(String),
}

#[derive(Debug, Clone)]
pub(crate) struct ScriptedCommandExit {
    pub(crate) status: String,
    pub(crate) exit_code: i64,
    pub(crate) stdout: String,
}

/// A scripting façade over the engine `FakeLauncher`: `push_outcome` enqueues a
/// runner behavior, and the recorded requests/transcript paths back the suites'
/// spawn assertions.
#[derive(Default)]
pub(crate) struct FakeLaunchDriver {
    launcher: FakeLauncher,
}

impl FakeLaunchDriver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push_outcome(&self, outcome: ScriptedCommandYield) {
        self.launcher.push_script(script_from_yield(outcome));
    }

    pub(crate) fn push_spawn_error(&self, _error: CommandServiceError) {
        self.launcher.push_script(FakeRunnerScript::spawn_error(
            NamespaceExecutionError::Spawn("scripted spawn failure".to_owned()),
        ));
    }

    pub(crate) fn recorded_requests(&self) -> Vec<NamespaceRunnerRequest> {
        self.launcher.recorded_requests()
    }

    pub(crate) fn recorded_request_ids(&self) -> Vec<String> {
        self.launcher.recorded_request_ids()
    }

    pub(crate) fn recorded_transcript_paths(&self) -> Vec<Option<PathBuf>> {
        self.launcher.recorded_transcript_paths()
    }

    pub(crate) fn recorded_cgroup_procs_paths(&self) -> Vec<Option<PathBuf>> {
        self.launcher.recorded_cgroup_procs_paths()
    }

    pub(crate) fn launcher(&self) -> FakeLauncher {
        self.launcher.clone()
    }
}

fn script_from_yield(outcome: ScriptedCommandYield) -> FakeRunnerScript {
    match outcome {
        ScriptedCommandYield::Running(output) => FakeRunnerScript::running(output.into_bytes()),
        ScriptedCommandYield::Completed(exit) => FakeRunnerScript::completes_with_output(
            exit.stdout.clone().into_bytes(),
            run_result_from_exit(&exit),
        ),
    }
}

fn run_result_from_exit(exit: &ScriptedCommandExit) -> RunResult {
    RunResult {
        exit_code: i32::try_from(exit.exit_code).unwrap_or(1),
        payload: serde_json::json!({ "status": exit.status }),
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
    launch_driver: Arc<FakeLaunchDriver>,
) -> TestServices {
    build_services_with_launch_driver_and_cgroup_root(fake, launch_driver, None)
}

pub(crate) fn build_services_with_launch_driver_and_cgroup_root(
    fake: Arc<FakeWorkspaceService>,
    launch_driver: Arc<FakeLaunchDriver>,
    cgroup_root: Option<PathBuf>,
) -> TestServices {
    let workspace = Arc::new(WorkspaceSessionService::with_cgroup_root(
        fake_workspace_runtime(fake),
        cgroup_root,
        Observer::disabled(),
    ));
    let command = Arc::new(build_command_service(
        &workspace,
        test_layerstack_service(),
        &launch_driver,
    ));
    TestServices { workspace, command }
}

pub(crate) fn build_services_with_launch_driver_and_layerstack(
    fake: Arc<FakeWorkspaceService>,
    launch_driver: Arc<FakeLaunchDriver>,
    layerstack: Arc<LayerStackService>,
) -> TestServices {
    let workspace = Arc::new(WorkspaceSessionService::new(
        fake_workspace_runtime(fake),
        Observer::disabled(),
    ));
    let command = Arc::new(build_command_service(
        &workspace,
        layerstack,
        &launch_driver,
    ));
    TestServices { workspace, command }
}

/// Build a command service over an engine wired to the driver's fake launcher.
/// The one `exec_spans` registry backs both the engine's terminal hook and the
/// service launch path, matching production wiring; the disabled observer makes
/// every span/event a no-op for suites that do not assert on observability.
pub(crate) fn build_command_service(
    workspace: &Arc<WorkspaceSessionService>,
    layerstack: Arc<LayerStackService>,
    launch_driver: &FakeLaunchDriver,
) -> CommandOperationService {
    let obs = Observer::disabled();
    let exec_spans = Arc::new(SpanRegistry::new(obs.clone()));
    let engine = Arc::new(NamespaceExecutionEngine::with_launcher(
        Box::new(launch_driver.launcher()),
        exec_spans.clone(),
        MAX_ACTIVE_COMMANDS,
        SETUP_TIMEOUT_S,
    ));
    CommandOperationService::with_engine(
        Arc::clone(workspace),
        layerstack,
        test_command_config(),
        engine,
        exec_spans,
        obs,
    )
}

/// Build a full service set over one shared, caller-supplied `Observer` (enabled
/// in trace tests so the emitted spans/events land in one log). The one
/// `exec_spans` registry backs both the engine and the launch path, and the
/// layerstack service shares the same observer, so a one-shot finalize publish
/// records under the same trace.
pub(crate) fn build_observed_services(
    fake: Arc<FakeWorkspaceService>,
    launch_driver: Arc<FakeLaunchDriver>,
    obs: Observer,
) -> TestServices {
    let workspace = Arc::new(WorkspaceSessionService::new(
        fake_workspace_runtime(fake),
        obs.clone(),
    ));
    let exec_spans = Arc::new(SpanRegistry::new(obs.clone()));
    let engine = Arc::new(NamespaceExecutionEngine::with_launcher(
        Box::new(launch_driver.launcher()),
        exec_spans.clone(),
        MAX_ACTIVE_COMMANDS,
        SETUP_TIMEOUT_S,
    ));
    let command = Arc::new(CommandOperationService::with_engine(
        Arc::clone(&workspace),
        observed_layerstack_service(obs.clone()),
        test_command_config(),
        engine,
        exec_spans,
        obs,
    ));
    TestServices { workspace, command }
}

pub(crate) fn create_request() -> CreateWorkspaceRequest {
    CreateWorkspaceRequest {
        network: NetworkProfile::Shared,
    }
}

pub(crate) fn workspace_handle(
    workspace_session_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    network: NetworkProfile,
) -> WorkspaceHandle {
    let base_dir = test_launch_base_dir();
    WorkspaceHandle::holder_backed_for_test(
        WorkspaceSessionId(workspace_session_id.to_owned()),
        workspace_root,
        network,
        test_snapshot(lease_id),
        base_dir.join("upper"),
        base_dir.join("work"),
    )
}

pub(crate) fn workspace_handle_without_launch(
    workspace_session_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    network: NetworkProfile,
) -> WorkspaceHandle {
    WorkspaceHandle::without_launch_for_test(
        WorkspaceSessionId(workspace_session_id.to_owned()),
        workspace_root,
        network,
        test_snapshot(lease_id),
    )
}

pub(crate) fn workspace_handle_unavailable_launch(
    workspace_session_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    network: NetworkProfile,
) -> WorkspaceHandle {
    let base_dir = test_launch_base_dir();
    WorkspaceHandle::unavailable_for_test(
        WorkspaceSessionId(workspace_session_id.to_owned()),
        workspace_root,
        network,
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

pub(crate) fn success_exit(stdout: &str) -> ScriptedCommandExit {
    ScriptedCommandExit {
        status: "ok".to_owned(),
        exit_code: 0,
        stdout: stdout.to_owned(),
    }
}

fn test_command_config() -> sandbox_runtime::command::CommandConfig {
    sandbox_runtime::command::CommandConfig {
        scratch_root: std::env::temp_dir().join(format!(
            "operation-service-command-test-{}-{}",
            std::process::id(),
            unique_suffix()
        )),
    }
}

fn test_layerstack_service() -> Arc<LayerStackService> {
    observed_layerstack_service(Observer::disabled())
}

pub(crate) fn observed_layerstack_service(obs: Observer) -> Arc<LayerStackService> {
    let base = std::env::temp_dir().join(format!(
        "operation-service-layerstack-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let root = base.join("layer-stack");
    let workspace = base.join("workspace");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&workspace).expect("create layerstack test workspace");
    sandbox_runtime_layerstack::build_workspace_base(&root, &workspace, false)
        .expect("build layerstack test base");
    Arc::new(LayerStackService::new(root, obs).expect("create layerstack test service"))
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
