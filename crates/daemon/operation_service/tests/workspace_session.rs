use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use operation_service::workspace_remount::RemountWorkspaceSession;
use operation_service::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};
use workspace::{
    BaseRevision, CallerId, CaptureChangesRequest, CapturedWorkspaceChanges,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LatestSnapshotRequest,
    LayerStackSnapshotRef, LeaseId, ReadonlySnapshotHandle, RemountWorkspaceRequest,
    RemountWorkspaceResult, WorkspaceError, WorkspaceHandle, WorkspaceId, WorkspaceProfile,
    WorkspaceRuntimeHooks, WorkspaceRuntimeService,
};

struct FakeWorkspaceService {
    create_results: Mutex<VecDeque<Result<WorkspaceHandle, WorkspaceError>>>,
    capture_results: Mutex<VecDeque<Result<CapturedWorkspaceChanges, WorkspaceError>>>,
    remount_results: Mutex<VecDeque<Result<RemountWorkspaceResult, WorkspaceError>>>,
    destroy_results: Mutex<VecDeque<Result<DestroyWorkspaceResult, WorkspaceError>>>,
    capture_calls: Mutex<Vec<WorkspaceId>>,
    remount_calls: Mutex<Vec<WorkspaceId>>,
    destroy_calls: Mutex<Vec<WorkspaceId>>,
}

impl FakeWorkspaceService {
    fn new() -> Self {
        Self {
            create_results: Mutex::new(VecDeque::new()),
            capture_results: Mutex::new(VecDeque::new()),
            remount_results: Mutex::new(VecDeque::new()),
            destroy_results: Mutex::new(VecDeque::new()),
            capture_calls: Mutex::new(Vec::new()),
            remount_calls: Mutex::new(Vec::new()),
            destroy_calls: Mutex::new(Vec::new()),
        }
    }

    fn push_create_result(&self, result: Result<WorkspaceHandle, WorkspaceError>) {
        self.create_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    fn push_capture_result(&self, result: Result<CapturedWorkspaceChanges, WorkspaceError>) {
        self.capture_results
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

    fn push_destroy_result(&self, result: Result<DestroyWorkspaceResult, WorkspaceError>) {
        self.destroy_results
            .lock()
            .expect("test operation succeeds")
            .push_back(result);
    }

    fn capture_calls(&self) -> Vec<WorkspaceId> {
        self.capture_calls
            .lock()
            .expect("test operation succeeds")
            .clone()
    }

    fn remount_calls(&self) -> Vec<WorkspaceId> {
        self.remount_calls
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

impl FakeWorkspaceService {
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

    fn latest_snapshot(
        &self,
        _request: LatestSnapshotRequest,
    ) -> Result<ReadonlySnapshotHandle, WorkspaceError> {
        Err(WorkspaceError::SnapshotAcquire {
            source: "latest snapshot not configured".to_owned(),
        })
    }
}

fn manager_with(fake: &Arc<FakeWorkspaceService>) -> WorkspaceSessionService {
    WorkspaceSessionService::new(fake_workspace_runtime(fake))
}

fn fake_workspace_runtime(fake: &Arc<FakeWorkspaceService>) -> Arc<WorkspaceRuntimeService> {
    Arc::new(WorkspaceRuntimeService::from_hooks_for_test(
        WorkspaceRuntimeHooks {
            create_workspace: Box::new({
                let fake = Arc::clone(fake);
                move |request| fake.create_workspace(request)
            }),
            capture_changes: Box::new({
                let fake = Arc::clone(fake);
                move |handle, request| fake.capture_changes(handle, request)
            }),
            remount_workspace: Box::new({
                let fake = Arc::clone(fake);
                move |handle, request| fake.remount_workspace(handle, request)
            }),
            destroy_workspace: Box::new({
                let fake = Arc::clone(fake);
                move |handle, request| fake.destroy_workspace(handle, request)
            }),
            latest_snapshot: Box::new({
                let fake = Arc::clone(fake);
                move |request| fake.latest_snapshot(request)
            }),
        },
    ))
}

fn create_request(caller_id: &str) -> CreateWorkspaceRequest {
    CreateWorkspaceRequest {
        caller_id: CallerId(caller_id.to_owned()),
        workspace_root: PathBuf::from("/workspace"),
        layer_stack_root: PathBuf::from("/layers"),
        profile: WorkspaceProfile::HostCompatible,
    }
}

fn workspace_handle(
    workspace_session_id: &str,
    caller_id: &str,
    lease_id: &str,
) -> WorkspaceHandle {
    let snapshot = LayerStackSnapshotRef {
        lease_id: LeaseId(lease_id.to_owned()),
        manifest_version: 1,
        root_hash: "root".to_owned(),
        layer_paths: vec![PathBuf::from("/lower/one")],
    };
    WorkspaceHandle::without_launch_for_test(
        WorkspaceId(workspace_session_id.to_owned()),
        CallerId(caller_id.to_owned()),
        PathBuf::from("/workspace"),
        WorkspaceProfile::HostCompatible,
        snapshot,
    )
}

fn destroy_result(handle: &WorkspaceHandle) -> DestroyWorkspaceResult {
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

fn capture_result(
    handle: &WorkspaceHandle,
    version: i64,
    root_hash: &str,
) -> CapturedWorkspaceChanges {
    CapturedWorkspaceChanges {
        workspace_id: handle.id.clone(),
        base_revision: BaseRevision {
            version,
            root_hash: root_hash.to_owned(),
            layer_count: handle.snapshot.layer_paths.len(),
        },
        changed_paths: Vec::new(),
        changed_path_kinds: Default::default(),
        protected_drops: Vec::new(),
        stats: None,
        changes: Vec::new(),
        route_stats: layerstack::CaptureRouteStats::default(),
        metadata_path_count: 0,
        spool_dir: None,
    }
}

#[test]
fn workspace_session_resolve_validates_caller_ownership() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    let manager = manager_with(&fake);

    manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");

    let wrong_caller = manager
        .resolve_session(
            WorkspaceId("workspace-1".to_owned()),
            CallerId("caller-2".to_owned()),
        )
        .expect_err("test operation fails");
    assert!(matches!(
        wrong_caller,
        WorkspaceSessionError::CallerMismatch { workspace_session_id, .. }
            if workspace_session_id == WorkspaceId("workspace-1".to_owned())
    ));

    let handler = manager
        .resolve_session(
            WorkspaceId("workspace-1".to_owned()),
            CallerId("caller-1".to_owned()),
        )
        .expect("test operation succeeds");
    assert_eq!(
        handler.workspace_session_id,
        WorkspaceId("workspace-1".to_owned())
    );
}

#[test]
fn workspace_session_create_rolls_back_raw_workspace_when_insert_fails() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-2")));
    let manager = manager_with(&fake);

    manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");
    let error = manager
        .create_workspace_session(create_request("caller-1"))
        .expect_err("test operation fails");

    assert!(matches!(
        error,
        WorkspaceSessionError::DuplicateWorkspaceSessionId { workspace_session_id }
            if workspace_session_id == WorkspaceId("workspace-1".to_owned())
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceId("workspace-1".to_owned())]
    );
    assert!(manager
        .resolve_session(
            WorkspaceId("workspace-1".to_owned()),
            CallerId("caller-1".to_owned()),
        )
        .is_ok());
}

#[test]
fn workspace_session_destroy_failure_retains_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "destroy failed".to_owned(),
    }));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");

    let error = manager
        .destroy_session(handler, DestroyWorkspaceRequest::default())
        .expect_err("test operation fails");

    assert!(matches!(
        error,
        WorkspaceSessionError::Workspace(WorkspaceError::Setup { .. })
    ));
    assert!(manager
        .resolve_session(
            WorkspaceId("workspace-1".to_owned()),
            CallerId("caller-1".to_owned()),
        )
        .is_ok());
}

#[test]
fn workspace_session_successful_destroy_removes_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");

    manager
        .destroy_session(handler, DestroyWorkspaceRequest::default())
        .expect("test operation succeeds");

    let missing = manager
        .resolve_session(
            WorkspaceId("workspace-1".to_owned()),
            CallerId("caller-1".to_owned()),
        )
        .expect_err("test operation fails");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
}

#[test]
fn workspace_session_rejects_stale_handler_before_raw_capture() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");

    manager
        .destroy_session(handler.clone(), DestroyWorkspaceRequest::default())
        .expect("test operation succeeds");

    let error = manager
        .capture_session_changes(
            &handler,
            CaptureChangesRequest {
                include_stats: false,
            },
        )
        .expect_err("test operation fails");

    assert!(matches!(error, WorkspaceSessionError::NotFound { .. }));
    assert!(fake.capture_calls().is_empty());
}

#[test]
fn workspace_session_uses_canonical_handle_for_capture() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle("workspace-1", "caller-1", "lease-1");
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(capture_result(&handle, 2, "root-2")));
    let manager = manager_with(&fake);
    let mut handler = manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");
    handler.handle.id = WorkspaceId("fabricated".to_owned());

    manager
        .capture_session_changes(
            &handler,
            CaptureChangesRequest {
                include_stats: false,
            },
        )
        .expect("test operation succeeds");

    assert_eq!(
        fake.capture_calls(),
        vec![WorkspaceId("workspace-1".to_owned())]
    );
}

#[test]
fn workspace_session_capture_updates_handler_snapshot_consistently() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle("workspace-1", "caller-1", "lease-1");
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(capture_result(&handle, 2, "root-2")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");

    manager
        .capture_session_changes(
            &handler,
            CaptureChangesRequest {
                include_stats: false,
            },
        )
        .expect("test operation succeeds");

    let resolved = manager
        .resolve_session(
            WorkspaceId("workspace-1".to_owned()),
            CallerId("caller-1".to_owned()),
        )
        .expect("test operation succeeds");
    assert_eq!(resolved.handle.snapshot.manifest_version, 2);
    assert_eq!(resolved.handle.snapshot.root_hash, "root-2");
}

#[test]
fn workspace_session_begin_remount_marks_pending_and_rejects_duplicate_begin() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    let manager = manager_with(&fake);
    manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");

    manager
        .begin_remount(WorkspaceId("workspace-1".to_owned()))
        .expect("begin remount succeeds");

    assert!(manager.is_remount_pending(&WorkspaceId("workspace-1".to_owned())));
    let duplicate = manager
        .begin_remount(WorkspaceId("workspace-1".to_owned()))
        .expect_err("duplicate begin is rejected");
    assert!(matches!(
        duplicate,
        WorkspaceSessionError::RemountAlreadyPending { workspace_session_id }
            if workspace_session_id == WorkspaceId("workspace-1".to_owned())
    ));
}

#[test]
fn workspace_session_finish_remount_returns_session_to_active() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    let manager = manager_with(&fake);
    manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");
    manager
        .begin_remount(WorkspaceId("workspace-1".to_owned()))
        .expect("begin remount succeeds");

    manager
        .finish_remount(WorkspaceId("workspace-1".to_owned()))
        .expect("finish remount succeeds");

    assert!(!manager.is_remount_pending(&WorkspaceId("workspace-1".to_owned())));
}

#[test]
fn workspace_session_blocked_remount_reason_is_retained() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    let manager = manager_with(&fake);
    manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");
    manager
        .begin_remount(WorkspaceId("workspace-1".to_owned()))
        .expect("begin remount succeeds");

    manager
        .finish_or_block_remount(
            WorkspaceId("workspace-1".to_owned()),
            Some("fd_pinned_workspace".to_owned()),
        )
        .expect("block remount succeeds");

    assert!(!manager.is_remount_pending(&WorkspaceId("workspace-1".to_owned())));
}

#[test]
fn workspace_session_capture_rejects_pending_remount_before_raw_capture() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");
    manager
        .begin_remount(handler.workspace_session_id.clone())
        .expect("begin remount succeeds");

    let error = manager
        .capture_session_changes(
            &handler,
            CaptureChangesRequest {
                include_stats: false,
            },
        )
        .expect_err("capture rejects pending remount");

    assert!(matches!(
        error,
        WorkspaceSessionError::RemountAlreadyPending { workspace_session_id }
            if workspace_session_id == WorkspaceId("workspace-1".to_owned())
    ));
    assert!(fake.capture_calls().is_empty());
    assert!(manager.is_remount_pending(&WorkspaceId("workspace-1".to_owned())));
}

#[test]
fn workspace_session_destroy_rejects_pending_remount_before_raw_destroy() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");
    manager
        .begin_remount(handler.workspace_session_id.clone())
        .expect("begin remount succeeds");

    let error = manager
        .destroy_session(handler, DestroyWorkspaceRequest::default())
        .expect_err("destroy rejects pending remount");

    assert!(matches!(
        error,
        WorkspaceSessionError::RemountAlreadyPending { workspace_session_id }
            if workspace_session_id == WorkspaceId("workspace-1".to_owned())
    ));
    assert!(fake.destroy_calls().is_empty());
    assert!(manager.is_remount_pending(&WorkspaceId("workspace-1".to_owned())));
}

#[test]
fn workspace_session_apply_remount_refreshes_canonical_handle() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle("workspace-1", "caller-1", "lease-1");
    let mut remounted = workspace_handle("workspace-1", "caller-1", "lease-2");
    remounted.snapshot.manifest_version = 2;
    remounted.snapshot.root_hash = "root-2".to_owned();
    remounted.snapshot.layer_paths = vec![PathBuf::from("/lower/two")];
    remounted.base_revision = remounted.snapshot.base_revision();
    fake.push_create_result(Ok(handle));
    fake.push_remount_result(Ok(RemountWorkspaceResult {
        handle: remounted.clone(),
    }));
    let manager = manager_with(&fake);
    manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");
    let handler = manager
        .begin_remount(WorkspaceId("workspace-1".to_owned()))
        .expect("begin remount succeeds");

    let updated = manager
        .apply_remount(
            &handler,
            RemountWorkspaceRequest {
                layer_paths: vec![PathBuf::from("/lower/two")],
            },
        )
        .expect("apply remount succeeds");

    assert_eq!(
        updated.handle.snapshot.lease_id,
        LeaseId("lease-2".to_owned())
    );
    assert_eq!(updated.handle.snapshot.manifest_version, 2);
    assert_eq!(
        updated.handle.snapshot.layer_paths,
        vec![PathBuf::from("/lower/two")]
    );
    assert_eq!(
        fake.remount_calls(),
        vec![WorkspaceId("workspace-1".to_owned())]
    );
}

#[test]
fn workspace_session_apply_remount_failure_blocks_and_keeps_session_available() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    fake.push_remount_result(Err(WorkspaceError::Setup {
        step: "remount failed".to_owned(),
    }));
    let manager = manager_with(&fake);
    manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");
    let handler = manager
        .begin_remount(WorkspaceId("workspace-1".to_owned()))
        .expect("begin remount succeeds");

    let error = manager
        .apply_remount(
            &handler,
            RemountWorkspaceRequest {
                layer_paths: vec![PathBuf::from("/lower/two")],
            },
        )
        .expect_err("apply remount fails");

    assert!(matches!(
        error,
        WorkspaceSessionError::Workspace(WorkspaceError::Setup { .. })
    ));
    assert!(!manager.is_remount_pending(&WorkspaceId("workspace-1".to_owned())));
    assert!(manager
        .resolve_session(
            WorkspaceId("workspace-1".to_owned()),
            CallerId("caller-1".to_owned()),
        )
        .is_ok());
}

#[test]
fn workspace_session_files_do_not_import_command_service() {
    let service = include_str!("../src/workspace_session/service.rs");
    let session_store = include_str!("../src/workspace_session/service/session_store.rs");
    let error = include_str!("../src/workspace_session/error.rs");

    for source in [service, session_store, error] {
        assert!(!source.contains("crate::command"));
        assert!(!source.contains("CommandOperationService"));
        assert!(!source.contains("CommandRemount"));
    }
}

#[test]
fn workspace_session_rejects_remount_workspace_session_id_mismatch() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    fake.push_remount_result(Ok(RemountWorkspaceResult {
        handle: workspace_handle("workspace-2", "caller-1", "lease-2"),
    }));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");
    let handler = manager
        .begin_remount(handler.workspace_session_id)
        .expect("test operation succeeds");

    let error = manager
        .apply_remount(
            &handler,
            RemountWorkspaceRequest {
                layer_paths: vec![PathBuf::from("/lower/two")],
            },
        )
        .expect_err("test operation fails");

    assert!(matches!(
        error,
        WorkspaceSessionError::RemountWorkspaceSessionIdMismatch { expected, actual }
            if expected == WorkspaceId("workspace-1".to_owned())
                && actual == WorkspaceId("workspace-2".to_owned())
    ));
    assert_eq!(
        fake.remount_calls(),
        vec![WorkspaceId("workspace-1".to_owned())]
    );
    assert!(!manager.is_remount_pending(&WorkspaceId("workspace-1".to_owned())));
    assert!(manager
        .resolve_session(
            WorkspaceId("workspace-1".to_owned()),
            CallerId("caller-1".to_owned()),
        )
        .is_ok());
}

#[test]
fn workspace_session_duplicate_destroy_does_not_call_raw_destroy_twice() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "caller-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request("caller-1"))
        .expect("test operation succeeds");

    manager
        .destroy_session(handler.clone(), DestroyWorkspaceRequest::default())
        .expect("test operation succeeds");
    let duplicate = manager
        .destroy_session(handler, DestroyWorkspaceRequest::default())
        .expect_err("test operation fails");

    assert!(matches!(duplicate, WorkspaceSessionError::NotFound { .. }));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceId("workspace-1".to_owned())]
    );
}
