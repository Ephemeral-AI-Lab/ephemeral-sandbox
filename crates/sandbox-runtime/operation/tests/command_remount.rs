use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use sandbox_runtime::command::test_support::command_service_with_launch_driver;
use sandbox_runtime::command::{
    CommandCompletionPromise, CommandCompletionWaitOutcome, CommandLaunchDriver,
    CommandOperationService, CommandServiceError, ExecCommandInput, ReadCommandLinesInput,
    WriteCommandStdinInput,
};
use sandbox_runtime::workspace_remount::{
    CommandRemountCoordinator, RemountWorkspaceSession, WorkspaceRemountService,
};
use sandbox_runtime::workspace_session::WorkspaceSessionService;
use sandbox_runtime_command::process::{CommandProcess, CommandProcessSpec};
use sandbox_runtime_workspace::{
    CaptureChangesRequest, CapturedWorkspaceChanges, CreateWorkspaceRequest,
    DestroyWorkspaceRequest, DestroyWorkspaceResult, LayerStackSnapshotRef, LeaseId,
    ReadonlySnapshotHandle, RemountWorkspaceRequest, RemountWorkspaceResult, WorkspaceError,
    WorkspaceHandle, WorkspaceProfile, WorkspaceRuntimeHooks, WorkspaceRuntimeService,
    WorkspaceSessionId,
};

struct TestServices {
    workspace: Arc<WorkspaceSessionService>,
    command: Arc<CommandOperationService>,
    workspace_remount: Arc<WorkspaceRemountService>,
}

#[derive(Default)]
struct PendingGuardWorkspaceService {
    create_results: Mutex<VecDeque<Result<WorkspaceHandle, WorkspaceError>>>,
    remount_calls: Mutex<Vec<WorkspaceSessionId>>,
    remount_notifier: Mutex<Option<mpsc::Sender<()>>>,
}

impl PendingGuardWorkspaceService {
    fn push_create_result(&self, result: Result<WorkspaceHandle, WorkspaceError>) {
        self.create_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    fn notify_on_remount(&self, notifier: mpsc::Sender<()>) {
        *self
            .remount_notifier
            .lock()
            .expect("test operation succeeds") = Some(notifier);
    }

    fn remount_calls(&self) -> Vec<WorkspaceSessionId> {
        self.remount_calls
            .lock()
            .expect("test operation succeeds")
            .clone()
    }
}

impl PendingGuardWorkspaceService {
    fn create_workspace(
        &self,
        _request: CreateWorkspaceRequest,
    ) -> Result<WorkspaceHandle, WorkspaceError> {
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
        handle: &WorkspaceHandle,
        _request: RemountWorkspaceRequest,
    ) -> Result<RemountWorkspaceResult, WorkspaceError> {
        self.remount_calls
            .lock()
            .expect("test operation succeeds")
            .push(handle.id.clone());
        if let Some(notifier) = self
            .remount_notifier
            .lock()
            .expect("test operation succeeds")
            .as_ref()
        {
            let _ = notifier.send(());
        }
        Err(WorkspaceError::Setup {
            step: "remount result not configured".to_owned(),
        })
    }

    fn destroy_workspace(
        &self,
        handle: WorkspaceHandle,
        _request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceError> {
        Ok(DestroyWorkspaceResult {
            workspace_session_id: handle.id,
            evicted_upperdir_bytes: 0,
            lifetime_s: 0.0,
            lease_released: Some(true),
            lease_release_error: None,
            active_leases_after: 0,
        })
    }

    fn latest_snapshot(&self) -> Result<ReadonlySnapshotHandle, WorkspaceError> {
        Err(WorkspaceError::SnapshotAcquire {
            source: "latest snapshot not configured".to_owned(),
        })
    }
}

fn fake_workspace_runtime(fake: Arc<PendingGuardWorkspaceService>) -> Arc<WorkspaceRuntimeService> {
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

struct PendingGuardLaunchDriver;

impl CommandLaunchDriver for PendingGuardLaunchDriver {
    fn spawn(
        &self,
        spec: CommandProcessSpec,
        _workspace_entry: sandbox_runtime_workspace::WorkspaceEntry,
        _config: &sandbox_runtime_command::CommandConfig,
    ) -> Result<CommandProcess, CommandServiceError> {
        Ok(CommandProcess::inactive_for_test(spec))
    }

    fn start_completion_watcher(
        &self,
        _completion: CommandCompletionPromise,
        _process: Arc<CommandProcess>,
    ) {
    }

    fn wait_for_command_yield(
        &self,
        _process: &CommandProcess,
        _completion: &CommandCompletionPromise,
        _yield_time_ms: u64,
        _start_offset: u64,
    ) -> CommandCompletionWaitOutcome {
        CommandCompletionWaitOutcome::Running
    }
}

struct BlockingLaunchDriver {
    spawn_started: Mutex<Option<mpsc::Sender<()>>>,
    release_spawn: Mutex<mpsc::Receiver<()>>,
}

impl BlockingLaunchDriver {
    fn new(spawn_started: mpsc::Sender<()>, release_spawn: mpsc::Receiver<()>) -> Self {
        Self {
            spawn_started: Mutex::new(Some(spawn_started)),
            release_spawn: Mutex::new(release_spawn),
        }
    }
}

impl CommandLaunchDriver for BlockingLaunchDriver {
    fn spawn(
        &self,
        spec: CommandProcessSpec,
        _workspace_entry: sandbox_runtime_workspace::WorkspaceEntry,
        _config: &sandbox_runtime_command::CommandConfig,
    ) -> Result<CommandProcess, CommandServiceError> {
        if let Some(sender) = self
            .spawn_started
            .lock()
            .expect("test operation succeeds")
            .take()
        {
            sender.send(()).expect("test receiver is alive");
        }
        self.release_spawn
            .lock()
            .expect("test operation succeeds")
            .recv()
            .expect("test releases spawn");
        Ok(CommandProcess::inactive_for_test(spec))
    }

    fn start_completion_watcher(
        &self,
        _completion: CommandCompletionPromise,
        _process: Arc<CommandProcess>,
    ) {
    }

    fn wait_for_command_yield(
        &self,
        _process: &CommandProcess,
        _completion: &CommandCompletionPromise,
        _yield_time_ms: u64,
        _start_offset: u64,
    ) -> CommandCompletionWaitOutcome {
        CommandCompletionWaitOutcome::Running
    }
}

fn build_services(fake: Arc<PendingGuardWorkspaceService>) -> TestServices {
    let workspace = Arc::new(WorkspaceSessionService::new(fake_workspace_runtime(fake)));
    let command = Arc::new(command_service_with_launch_driver(
        Arc::clone(&workspace),
        command_config(),
        Arc::new(PendingGuardLaunchDriver),
    ));
    let remount_workspace: Arc<dyn RemountWorkspaceSession> = workspace.clone();
    let remount_command: Arc<dyn CommandRemountCoordinator> = command.clone();
    let remount = Arc::new(WorkspaceRemountService::new(
        remount_workspace,
        remount_command,
    ));
    TestServices {
        workspace,
        command,
        workspace_remount: remount,
    }
}

fn build_services_with_launch_driver(
    fake: Arc<PendingGuardWorkspaceService>,
    launch_driver: Arc<dyn CommandLaunchDriver>,
) -> TestServices {
    let workspace = Arc::new(WorkspaceSessionService::new(fake_workspace_runtime(fake)));
    let command = Arc::new(command_service_with_launch_driver(
        Arc::clone(&workspace),
        command_config(),
        launch_driver,
    ));
    let remount_workspace: Arc<dyn RemountWorkspaceSession> = workspace.clone();
    let remount_command: Arc<dyn CommandRemountCoordinator> = command.clone();
    let remount = Arc::new(WorkspaceRemountService::new(
        remount_workspace,
        remount_command,
    ));
    TestServices {
        workspace,
        command,
        workspace_remount: remount,
    }
}

fn create_request() -> CreateWorkspaceRequest {
    create_request_with_profile(WorkspaceProfile::HostCompatible)
}

fn create_request_with_profile(profile: WorkspaceProfile) -> CreateWorkspaceRequest {
    CreateWorkspaceRequest { profile }
}

fn exec_input(workspace_session_id: WorkspaceSessionId) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: Some(workspace_session_id),
        cmd: "echo ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(0),
    }
}

fn workspace_handle(
    workspace_session_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
) -> WorkspaceHandle {
    workspace_handle_with_profile(
        workspace_session_id,
        lease_id,
        workspace_root,
        WorkspaceProfile::HostCompatible,
    )
}

fn workspace_handle_with_profile(
    workspace_session_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    profile: WorkspaceProfile,
) -> WorkspaceHandle {
    let snapshot = LayerStackSnapshotRef {
        lease_id: LeaseId(lease_id.to_owned()),
        manifest_version: 1,
        root_hash: "root".to_owned(),
        manifest: test_manifest(),
        layer_paths: vec![PathBuf::from("/lower/one")],
    };
    WorkspaceHandle::holder_backed_for_test(
        WorkspaceSessionId(workspace_session_id.to_owned()),
        workspace_root,
        profile,
        snapshot,
        PathBuf::from("/tmp/command-remount-upper"),
        PathBuf::from("/tmp/command-remount-work"),
    )
}

fn test_manifest() -> sandbox_runtime_layerstack::Manifest {
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

fn create_session_and_command() -> (
    TestServices,
    WorkspaceSessionId,
    sandbox_runtime::command::CommandSessionId,
) {
    let fake = Arc::new(PendingGuardWorkspaceService::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
    )));
    let handler = services
        .workspace
        .create_workspace_session(create_request())
        .expect("create workspace session succeeds");
    let output = services
        .command
        .exec_command(exec_input(handler.workspace_session_id.clone()), None)
        .expect("exec command succeeds");
    (
        services,
        handler.workspace_session_id,
        output.command_session_id.expect("running command has id"),
    )
}

fn command_config() -> sandbox_runtime_command::CommandConfig {
    sandbox_runtime_command::CommandConfig {
        scratch_root: std::env::temp_dir().join(format!(
            "operation-service-command-remount-test-{}-{}",
            std::process::id(),
            unique_suffix()
        )),
    }
}

fn unique_suffix() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[test]
fn command_remount_start_rejects_for_pending_isolated_workspace() {
    let fake = Arc::new(PendingGuardWorkspaceService::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    fake.push_create_result(Ok(workspace_handle_with_profile(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::Isolated,
    )));
    let handler = services
        .workspace
        .create_workspace_session(create_request_with_profile(WorkspaceProfile::Isolated))
        .expect("create isolated workspace session succeeds");
    services
        .workspace
        .begin_remount(handler.workspace_session_id.clone())
        .expect("begin remount succeeds");

    let error = services
        .command
        .exec_command(exec_input(handler.workspace_session_id.clone()), None)
        .expect_err("exec rejects pending isolated remount");

    assert!(matches!(
        error,
        CommandServiceError::WorkspaceSessionRemountPending { workspace_session_id }
            if workspace_session_id == WorkspaceSessionId("workspace-1".to_owned())
    ));
}

#[test]
fn command_remount_start_rejects_for_pending_persistent_workspace() {
    let fake = Arc::new(PendingGuardWorkspaceService::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
    )));
    let handler = services
        .workspace
        .create_workspace_session(create_request())
        .expect("create workspace session succeeds");
    services
        .workspace
        .begin_remount(handler.workspace_session_id.clone())
        .expect("begin remount succeeds");

    let error = services
        .command
        .exec_command(exec_input(handler.workspace_session_id.clone()), None)
        .expect_err("exec rejects pending remount");

    assert!(matches!(
        error,
        CommandServiceError::WorkspaceSessionRemountPending { workspace_session_id }
            if workspace_session_id == WorkspaceSessionId("workspace-1".to_owned())
    ));
}

#[test]
fn command_remount_start_rejects_for_blocked_workspace() {
    let fake = Arc::new(PendingGuardWorkspaceService::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
    )));
    let handler = services
        .workspace
        .create_workspace_session(create_request())
        .expect("create workspace session succeeds");
    services
        .workspace
        .begin_remount(handler.workspace_session_id.clone())
        .expect("begin remount succeeds");
    services
        .workspace
        .block_remount(handler.workspace_session_id.clone())
        .expect("block remount succeeds");

    let error = services
        .command
        .exec_command(exec_input(handler.workspace_session_id.clone()), None)
        .expect_err("exec rejects blocked remount");

    assert!(matches!(
        error,
        CommandServiceError::WorkspaceSessionRemountBlocked { workspace_session_id }
            if workspace_session_id == WorkspaceSessionId("workspace-1".to_owned())
    ));
}

#[test]
fn command_remount_waits_for_in_flight_persistent_exec_admission() {
    let fake = Arc::new(PendingGuardWorkspaceService::default());
    let (spawn_started_tx, spawn_started_rx) = mpsc::channel();
    let (release_spawn_tx, release_spawn_rx) = mpsc::channel();
    let (remount_called_tx, remount_called_rx) = mpsc::channel();
    fake.notify_on_remount(remount_called_tx);
    let services = build_services_with_launch_driver(
        Arc::clone(&fake),
        Arc::new(BlockingLaunchDriver::new(
            spawn_started_tx,
            release_spawn_rx,
        )),
    );
    let workspace_root = PathBuf::from("/workspace");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
    )));
    let handler = services
        .workspace
        .create_workspace_session(create_request())
        .expect("create workspace session succeeds");

    let exec_command = Arc::clone(&services.command);
    let exec_workspace_session_id = handler.workspace_session_id.clone();
    let exec_thread = thread::spawn(move || {
        exec_command.exec_command(exec_input(exec_workspace_session_id), None)
    });
    spawn_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("exec reached blocked spawn");

    let remount = Arc::clone(&services.workspace_remount);
    let remount_workspace_session_id = handler.workspace_session_id.clone();
    let remount_thread =
        thread::spawn(move || remount.remount_workspace_session(remount_workspace_session_id));

    assert!(
        remount_called_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "remount must not scan past in-flight persistent exec admission"
    );
    release_spawn_tx.send(()).expect("exec thread is alive");
    let output = exec_thread
        .join()
        .expect("exec thread does not panic")
        .expect("exec succeeds");
    assert!(output.command_session_id.is_some());

    let outcome = remount_thread
        .join()
        .expect("remount thread does not panic")
        .expect("remount returns blocked outcome");
    assert_eq!(
        outcome.blocked_reason.as_deref(),
        Some("process_group_unavailable")
    );
    assert!(fake.remount_calls().is_empty());
}

#[test]
fn command_remount_stdin_rejects_for_active_command_when_workspace_becomes_pending() {
    let (services, workspace_session_id, command_session_id) = create_session_and_command();
    services
        .workspace
        .begin_remount(workspace_session_id.clone())
        .expect("begin remount succeeds");

    let error = services
        .command
        .write_command_stdin(WriteCommandStdinInput {
            command_session_id: command_session_id.clone(),
            stdin: "input".to_owned(),
            yield_time_ms: Some(0),
        })
        .expect_err("stdin rejects pending remount");

    assert!(matches!(
        error,
        CommandServiceError::WorkspaceSessionRemountPending { workspace_session_id: pending }
            if pending == workspace_session_id
    ));
}

#[test]
fn command_remount_read_lines_remains_allowed_while_pending() {
    let (services, workspace_session_id, command_session_id) = create_session_and_command();
    services
        .workspace
        .begin_remount(workspace_session_id)
        .expect("begin remount succeeds");

    let rows = services
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id: command_session_id.clone(),
            start_offset: Some(0),
            limit: Some(10),
        })
        .expect("read lines remains allowed");

    assert_eq!(
        rows.status,
        sandbox_runtime::command::CommandStatus::Running
    );
}
