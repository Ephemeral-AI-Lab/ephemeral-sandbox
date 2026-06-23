use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sandbox_runtime::command::test_support::{
    command_service_with_launch_driver, command_service_with_launch_driver_and_remount_controller,
};
use sandbox_runtime::command::{
    CommandCompletionPromise, CommandCompletionWaitOutcome, CommandLaunchDriver,
    CommandOperationService, CommandServiceError, ExecCommandInput, WriteCommandStdinInput,
};
use sandbox_runtime::workspace_remount::{
    CommandRemountCoordinator, ProcessGroupController, ProcessGroupInspection,
    RemountWorkspaceSession, WorkspaceRemountError, WorkspaceRemountService,
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
struct RemountWorkspaceServiceFake {
    create_results: Mutex<VecDeque<Result<WorkspaceHandle, WorkspaceError>>>,
    remount_results: Mutex<VecDeque<Result<RemountWorkspaceResult, WorkspaceError>>>,
    remount_calls: Mutex<Vec<WorkspaceSessionId>>,
    remount_callback: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl RemountWorkspaceServiceFake {
    fn push_create_result(&self, result: Result<WorkspaceHandle, WorkspaceError>) {
        self.create_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    fn push_remount_result(&self, result: Result<RemountWorkspaceResult, WorkspaceError>) {
        self.remount_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    fn remount_calls(&self) -> Vec<WorkspaceSessionId> {
        self.remount_calls
            .lock()
            .expect("test operation succeeds")
            .clone()
    }

    fn on_remount(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        *self
            .remount_callback
            .lock()
            .expect("test operation succeeds") = Some(callback);
    }
}

impl RemountWorkspaceServiceFake {
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
        let callback = self
            .remount_callback
            .lock()
            .expect("test operation succeeds")
            .clone();
        if let Some(callback) = callback {
            callback();
        }
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

fn fake_workspace_runtime(fake: Arc<RemountWorkspaceServiceFake>) -> Arc<WorkspaceRuntimeService> {
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

struct InactiveLaunchDriver {
    process_group_id: Option<i32>,
}

impl CommandLaunchDriver for InactiveLaunchDriver {
    fn spawn(
        &self,
        spec: CommandProcessSpec,
        _workspace_entry: sandbox_runtime_workspace::WorkspaceEntry,
        _config: &sandbox_runtime_command::CommandConfig,
    ) -> Result<CommandProcess, CommandServiceError> {
        Ok(match self.process_group_id {
            Some(pgid) => CommandProcess::inactive_with_process_group_for_test(spec, pgid),
            None => CommandProcess::inactive_for_test(spec),
        })
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

#[derive(Default)]
struct FakeProcessGroupController {
    resumed: Mutex<Vec<i32>>,
    resume_pending: Mutex<Vec<bool>>,
    state_probe: Mutex<Option<(Arc<WorkspaceSessionService>, WorkspaceSessionId)>>,
}

impl FakeProcessGroupController {
    fn observe_state_on_resume(
        &self,
        workspace: Arc<WorkspaceSessionService>,
        workspace_session_id: WorkspaceSessionId,
    ) {
        *self.state_probe.lock().expect("test operation succeeds") =
            Some((workspace, workspace_session_id));
    }

    fn resumed(&self) -> Vec<i32> {
        self.resumed
            .lock()
            .expect("test operation succeeds")
            .clone()
    }

    fn resume_pending(&self) -> Vec<bool> {
        self.resume_pending
            .lock()
            .expect("test operation succeeds")
            .clone()
    }
}

impl ProcessGroupController for FakeProcessGroupController {
    fn inspect_command_process_group(
        &self,
        _pgid: i32,
        _workspace_root: &Path,
    ) -> ProcessGroupInspection {
        ProcessGroupInspection {
            process_count: 1,
            quiesced_process_count: 1,
            inspected: true,
            quiesce_attempted: true,
            ..ProcessGroupInspection::default()
        }
    }

    fn resume_process_group_id(&self, pgid: i32) -> bool {
        self.resumed
            .lock()
            .expect("test operation succeeds")
            .push(pgid);
        if let Some((workspace, workspace_session_id)) = self
            .state_probe
            .lock()
            .expect("test operation succeeds")
            .as_ref()
        {
            self.resume_pending
                .lock()
                .expect("test operation succeeds")
                .push(workspace.is_remount_pending(workspace_session_id));
        }
        true
    }
}

fn build_services(fake: Arc<RemountWorkspaceServiceFake>) -> TestServices {
    let workspace = Arc::new(WorkspaceSessionService::new(fake_workspace_runtime(fake)));
    let command = Arc::new(command_service_with_launch_driver(
        Arc::clone(&workspace),
        command_config(),
        Arc::new(InactiveLaunchDriver {
            process_group_id: None,
        }),
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

fn build_services_with_process_group_controller(
    fake: Arc<RemountWorkspaceServiceFake>,
    controller: Arc<dyn ProcessGroupController>,
    process_group_id: i32,
) -> TestServices {
    let workspace = Arc::new(WorkspaceSessionService::new(fake_workspace_runtime(fake)));
    let command = Arc::new(command_service_with_launch_driver_and_remount_controller(
        Arc::clone(&workspace),
        command_config(),
        Arc::new(InactiveLaunchDriver {
            process_group_id: Some(process_group_id),
        }),
        controller,
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
        PathBuf::from("/tmp/workspace-remount-upper"),
        PathBuf::from("/tmp/workspace-remount-work"),
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

fn command_config() -> sandbox_runtime_command::CommandConfig {
    sandbox_runtime_command::CommandConfig {
        scratch_root: std::env::temp_dir().join(format!(
            "operation-service-workspace-remount-test-{}-{}",
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
fn workspace_remount_isolated_no_active_command_path_succeeds_and_clears_pending() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    let mut remounted = workspace_handle_with_profile(
        "workspace-1",
        "lease-2",
        workspace_root.clone(),
        WorkspaceProfile::Isolated,
    );
    remounted.snapshot.manifest_version = 2;
    remounted.snapshot.root_hash = "root-2".to_owned();
    remounted.snapshot.layer_paths = vec![PathBuf::from("/lower/two")];
    remounted.base_revision = remounted.snapshot.base_revision();
    fake.push_create_result(Ok(workspace_handle_with_profile(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::Isolated,
    )));
    fake.push_remount_result(Ok(RemountWorkspaceResult {
        handle: remounted.clone(),
    }));
    let handler = services
        .workspace
        .create_workspace_session(create_request_with_profile(WorkspaceProfile::Isolated))
        .expect("create isolated workspace session succeeds");

    let outcome = services
        .workspace_remount
        .remount_workspace_session(handler.workspace_session_id.clone())
        .expect("isolated remount succeeds");

    assert!(outcome.remounted);
    assert!(outcome.blocked_reason.is_none());
    assert_eq!(
        outcome
            .updated_handler
            .expect("updated handler is returned")
            .handle
            .profile,
        WorkspaceProfile::Isolated
    );
    assert!(!services
        .workspace
        .is_remount_pending(&WorkspaceSessionId("workspace-1".to_owned())));
}

#[test]
fn workspace_remount_no_active_command_path_succeeds_and_clears_pending() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    let mut remounted = workspace_handle("workspace-1", "lease-2", workspace_root.clone());
    remounted.snapshot.manifest_version = 2;
    remounted.snapshot.root_hash = "root-2".to_owned();
    remounted.snapshot.layer_paths = vec![PathBuf::from("/lower/two")];
    remounted.base_revision = remounted.snapshot.base_revision();
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
    )));
    fake.push_remount_result(Ok(RemountWorkspaceResult {
        handle: remounted.clone(),
    }));
    let handler = services
        .workspace
        .create_workspace_session(create_request())
        .expect("create workspace session succeeds");

    let outcome = services
        .workspace_remount
        .remount_workspace_session(handler.workspace_session_id.clone())
        .expect("remount succeeds");

    assert!(outcome.remounted);
    assert!(outcome.blocked_reason.is_none());
    assert_eq!(outcome.command_inspection.active_commands, 0);
    assert_eq!(
        outcome
            .updated_handler
            .expect("updated handler is returned")
            .handle
            .snapshot
            .manifest_version,
        2
    );
    assert_eq!(
        fake.remount_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
    assert!(!services
        .workspace
        .is_remount_pending(&WorkspaceSessionId("workspace-1".to_owned())));
}

#[test]
fn workspace_remount_live_command_success_finishes_before_resume() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let controller = Arc::new(FakeProcessGroupController::default());
    let services =
        build_services_with_process_group_controller(Arc::clone(&fake), controller.clone(), 101);
    let workspace_root = PathBuf::from("/workspace");
    let mut remounted = workspace_handle("workspace-1", "lease-2", workspace_root.clone());
    remounted.snapshot.manifest_version = 2;
    remounted.snapshot.root_hash = "root-2".to_owned();
    remounted.snapshot.layer_paths = vec![PathBuf::from("/lower/two")];
    remounted.base_revision = remounted.snapshot.base_revision();
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
    )));
    fake.push_remount_result(Ok(RemountWorkspaceResult { handle: remounted }));
    let handler = services
        .workspace
        .create_workspace_session(create_request())
        .expect("create workspace session succeeds");
    services
        .command
        .exec_command(exec_input(handler.workspace_session_id.clone()), None)
        .expect("exec command succeeds");
    controller.observe_state_on_resume(
        Arc::clone(&services.workspace),
        handler.workspace_session_id.clone(),
    );

    let outcome = services
        .workspace_remount
        .remount_workspace_session(handler.workspace_session_id.clone())
        .expect("live remount succeeds");

    assert!(outcome.remounted);
    assert_eq!(outcome.command_inspection.active_commands, 1);
    assert_eq!(controller.resumed(), vec![101]);
    assert_eq!(controller.resume_pending(), vec![false]);
    assert!(!services
        .workspace
        .is_remount_pending(&WorkspaceSessionId("workspace-1".to_owned())));
}

#[test]
fn workspace_remount_cancel_during_critical_switch_still_applies_and_resumes() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let controller = Arc::new(FakeProcessGroupController::default());
    let services =
        build_services_with_process_group_controller(Arc::clone(&fake), controller.clone(), 101);
    let workspace_root = PathBuf::from("/workspace");
    let mut remounted = workspace_handle("workspace-1", "lease-2", workspace_root.clone());
    remounted.snapshot.manifest_version = 2;
    remounted.snapshot.root_hash = "root-2".to_owned();
    remounted.snapshot.layer_paths = vec![PathBuf::from("/lower/two")];
    remounted.base_revision = remounted.snapshot.base_revision();
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
    )));
    fake.push_remount_result(Ok(RemountWorkspaceResult { handle: remounted }));
    let handler = services
        .workspace
        .create_workspace_session(create_request())
        .expect("create workspace session succeeds");
    let command_session_id = services
        .command
        .exec_command(exec_input(handler.workspace_session_id.clone()), None)
        .expect("exec command succeeds")
        .command_session_id
        .expect("running command has id");
    controller.observe_state_on_resume(
        Arc::clone(&services.workspace),
        handler.workspace_session_id.clone(),
    );
    let command = Arc::clone(&services.command);
    fake.on_remount(Arc::new(move || {
        command
            .write_command_stdin(WriteCommandStdinInput {
                command_session_id: command_session_id.clone(),
                stdin: "\u{3}".to_owned(),
                yield_time_ms: Some(0),
            })
            .expect("cancel during remount is accepted");
    }));

    let outcome = services
        .workspace_remount
        .remount_workspace_session(handler.workspace_session_id.clone())
        .expect("live remount succeeds despite cancellation");

    assert!(outcome.remounted);
    assert_eq!(fake.remount_calls(), vec![handler.workspace_session_id]);
    assert_eq!(controller.resumed(), vec![101]);
    assert_eq!(controller.resume_pending(), vec![false]);
}

#[test]
fn workspace_remount_blocked_inspection_marks_blocked_and_skips_resource_remount() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
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
        .command
        .exec_command(exec_input(handler.workspace_session_id.clone()), None)
        .expect("exec command succeeds");

    let outcome = services
        .workspace_remount
        .remount_workspace_session(handler.workspace_session_id.clone())
        .expect("blocked remount returns outcome");

    assert!(!outcome.remounted);
    assert_eq!(
        outcome.blocked_reason.as_deref(),
        Some("process_group_unavailable")
    );
    assert!(fake.remount_calls().is_empty());
    assert!(!services
        .workspace
        .is_remount_pending(&WorkspaceSessionId("workspace-1".to_owned())));
}

#[test]
fn workspace_remount_resource_failure_blocks_state_after_cleanup() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "lease-1",
        workspace_root.clone(),
    )));
    fake.push_remount_result(Err(WorkspaceError::Setup {
        step: "remount failed".to_owned(),
    }));
    let handler = services
        .workspace
        .create_workspace_session(create_request())
        .expect("create workspace session succeeds");

    let error = services
        .workspace_remount
        .remount_workspace_session(handler.workspace_session_id.clone())
        .expect_err("resource remount failure is returned");

    assert!(matches!(
        error,
        WorkspaceRemountError::WorkspaceSession(
            sandbox_runtime::workspace_session::WorkspaceSessionError::Workspace(
                WorkspaceError::Setup { .. }
            )
        )
    ));
    assert_eq!(
        fake.remount_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
    assert!(!services
        .workspace
        .is_remount_pending(&WorkspaceSessionId("workspace-1".to_owned())));
}
