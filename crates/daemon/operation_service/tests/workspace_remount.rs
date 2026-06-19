use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use command::process::{
    CommandProcess, CommandProcessExit, CommandProcessSpawn, CommandProcessSpec,
};
use command::yield_wait_loop::WaitOutcome;
use operation_service::command::{
    CancelCommandInput, CommandCallContext, CommandLaunchDriver, CommandOperationService,
    CommandRemountInspection, CommandServiceError, ExecCommandInput, OperationTraceContext,
    ProcessGroupController,
};
use operation_service::workspace_manager::{WorkspaceManagerService, WorkspaceRemountState};
use operation_service::workspace_remount::{
    WorkspaceRemountError, WorkspaceRemountOptions, WorkspaceRemountService,
};
use operation_service::OperationServices;
use workspace::{
    CallerId, CaptureChangesRequest, CapturedWorkspaceChanges, CreateWorkspaceRequest,
    DestroyWorkspaceRequest, DestroyWorkspaceResult, LatestSnapshotRequest, LayerStackSnapshotRef,
    LeaseId, ReadonlySnapshotHandle, RemountWorkspaceRequest, RemountWorkspaceResult,
    WorkspaceError, WorkspaceHandle, WorkspaceId, WorkspaceProfile, WorkspaceService,
};

struct TestServices {
    workspace: Arc<WorkspaceManagerService>,
    command: Arc<CommandOperationService>,
    services: OperationServices,
}

#[derive(Default)]
struct RemountWorkspaceServiceFake {
    create_results: Mutex<VecDeque<Result<WorkspaceHandle, WorkspaceError>>>,
    remount_results: Mutex<VecDeque<Result<RemountWorkspaceResult, WorkspaceError>>>,
    remount_calls: Mutex<Vec<WorkspaceId>>,
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

    fn remount_calls(&self) -> Vec<WorkspaceId> {
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

impl WorkspaceService for RemountWorkspaceServiceFake {
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

struct InactiveLaunchDriver {
    process_group_id: Option<i32>,
}

impl CommandLaunchDriver for InactiveLaunchDriver {
    fn spawn(
        &self,
        spec: CommandProcessSpec,
        _parts: CommandProcessSpawn<'_>,
    ) -> Result<CommandProcess, CommandServiceError> {
        Ok(match self.process_group_id {
            Some(pgid) => CommandProcess::inactive_with_process_group_for_test(spec, pgid),
            None => CommandProcess::inactive_for_test(spec),
        })
    }

    fn wait_for_initial_yield(
        &self,
        _process: &CommandProcess,
        _config: &command::CommandConfig,
        _yield_time_ms: u64,
        _start_offset: u64,
    ) -> WaitOutcome<CommandProcessExit> {
        WaitOutcome::Running(String::new())
    }
}

#[derive(Default)]
struct FakeProcessGroupController {
    resumed: Mutex<Vec<i32>>,
    resume_states: Mutex<Vec<WorkspaceRemountState>>,
    state_probe: Mutex<Option<(Arc<WorkspaceManagerService>, WorkspaceId)>>,
}

impl FakeProcessGroupController {
    fn observe_state_on_resume(
        &self,
        workspace: Arc<WorkspaceManagerService>,
        workspace_id: WorkspaceId,
    ) {
        *self.state_probe.lock().expect("test operation succeeds") =
            Some((workspace, workspace_id));
    }

    fn resumed(&self) -> Vec<i32> {
        self.resumed
            .lock()
            .expect("test operation succeeds")
            .clone()
    }

    fn resume_states(&self) -> Vec<WorkspaceRemountState> {
        self.resume_states
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
    ) -> CommandRemountInspection {
        CommandRemountInspection {
            active_commands: 1,
            process_count: 1,
            quiesced_process_count: 1,
            inspected: true,
            quiesce_attempted: true,
            ..CommandRemountInspection::default()
        }
    }

    fn resume_process_group_id(&self, pgid: i32) -> bool {
        self.resumed
            .lock()
            .expect("test operation succeeds")
            .push(pgid);
        if let Some((workspace, workspace_id)) = self
            .state_probe
            .lock()
            .expect("test operation succeeds")
            .as_ref()
        {
            self.resume_states
                .lock()
                .expect("test operation succeeds")
                .push(
                    workspace
                        .remount_state(workspace_id)
                        .expect("remount state is readable"),
                );
        }
        true
    }
}

fn build_services(fake: Arc<RemountWorkspaceServiceFake>) -> TestServices {
    let workspace = Arc::new(WorkspaceManagerService::new(fake));
    let command = Arc::new(CommandOperationService::with_launch_driver_for_test(
        Arc::clone(&workspace),
        command_config(),
        Arc::new(InactiveLaunchDriver {
            process_group_id: None,
        }),
    ));
    let remount = Arc::new(WorkspaceRemountService::new(
        Arc::clone(&workspace),
        Arc::clone(&command),
        WorkspaceRemountOptions::default(),
    ));
    let services = OperationServices::new(Arc::clone(&workspace), command, remount);
    TestServices {
        workspace,
        command: Arc::clone(&services.command),
        services,
    }
}

fn build_services_with_process_group_controller(
    fake: Arc<RemountWorkspaceServiceFake>,
    controller: Arc<dyn ProcessGroupController>,
    process_group_id: i32,
) -> TestServices {
    let workspace = Arc::new(WorkspaceManagerService::new(fake));
    let command = Arc::new(
        CommandOperationService::with_launch_driver_and_remount_controller_for_test(
            Arc::clone(&workspace),
            command_config(),
            Arc::new(InactiveLaunchDriver {
                process_group_id: Some(process_group_id),
            }),
            controller,
        ),
    );
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

fn create_request(caller_id: &str, workspace_root: PathBuf) -> CreateWorkspaceRequest {
    create_request_with_profile(caller_id, workspace_root, WorkspaceProfile::HostCompatible)
}

fn create_request_with_profile(
    caller_id: &str,
    workspace_root: PathBuf,
    profile: WorkspaceProfile,
) -> CreateWorkspaceRequest {
    CreateWorkspaceRequest {
        caller_id: CallerId(caller_id.to_owned()),
        workspace_root,
        layer_stack_root: PathBuf::from("/layers"),
        profile,
    }
}

fn exec_input(workspace_id: WorkspaceId, workspace_root: PathBuf) -> ExecCommandInput {
    ExecCommandInput {
        caller_id: CallerId("caller-1".to_owned()),
        workspace_root,
        workspace_id: Some(workspace_id),
        cmd: "echo ok".to_owned(),
        cwd: None,
        timeout_seconds: None,
        yield_time_ms: Some(0),
    }
}

fn command_context(caller_id: &str) -> CommandCallContext {
    CommandCallContext {
        caller_id: CallerId(caller_id.to_owned()),
        trace: OperationTraceContext,
    }
}

fn workspace_handle(
    workspace_id: &str,
    caller_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
) -> WorkspaceHandle {
    workspace_handle_with_profile(
        workspace_id,
        caller_id,
        lease_id,
        workspace_root,
        WorkspaceProfile::HostCompatible,
    )
}

fn workspace_handle_with_profile(
    workspace_id: &str,
    caller_id: &str,
    lease_id: &str,
    workspace_root: PathBuf,
    profile: WorkspaceProfile,
) -> WorkspaceHandle {
    let snapshot = LayerStackSnapshotRef {
        lease_id: LeaseId(lease_id.to_owned()),
        manifest_version: 1,
        root_hash: "root".to_owned(),
        layer_paths: vec![PathBuf::from("/lower/one")],
    };
    WorkspaceHandle::holder_backed_for_test(
        WorkspaceId(workspace_id.to_owned()),
        CallerId(caller_id.to_owned()),
        workspace_root,
        profile,
        snapshot,
        PathBuf::from("/tmp/workspace-remount-upper"),
        PathBuf::from("/tmp/workspace-remount-work"),
        None,
    )
}

fn command_config() -> command::CommandConfig {
    command::CommandConfig {
        scratch_root: std::env::temp_dir().join(format!(
            "operation-service-workspace-remount-test-{}-{}",
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

#[test]
fn workspace_remount_isolated_no_active_command_path_succeeds_and_clears_pending() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    let mut remounted = workspace_handle_with_profile(
        "workspace-1",
        "caller-1",
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
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::Isolated,
    )));
    fake.push_remount_result(Ok(RemountWorkspaceResult {
        handle: remounted.clone(),
    }));
    let handler = services
        .workspace
        .create(create_request_with_profile(
            "caller-1",
            workspace_root,
            WorkspaceProfile::Isolated,
        ))
        .expect("create isolated workspace session succeeds");

    let report = services
        .services
        .remount
        .compact_or_remount_session(handler.workspace_id.clone())
        .expect("isolated remount succeeds");

    assert!(report.remounted);
    assert!(report.blocked_reason.is_none());
    assert_eq!(
        report
            .updated_handler
            .expect("updated handler is returned")
            .handle
            .profile,
        WorkspaceProfile::Isolated
    );
    assert_eq!(
        services
            .workspace
            .remount_state(&WorkspaceId("workspace-1".to_owned()))
            .expect("remount state is readable"),
        WorkspaceRemountState::Active
    );
}

#[test]
fn workspace_remount_no_active_command_path_succeeds_and_clears_pending() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    let mut remounted =
        workspace_handle("workspace-1", "caller-1", "lease-2", workspace_root.clone());
    remounted.snapshot.manifest_version = 2;
    remounted.snapshot.root_hash = "root-2".to_owned();
    remounted.snapshot.layer_paths = vec![PathBuf::from("/lower/two")];
    remounted.base_revision = remounted.snapshot.base_revision();
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
    )));
    fake.push_remount_result(Ok(RemountWorkspaceResult {
        handle: remounted.clone(),
    }));
    let handler = services
        .workspace
        .create(create_request("caller-1", workspace_root))
        .expect("create workspace session succeeds");

    let report = services
        .services
        .remount
        .compact_or_remount_session(handler.workspace_id.clone())
        .expect("remount succeeds");

    assert!(report.remounted);
    assert!(report.blocked_reason.is_none());
    assert_eq!(report.command_inspection.active_commands, 0);
    assert_eq!(
        report
            .updated_handler
            .expect("updated handler is returned")
            .snapshot
            .manifest_version,
        2
    );
    assert_eq!(
        fake.remount_calls(),
        vec![WorkspaceId("workspace-1".to_owned())]
    );
    assert_eq!(
        services
            .workspace
            .remount_state(&WorkspaceId("workspace-1".to_owned()))
            .expect("remount state is readable"),
        WorkspaceRemountState::Active
    );
}

#[test]
fn workspace_remount_live_command_success_finishes_before_resume() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let controller = Arc::new(FakeProcessGroupController::default());
    let services =
        build_services_with_process_group_controller(Arc::clone(&fake), controller.clone(), 101);
    let workspace_root = PathBuf::from("/workspace");
    let mut remounted =
        workspace_handle("workspace-1", "caller-1", "lease-2", workspace_root.clone());
    remounted.snapshot.manifest_version = 2;
    remounted.snapshot.root_hash = "root-2".to_owned();
    remounted.snapshot.layer_paths = vec![PathBuf::from("/lower/two")];
    remounted.base_revision = remounted.snapshot.base_revision();
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
    )));
    fake.push_remount_result(Ok(RemountWorkspaceResult { handle: remounted }));
    let handler = services
        .workspace
        .create(create_request("caller-1", workspace_root.clone()))
        .expect("create workspace session succeeds");
    services
        .services
        .exec_command(
            exec_input(handler.workspace_id.clone(), workspace_root),
            OperationTraceContext,
        )
        .expect("exec command succeeds");
    controller.observe_state_on_resume(
        Arc::clone(&services.workspace),
        handler.workspace_id.clone(),
    );

    let report = services
        .services
        .remount
        .compact_or_remount_session(handler.workspace_id.clone())
        .expect("live remount succeeds");

    assert!(report.remounted);
    assert_eq!(report.command_inspection.active_commands, 1);
    assert_eq!(controller.resumed(), vec![101]);
    assert_eq!(
        controller.resume_states(),
        vec![WorkspaceRemountState::Active]
    );
    assert_eq!(
        services
            .workspace
            .remount_state(&WorkspaceId("workspace-1".to_owned()))
            .expect("remount state is readable"),
        WorkspaceRemountState::Active
    );
}

#[test]
fn workspace_remount_cancel_during_critical_switch_still_applies_and_resumes() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let controller = Arc::new(FakeProcessGroupController::default());
    let services =
        build_services_with_process_group_controller(Arc::clone(&fake), controller.clone(), 101);
    let workspace_root = PathBuf::from("/workspace");
    let mut remounted =
        workspace_handle("workspace-1", "caller-1", "lease-2", workspace_root.clone());
    remounted.snapshot.manifest_version = 2;
    remounted.snapshot.root_hash = "root-2".to_owned();
    remounted.snapshot.layer_paths = vec![PathBuf::from("/lower/two")];
    remounted.base_revision = remounted.snapshot.base_revision();
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
    )));
    fake.push_remount_result(Ok(RemountWorkspaceResult { handle: remounted }));
    let handler = services
        .workspace
        .create(create_request("caller-1", workspace_root.clone()))
        .expect("create workspace session succeeds");
    let command_id = services
        .services
        .exec_command(
            exec_input(handler.workspace_id.clone(), workspace_root),
            OperationTraceContext,
        )
        .expect("exec command succeeds")
        .command_id
        .expect("running command has id");
    controller.observe_state_on_resume(
        Arc::clone(&services.workspace),
        handler.workspace_id.clone(),
    );
    let command = Arc::clone(&services.command);
    fake.on_remount(Arc::new(move || {
        command
            .cancel(
                CancelCommandInput {
                    command_id: command_id.clone(),
                },
                command_context("caller-1"),
            )
            .expect("cancel during remount is accepted");
    }));

    let report = services
        .services
        .remount
        .compact_or_remount_session(handler.workspace_id.clone())
        .expect("live remount succeeds despite cancellation");

    assert!(report.remounted);
    assert_eq!(fake.remount_calls(), vec![handler.workspace_id]);
    assert_eq!(controller.resumed(), vec![101]);
    assert_eq!(
        controller.resume_states(),
        vec![WorkspaceRemountState::Active]
    );
}

#[test]
fn workspace_remount_blocked_inspection_marks_blocked_and_skips_resource_remount() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
    )));
    let handler = services
        .workspace
        .create(create_request("caller-1", workspace_root.clone()))
        .expect("create workspace session succeeds");
    services
        .services
        .exec_command(
            exec_input(handler.workspace_id.clone(), workspace_root),
            OperationTraceContext,
        )
        .expect("exec command succeeds");

    let report = services
        .services
        .remount
        .compact_or_remount_session(handler.workspace_id.clone())
        .expect("blocked remount returns report");

    assert!(!report.remounted);
    assert_eq!(
        report.blocked_reason.as_deref(),
        Some("process_group_unavailable")
    );
    assert!(fake.remount_calls().is_empty());
    assert_eq!(
        services
            .workspace
            .remount_state(&WorkspaceId("workspace-1".to_owned()))
            .expect("remount state is readable"),
        WorkspaceRemountState::RemountBlocked {
            reason: "process_group_unavailable".to_owned()
        }
    );
}

#[test]
fn workspace_remount_resource_failure_blocks_state_after_cleanup() {
    let fake = Arc::new(RemountWorkspaceServiceFake::default());
    let services = build_services(Arc::clone(&fake));
    let workspace_root = PathBuf::from("/workspace");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-1",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
    )));
    fake.push_remount_result(Err(WorkspaceError::Setup {
        step: "remount failed".to_owned(),
    }));
    let handler = services
        .workspace
        .create(create_request("caller-1", workspace_root))
        .expect("create workspace session succeeds");

    let error = services
        .services
        .remount
        .compact_or_remount_session(handler.workspace_id.clone())
        .expect_err("resource remount failure is returned");

    assert!(matches!(
        error,
        WorkspaceRemountError::WorkspaceManager(
            operation_service::workspace_manager::WorkspaceManagerError::Workspace(
                WorkspaceError::Setup { .. }
            )
        )
    ));
    assert_eq!(
        fake.remount_calls(),
        vec![WorkspaceId("workspace-1".to_owned())]
    );
    match services
        .workspace
        .remount_state(&WorkspaceId("workspace-1".to_owned()))
        .expect("remount state is readable")
    {
        WorkspaceRemountState::RemountBlocked { reason } => {
            assert!(reason.contains("remount failed"));
        }
        other => panic!("expected blocked remount state, got {other:?}"),
    }
}
