use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use sandbox_observability_telemetry::Observer;
use sandbox_operation_contract::{OperationRequest, OperationScope};
use sandbox_runtime::command::ExecCommandInput;
use sandbox_runtime::workspace_session::{
    FinalizePolicy, MplaLifecycleRoots, WorkspaceSessionError, WorkspaceSessionService,
};
use sandbox_runtime::{
    CommandOperationService, LayerStackService, SandboxRuntimeOperations,
    StorageAdminCapabilityProfile, WorkloadCgroupLimits,
};
use sandbox_runtime_namespace_process::runner::protocol::RunResult;
use sandbox_runtime_workspace::{
    CapturedWorkspaceChanges, DestroyWorkspaceRequest, FileRunnerOp, NetworkProfile,
    WorkspaceError, WorkspaceHandle, WorkspaceSessionId,
};
use serde_json::json;

mod support;
use support::{FakeLaunchDriver, FakeWorkspaceService, ScriptedCommandYield};

fn manager_with(fake: &Arc<FakeWorkspaceService>) -> WorkspaceSessionService {
    WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(fake)),
        support::observed_layerstack_service(Observer::disabled()),
        Observer::disabled(),
    )
}

fn create_request() -> sandbox_runtime::workspace_session::CreateSessionRequest {
    support::create_request()
}

fn workspace_handle(workspace_session_id: &str, lease_id: &str) -> WorkspaceHandle {
    workspace_handle_with_profile(workspace_session_id, lease_id, NetworkProfile::Shared)
}

fn workspace_handle_with_profile(
    workspace_session_id: &str,
    lease_id: &str,
    network: NetworkProfile,
) -> WorkspaceHandle {
    support::workspace_handle(
        workspace_session_id,
        lease_id,
        PathBuf::from("/workspace"),
        network,
    )
}

fn empty_capture(handle: &WorkspaceHandle) -> CapturedWorkspaceChanges {
    CapturedWorkspaceChanges {
        workspace_session_id: handle.id.clone(),
        base_revision: handle.base_revision(),
        base_manifest: handle.snapshot.manifest.clone(),
        changed_path_kinds: Default::default(),
        protected_drops: Vec::new(),
        stats: None,
        metadata_path_count: 0,
        changed_paths: Vec::new(),
        changes: Vec::new(),
    }
}

fn exec_input(workspace_session_id: Option<WorkspaceSessionId>, yield_ms: u64) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id,
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(yield_ms),
    }
}

fn ok_run_result() -> RunResult {
    RunResult {
        exit_code: 0,
        payload: json!({ "status": "ok" }),
    }
}

fn wait_until(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let stop_at = Instant::now() + deadline;
    while Instant::now() < stop_at {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

#[test]
fn workspace_session_resolve_returns_session_by_id() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let manager = manager_with(&fake);

    manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");

    let handler = manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .expect("test operation succeeds");
    assert_eq!(
        handler.workspace_session_id,
        WorkspaceSessionId("workspace-1".to_owned())
    );
}

#[test]
fn duplicate_workspace_id_is_rejected_before_raw_create_or_cgroup_mutation() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-2")));
    let cgroup_root = temp_root().join("cgroup-root");
    let manager = WorkspaceSessionService::with_cgroup_root(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        support::observed_layerstack_service(Observer::disabled()),
        Some(cgroup_root.clone()),
        Observer::disabled(),
    );

    manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");
    let leaf = cgroup_root.join("workspace-workspace-1");
    std::fs::write(leaf.join("unknown.owner"), "existing session owner")
        .expect("inject an existing-session cgroup owner");
    let error = manager
        .create_workspace_session(create_request())
        .expect_err("test operation fails");

    assert!(matches!(
        error,
        WorkspaceSessionError::DuplicateWorkspaceSessionId { workspace_session_id }
            if workspace_session_id == WorkspaceSessionId("workspace-1".to_owned())
    ));
    assert_eq!(fake.create_requests().len(), 1);
    assert!(fake.destroy_calls().is_empty());
    assert_eq!(
        std::fs::read_to_string(leaf.join("unknown.owner"))
            .expect("existing session cgroup remains intact"),
        "existing session owner"
    );
    assert!(manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .is_ok());
    assert!(fake.commit_destroy_calls().is_empty());
}

#[test]
fn concurrent_duplicate_workspace_id_has_one_raw_create_owner() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-race", "lease-1")));
    fake.push_create_result(Ok(workspace_handle("workspace-race", "lease-2")));
    let manager = Arc::new(manager_with(&fake));
    let (create_entered, release_create) = fake.park_next_create();

    let creating_manager = Arc::clone(&manager);
    let creator =
        std::thread::spawn(move || creating_manager.create_workspace_session(create_request()));
    create_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("first creator holds the identity reservation inside raw create");

    let duplicate = manager
        .create_workspace_session(create_request())
        .expect_err("a concurrent creator cannot share the reservation");
    assert!(matches!(
        duplicate,
        WorkspaceSessionError::DuplicateWorkspaceSessionId { workspace_session_id }
            if workspace_session_id == WorkspaceSessionId("workspace-race".to_owned())
    ));
    assert_eq!(fake.create_requests().len(), 1);
    assert!(fake.destroy_calls().is_empty());

    release_create.send(()).expect("release first raw create");
    let handler = creator
        .join()
        .expect("creator thread")
        .expect("reserved creator succeeds");
    assert_eq!(
        handler.workspace_session_id,
        WorkspaceSessionId("workspace-race".to_owned())
    );
    assert_eq!(fake.create_requests().len(), 1);
}

#[test]
fn failed_raw_create_releases_identity_reservation_for_retry() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_session_id = WorkspaceSessionId("workspace-retry".to_owned());
    fake.push_workspace_session_id(workspace_session_id.clone());
    fake.push_create_result(Err(WorkspaceError::Setup {
        step: "injected create failure".to_owned(),
    }));
    fake.push_create_result(Ok(workspace_handle("workspace-retry", "lease-2")));
    let manager = manager_with(&fake);

    let first = manager
        .create_workspace_session(create_request())
        .expect_err("injected raw create fails");
    assert!(matches!(
        first,
        WorkspaceSessionError::Workspace(WorkspaceError::Setup { .. })
    ));

    let retried = manager
        .create_workspace_session(create_request())
        .expect("retry can reserve the same identity");
    assert_eq!(retried.workspace_session_id, workspace_session_id.clone());
    assert_eq!(
        fake.create_requests(),
        vec![
            sandbox_runtime_workspace::CreateWorkspaceRequest {
                workspace_session_id: workspace_session_id.clone(),
                network: NetworkProfile::Shared,
            },
            sandbox_runtime_workspace::CreateWorkspaceRequest {
                workspace_session_id,
                network: NetworkProfile::Shared,
            },
        ]
    );
}

#[test]
fn workspace_session_destroy_failure_retains_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "destroy failed".to_owned(),
    }));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");

    let error = manager
        .destroy_session(handler, DestroyWorkspaceRequest::default())
        .expect_err("test operation fails");

    assert!(matches!(
        error,
        WorkspaceSessionError::Workspace(WorkspaceError::Setup { .. })
    ));
    assert!(manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .is_ok());
    assert!(fake.commit_destroy_calls().is_empty());
}

#[test]
fn cgroup_removal_failure_retries_without_repeating_raw_workspace_destroy() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("cgroup-retry", "lease-1")));
    let cgroup_root = temp_root().join("cgroup-root");
    let manager = WorkspaceSessionService::with_cgroup_root(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        support::observed_layerstack_service(Observer::disabled()),
        Some(cgroup_root.clone()),
        Observer::disabled(),
    );
    let handler = manager
        .create_workspace_session(create_request())
        .expect("session creates");
    let leaf = cgroup_root.join("workspace-cgroup-retry");
    std::fs::write(leaf.join("unknown.owner"), "retained").expect("inject removal failure");

    let first = manager
        .destroy_session(handler.clone(), DestroyWorkspaceRequest::default())
        .expect_err("cgroup failure remains visible");
    assert!(matches!(
        first,
        WorkspaceSessionError::TeardownIncomplete { failures, .. }
            if failures.iter().any(|failure| failure.contains("workload-cgroup"))
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![handler.workspace_session_id.clone()]
    );
    assert!(fake.commit_destroy_calls().is_empty());
    assert!(leaf.exists());

    std::fs::remove_file(leaf.join("unknown.owner")).expect("release injected owner");
    manager
        .destroy_session(handler.clone(), DestroyWorkspaceRequest::default())
        .expect("retry completes remaining cgroup resource");
    assert_eq!(
        fake.destroy_calls(),
        vec![handler.workspace_session_id.clone()],
        "raw workspace teardown is recorded before the retry"
    );
    assert_eq!(
        fake.commit_destroy_calls(),
        vec![handler.workspace_session_id.clone()]
    );
    assert!(!leaf.exists());
    assert!(matches!(
        manager.resolve_session(handler.workspace_session_id),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
}

#[test]
fn cgroup_kill_and_drained_events_are_removed_with_the_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("cgroup-drained", "lease-1")));
    let cgroup_root = temp_root().join("cgroup-root");
    let manager = WorkspaceSessionService::with_cgroup_root(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        support::observed_layerstack_service(Observer::disabled()),
        Some(cgroup_root.clone()),
        Observer::disabled(),
    );
    let handler = manager
        .create_workspace_session(create_request())
        .expect("session creates");
    let leaf = cgroup_root.join("workspace-cgroup-drained");
    std::fs::write(leaf.join("cgroup.kill"), "").expect("create kill control");
    std::fs::write(leaf.join("cgroup.events"), "populated 0\nfrozen 0\n")
        .expect("create drained events control");

    manager
        .destroy_session(handler.clone(), DestroyWorkspaceRequest::default())
        .expect("drained cgroup cleanup succeeds");

    assert!(!leaf.exists());
    assert_eq!(
        fake.destroy_calls(),
        vec![handler.workspace_session_id.clone()]
    );
    assert!(matches!(
        manager.resolve_session(handler.workspace_session_id),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
}

#[test]
fn workspace_cgroup_kill_cleanup_is_leaf_scoped_and_preserves_peer() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("pressured", "lease-pressure")));
    fake.push_create_result(Ok(workspace_handle("peer", "lease-peer")));
    let cgroup_root = temp_root().join("cgroup-root");
    let manager = WorkspaceSessionService::with_workload_cgroup(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        support::observed_layerstack_service(Observer::disabled()),
        cgroup_root.clone(),
        WorkloadCgroupLimits {
            nano_cpus: 500_000_000,
            memory_high_bytes: 64 * 1024 * 1024,
            memory_max_bytes: 96 * 1024 * 1024,
            pids_max: 32,
        },
        Observer::disabled(),
    );
    let pressured = manager
        .create_workspace_session(create_request())
        .expect("pressured session creates");
    let peer = manager
        .create_workspace_session(create_request())
        .expect("peer session creates");
    let pressured_leaf = cgroup_root.join("workspace-pressured");
    let peer_leaf = cgroup_root.join("workspace-peer");
    std::fs::write(pressured_leaf.join("cgroup.kill"), "").expect("create kill control");
    std::fs::write(pressured_leaf.join("cgroup.events"), "populated 0\n")
        .expect("record pressure workload drained");
    std::fs::write(peer_leaf.join("cgroup.procs"), "4242").expect("record peer cgroup ownership");

    manager
        .destroy_session(pressured.clone(), DestroyWorkspaceRequest::default())
        .expect("pressured leaf cleanup succeeds");

    assert!(!pressured_leaf.exists());
    assert!(peer_leaf.is_dir());
    assert_eq!(
        std::fs::read_to_string(peer_leaf.join("cgroup.procs"))
            .expect("peer cgroup ownership remains readable"),
        "4242"
    );
    assert!(manager
        .resolve_session(peer.workspace_session_id.clone())
        .is_ok());
    assert_eq!(fake.destroy_calls(), vec![pressured.workspace_session_id]);

    manager
        .destroy_session(peer.clone(), DestroyWorkspaceRequest::default())
        .expect("peer cleanup remains independently joinable");
    assert!(!peer_leaf.exists());
    assert!(matches!(
        manager.resolve_session(peer.workspace_session_id),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
}

#[test]
fn populated_cgroup_without_kill_is_retained_until_a_bounded_retry() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("cgroup-populated", "lease-1")));
    let cgroup_root = temp_root().join("cgroup-root");
    let manager = WorkspaceSessionService::with_cgroup_root(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        support::observed_layerstack_service(Observer::disabled()),
        Some(cgroup_root.clone()),
        Observer::disabled(),
    );
    let handler = manager
        .create_workspace_session(create_request())
        .expect("session creates");
    let leaf = cgroup_root.join("workspace-cgroup-populated");
    std::fs::write(leaf.join("cgroup.events"), "populated 1\n")
        .expect("create populated events control");

    let first = manager
        .destroy_session(handler.clone(), DestroyWorkspaceRequest::default())
        .expect_err("populated leaf without cgroup.kill fails closed");

    assert!(matches!(
        first,
        WorkspaceSessionError::TeardownIncomplete { failures, .. }
            if failures.iter().any(|failure| failure.contains("cgroup.kill is unavailable"))
    ));
    assert!(leaf.exists());
    assert_eq!(
        fake.destroy_calls(),
        vec![handler.workspace_session_id.clone()]
    );

    std::fs::write(leaf.join("cgroup.events"), "populated 0\n").expect("record drained events");
    manager
        .destroy_session(handler.clone(), DestroyWorkspaceRequest::default())
        .expect("retry removes the drained leaf");

    assert!(!leaf.exists());
    assert_eq!(
        fake.destroy_calls(),
        vec![handler.workspace_session_id.clone()],
        "raw workspace teardown is not repeated while cgroup cleanup retries"
    );
    assert!(matches!(
        manager.resolve_session(handler.workspace_session_id),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
}

#[test]
fn configured_workload_cgroup_limit_failure_aborts_and_rolls_back_raw_workspace() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("cgroup-setup-fail", "lease-1")));
    let cgroup_root = temp_root().join("cgroup-root");
    let leaf = cgroup_root.join("workspace-cgroup-setup-fail");
    std::fs::create_dir_all(&leaf).expect("precreate injected leaf");
    std::os::unix::fs::symlink(&cgroup_root, leaf.join("cpu.max"))
        .expect("cpu.max write fails on directory target");
    let manager = WorkspaceSessionService::with_workload_cgroup(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        support::observed_layerstack_service(Observer::disabled()),
        cgroup_root,
        WorkloadCgroupLimits {
            nano_cpus: 1_000_000_000,
            memory_high_bytes: 64 * 1024 * 1024,
            memory_max_bytes: 96 * 1024 * 1024,
            pids_max: 32,
        },
        Observer::disabled(),
    );

    let error = manager
        .create_workspace_session(create_request())
        .expect_err("configured limit write must fail closed");
    assert!(matches!(
        error,
        WorkspaceSessionError::WorkloadCgroupSetupFailed {
            workspace_session_id,
            rollback_diagnostic: None,
            ..
        } if workspace_session_id == WorkspaceSessionId("cgroup-setup-fail".to_owned())
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("cgroup-setup-fail".to_owned())]
    );
    assert_eq!(
        fake.commit_destroy_calls(),
        vec![WorkspaceSessionId("cgroup-setup-fail".to_owned())]
    );
    assert!(!leaf.exists(), "partial cgroup leaf is rolled back");
}

#[test]
fn create_cgroup_cleanup_failure_is_visible_and_joinable_without_repeating_raw_destroy() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("cgroup-create-retry", "lease-1")));
    let cgroup_root = temp_root().join("cgroup-root");
    let leaf = cgroup_root.join("workspace-cgroup-create-retry");
    std::fs::create_dir_all(&leaf).expect("precreate injected leaf");
    std::os::unix::fs::symlink(&cgroup_root, leaf.join("cpu.max"))
        .expect("cpu.max write fails on directory target");
    std::fs::write(leaf.join("unknown.owner"), "retained")
        .expect("unknown owner blocks setup rollback");
    let manager = WorkspaceSessionService::with_workload_cgroup(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        support::observed_layerstack_service(Observer::disabled()),
        cgroup_root,
        WorkloadCgroupLimits {
            nano_cpus: 1_000_000_000,
            memory_high_bytes: 64 * 1024 * 1024,
            memory_max_bytes: 96 * 1024 * 1024,
            pids_max: 32,
        },
        Observer::disabled(),
    );

    let first = manager
        .create_workspace_session(create_request())
        .expect_err("failed cgroup rollback remains visible");
    assert!(matches!(
        first,
        WorkspaceSessionError::TeardownIncomplete {
            ref workspace_session_id,
            ref failures,
        } if workspace_session_id == &WorkspaceSessionId("cgroup-create-retry".to_owned())
            && failures.iter().any(|failure| failure.contains("workload-cgroup"))
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("cgroup-create-retry".to_owned())]
    );
    assert!(fake.commit_destroy_calls().is_empty());
    assert!(leaf.exists());

    std::fs::remove_file(leaf.join("unknown.owner")).expect("release injected owner");
    manager
        .guarded_destroy(WorkspaceSessionId("cgroup-create-retry".to_owned()), None)
        .expect("joinable cleanup retry converges");
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("cgroup-create-retry".to_owned())],
        "successful raw rollback is never repeated"
    );
    assert_eq!(
        fake.commit_destroy_calls(),
        vec![WorkspaceSessionId("cgroup-create-retry".to_owned())]
    );
    assert!(!leaf.exists());
}

#[test]
fn create_raw_rollback_failure_retains_exact_handle_for_joinable_retry() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle("raw-create-retry", "lease-1");
    fake.push_create_result(Ok(handle.clone()));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "injected raw rollback failure".to_owned(),
    }));
    fake.push_destroy_result(Ok(support::destroy_result(&handle)));
    let cgroup_root = temp_root().join("cgroup-root");
    let leaf = cgroup_root.join("workspace-raw-create-retry");
    std::fs::create_dir_all(&leaf).expect("precreate injected leaf");
    std::os::unix::fs::symlink(&cgroup_root, leaf.join("cpu.max"))
        .expect("cpu.max write fails on directory target");
    let manager = WorkspaceSessionService::with_workload_cgroup(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        support::observed_layerstack_service(Observer::disabled()),
        cgroup_root,
        WorkloadCgroupLimits {
            nano_cpus: 1_000_000_000,
            memory_high_bytes: 64 * 1024 * 1024,
            memory_max_bytes: 96 * 1024 * 1024,
            pids_max: 32,
        },
        Observer::disabled(),
    );

    let first = manager
        .create_workspace_session(create_request())
        .expect_err("raw rollback failure remains visible");
    assert!(matches!(
        first,
        WorkspaceSessionError::TeardownIncomplete { ref failures, .. }
            if failures.iter().any(|failure| failure.contains("workspace rollback"))
    ));
    assert!(!leaf.exists(), "successful cgroup rollback is terminal");
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("raw-create-retry".to_owned())]
    );
    assert!(fake.commit_destroy_calls().is_empty());

    manager
        .guarded_destroy(WorkspaceSessionId("raw-create-retry".to_owned()), None)
        .expect("retry uses the retained raw handle");
    assert_eq!(
        fake.destroy_calls(),
        vec![
            WorkspaceSessionId("raw-create-retry".to_owned()),
            WorkspaceSessionId("raw-create-retry".to_owned()),
        ]
    );
    assert_eq!(
        fake.commit_destroy_calls(),
        vec![WorkspaceSessionId("raw-create-retry".to_owned())]
    );
}

#[test]
fn isolated_ip_waits_for_cleanup_and_rejects_finalize_failed_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle_with_profile(
        "ws-forward-cleanup",
        "lease-1",
        NetworkProfile::Isolated,
    )));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "cleanup failed".to_owned(),
    }));
    let env = support::build_services(Arc::clone(&fake));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;
    let (destroy_entered, release_destroy) = fake.park_next_destroy();

    let destroy_workspace = Arc::clone(&env.workspace);
    let destroy_id = workspace_session_id.clone();
    let destroy = std::thread::spawn(move || destroy_workspace.guarded_destroy(destroy_id, None));
    destroy_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("guarded destroy reached the workspace hook under the session gate");

    let (lookup_started_tx, lookup_started_rx) = mpsc::channel();
    let (lookup_result_tx, lookup_result_rx) = mpsc::channel();
    let lookup_workspace = Arc::clone(&env.workspace);
    let lookup_id = workspace_session_id.clone();
    let lookup = std::thread::spawn(move || {
        lookup_started_tx
            .send(())
            .expect("announce isolated IP lookup");
        lookup_result_tx
            .send(lookup_workspace.isolated_ip(&lookup_id))
            .expect("send isolated IP lookup result");
    });
    lookup_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("isolated IP lookup thread started");
    assert!(
        lookup_result_rx
            .recv_timeout(Duration::from_millis(200))
            .is_err(),
        "isolated IP resolution must wait for the in-flight cleanup transaction"
    );

    release_destroy.send(()).expect("release guarded destroy");
    assert!(matches!(
        destroy.join().expect("guarded destroy thread"),
        Err(WorkspaceSessionError::Workspace(
            WorkspaceError::Setup { .. }
        ))
    ));
    assert!(matches!(
        lookup_result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("isolated IP lookup completes after cleanup"),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    lookup.join().expect("isolated IP lookup thread");
}

#[test]
fn workspace_session_successful_destroy_removes_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");

    manager
        .destroy_session(handler, DestroyWorkspaceRequest::default())
        .expect("test operation succeeds");

    assert_eq!(
        fake.commit_destroy_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );

    let missing = manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .expect_err("test operation fails");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
}

#[test]
fn workspace_session_duplicate_destroy_does_not_call_raw_destroy_twice() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request())
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
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
}

#[test]
fn stale_destroy_handler_cannot_destroy_recreated_same_id_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-reused", "lease-old")));
    let manager = manager_with(&fake);
    let stale = manager
        .create_workspace_session(create_request())
        .expect("old generation creates");
    manager
        .destroy_session(stale.clone(), DestroyWorkspaceRequest::default())
        .expect("old generation destroys");

    fake.push_create_result(Ok(workspace_handle("workspace-reused", "lease-new")));
    let current = manager
        .create_workspace_session(create_request())
        .expect("new generation reuses the public id");
    let stale_error = manager
        .destroy_session(stale, DestroyWorkspaceRequest::default())
        .expect_err("stale generation cannot target the replacement");

    assert!(matches!(
        stale_error,
        WorkspaceSessionError::NotFound {
            workspace_session_id
        } if workspace_session_id == current.workspace_session_id
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![current.workspace_session_id.clone()]
    );
    assert_eq!(
        manager
            .resolve_session(current.workspace_session_id.clone())
            .expect("replacement remains resolvable"),
        current
    );
}

// ---------------------------------------------------------------------------
// Finalize-policy matrix (§5): the completion edge is the only trigger.
// ---------------------------------------------------------------------------

#[test]
fn no_op_session_survives_command_completion() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "done\n",
    )));
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    fake.push_create_result(Ok(workspace_handle("ws-noop", "lease-1")));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;

    let output = env
        .command
        .exec_command(exec_input(Some(workspace_session_id.clone()), 250))
        .expect("session command completes");

    assert_eq!(
        output.workspace_session_id,
        Some(workspace_session_id.clone())
    );
    assert!(fake.destroy_calls().is_empty());
    assert!(fake.capture_calls().is_empty());
    assert!(env.workspace.resolve_session(workspace_session_id).is_ok());
}

#[test]
fn publish_then_destroy_session_finalizes_when_last_command_completes() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "done\n",
    )));
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    fake.push_create_result(Ok(workspace_handle("ws-ptd", "lease-1")));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(support::create_request_with_policy(
            FinalizePolicy::PublishThenDestroy,
        ))
        .expect("session create succeeds")
        .workspace_session_id;

    let output = env
        .command
        .exec_command(exec_input(Some(workspace_session_id.clone()), 250))
        .expect("session command completes");

    assert_eq!(
        output.workspace_session_id,
        Some(workspace_session_id.clone())
    );
    assert!(
        wait_until(Duration::from_secs(5), || !fake.destroy_calls().is_empty()),
        "publish_then_destroy finalizes once the ledger drains"
    );
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);
    assert_eq!(fake.capture_calls(), vec![workspace_session_id.clone()]);
    let missing = env
        .workspace
        .resolve_session(workspace_session_id)
        .expect_err("finalized session is gone");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn explicit_publish_rejects_publish_then_destroy_policy_without_side_effects() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "done\n",
    )));
    let layerstack = support::observed_layerstack_service(Observer::disabled());
    let env = support::build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        launch_driver,
        Arc::clone(&layerstack),
    );
    fake.push_create_result(Ok(workspace_handle("ws-policy", "lease-1")));
    let handler = env
        .workspace
        .create_workspace_session(support::create_request_with_policy(
            FinalizePolicy::PublishThenDestroy,
        ))
        .expect("publish_then_destroy session create succeeds");
    let workspace_session_id = handler.workspace_session_id.clone();
    let before =
        sandbox_runtime_layerstack::LayerStack::open(layerstack.layer_stack_root().to_path_buf())
            .expect("open test layerstack")
            .read_active_manifest()
            .expect("read active manifest before rejected publish");

    let error = env
        .workspace
        .publish_workspace_session(workspace_session_id.clone(), None)
        .expect_err("explicit publish must reject an implicit-policy session");

    assert!(matches!(error, WorkspaceSessionError::NotFound { .. }));
    assert!(fake.capture_calls().is_empty());
    assert!(fake.destroy_calls().is_empty());
    let after =
        sandbox_runtime_layerstack::LayerStack::open(layerstack.layer_stack_root().to_path_buf())
            .expect("reopen test layerstack")
            .read_active_manifest()
            .expect("read active manifest after rejected publish");
    assert_eq!(after, before, "explicit rejection cannot publish a layer");
    assert!(
        env.workspace
            .resolve_session(workspace_session_id.clone())
            .is_ok(),
        "the implicit-policy session remains active for its ordinary completion edge"
    );

    fake.push_capture_result(Ok(empty_capture(&handler.handle)));
    let output = env
        .command
        .exec_command(exec_input(Some(workspace_session_id.clone()), 250))
        .expect("targeted command completes through the implicit policy");
    assert_eq!(
        output.workspace_session_id,
        Some(workspace_session_id.clone())
    );
    assert!(
        wait_until(Duration::from_secs(5), || !fake.destroy_calls().is_empty()),
        "the unchanged implicit completion edge still captures and destroys"
    );
    assert_eq!(fake.capture_calls(), vec![workspace_session_id.clone()]);
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);
    assert!(matches!(
        env.workspace.resolve_session(workspace_session_id),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn rider_command_defers_finalization_until_last_completion() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-rider", "lease-1")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let launcher = launch_driver.launcher();
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let first = env
        .command
        .exec_command(exec_input(None, 0))
        .expect("implicit session command starts");
    let workspace_session_id = first
        .workspace_session_id
        .clone()
        .expect("exec_command returns the session id");
    let rider = env
        .command
        .exec_command(exec_input(Some(workspace_session_id.clone()), 0))
        .expect("rider attaches to the running session");
    assert_eq!(
        rider.workspace_session_id,
        Some(workspace_session_id.clone())
    );

    let rider_id = rider.command_session_id.expect("rider is running");
    launcher.complete_request(&rider_id.0, ok_run_result());
    assert!(
        !wait_until(Duration::from_millis(300), || {
            !fake.destroy_calls().is_empty()
        }),
        "rider completion must not finalize while the first command runs"
    );
    assert!(env
        .workspace
        .resolve_session(workspace_session_id.clone())
        .is_ok());

    let first_id = first.command_session_id.expect("first command is running");
    launcher.complete_request(&first_id.0, ok_run_result());
    assert!(
        wait_until(Duration::from_secs(5), || !fake.destroy_calls().is_empty()),
        "last completion drains the ledger and finalizes"
    );
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);
    let missing = env
        .workspace
        .resolve_session(workspace_session_id)
        .expect_err("finalized session is gone");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
}

#[test]
fn sweep_remount_does_not_finalize_idle_publish_then_destroy_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-sweep", "lease-1")));
    let env = support::build_services(Arc::clone(&fake));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(support::create_request_with_policy(
            FinalizePolicy::PublishThenDestroy,
        ))
        .expect("session create succeeds")
        .workspace_session_id;

    for swept_id in env.workspace.session_ids() {
        let _ = env.workspace.remount_session(&swept_id);
    }

    assert!(
        env.workspace.resolve_session(workspace_session_id).is_ok(),
        "an idle publish_then_destroy session survives the remount sweep"
    );
    assert!(fake.destroy_calls().is_empty());
    assert!(fake.capture_calls().is_empty());
}

#[test]
fn sync_op_racing_last_completion_blocks_on_gate_and_gets_not_found() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-race", "lease-1")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "done\n",
    )));
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let (entered, release) = fake.park_next_destroy();

    let output = env
        .command
        .exec_command(exec_input(None, 0))
        .expect("implicit session command starts");
    let workspace_session_id = output
        .workspace_session_id
        .expect("exec_command returns the session id");
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("finalize reached the destroy hook under the gate");

    let file_op_workspace = Arc::clone(&env.workspace);
    let file_op_id = workspace_session_id.clone();
    let file_op = std::thread::spawn(move || {
        file_op_workspace.run_file_op(
            &file_op_id,
            FileRunnerOp::ReadFile {
                rel: "f.txt".to_owned(),
                max_bytes: 16,
            },
        )
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        fake.run_file_op_calls().is_empty(),
        "the file op must wait on the session gate while finalize holds it"
    );

    release.send(()).expect("release the parked destroy");
    let result = file_op.join().expect("file op thread");
    assert!(matches!(
        result,
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert!(
        fake.run_file_op_calls().is_empty(),
        "a sync op racing the last completion never runs against the finalized session"
    );
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn launch_failure_completes_under_the_held_guard_without_deadlock() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-launch-fail", "lease-1")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_spawn_error(
        sandbox_runtime::command::CommandServiceError::InvalidCommand {
            message: "scripted spawn failure".to_owned(),
        },
    );
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let (result_tx, result_rx) = mpsc::channel();
    let command = Arc::clone(&env.command);
    std::thread::spawn(move || {
        let _ = result_tx.send(command.exec_command(exec_input(None, 0)));
    });

    let result = result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("launch-failure completion must not deadlock on the admission gate");
    assert!(matches!(
        result,
        Err(sandbox_runtime::command::CommandServiceError::CommandIo { .. })
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("ws-launch-fail".to_owned())],
        "the failed launch completes through the ordinary trigger and finalizes"
    );
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn faulty_destroy_waits_for_command_owner_release_before_deleting_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-faulty", "lease-1")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let launcher = launch_driver.launcher();
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;
    let output = env
        .command
        .exec_command(exec_input(Some(workspace_session_id.clone()), 0))
        .expect("command starts");
    let command_session_id = output.command_session_id.expect("command is running");

    let lease_errors = env.workspace.destroy_faulty_session(&workspace_session_id);
    assert_eq!(lease_errors.len(), 1);
    assert!(
        lease_errors[0].contains("1 active command scratch owner(s) remain"),
        "faulty destroy must expose the live command owner: {lease_errors:?}"
    );
    assert!(
        fake.destroy_calls().is_empty(),
        "workspace storage remains intact while command scratch is live"
    );

    launcher.complete_request(&command_session_id.0, ok_run_result());
    assert!(
        wait_until(Duration::from_secs(5), || {
            env.command.active_namespace_executions().is_empty()
        }),
        "the command owner must reach terminal release before retrying destroy"
    );

    let lease_errors = env.workspace.destroy_faulty_session(&workspace_session_id);
    assert!(lease_errors.is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![workspace_session_id.clone()],
        "the retry destroys exactly once after command-owner release"
    );
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn guarded_destroy_accepts_a_finalize_failed_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle("ws-stuck", "lease-1");
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(empty_capture(&handle)));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "finalize destroy failed".to_owned(),
    }));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "done\n",
    )));
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let _ = env
        .command
        .exec_command(exec_input(None, 250))
        .expect("implicit session command completes");
    assert!(
        wait_until(Duration::from_secs(5), || !fake.destroy_calls().is_empty()),
        "finalize attempts the destroy"
    );
    let workspace_session_id = WorkspaceSessionId("ws-stuck".to_owned());
    assert_eq!(fake.destroy_calls().len(), 1);

    let result = env
        .workspace
        .guarded_destroy(workspace_session_id.clone(), None)
        .expect("guarded destroy recovers a finalize_failed session");
    assert_eq!(result.workspace_session_id, workspace_session_id);
    assert_eq!(fake.destroy_calls().len(), 2);
    let missing = env
        .workspace
        .resolve_session(workspace_session_id)
        .expect_err("recovered session is gone");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn gates_map_does_not_grow_on_dead_id_touches() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let env = support::build_services(Arc::clone(&fake));
    let dead = WorkspaceSessionId("ws-dead".to_owned());

    let file_op = env.workspace.run_file_op(
        &dead,
        FileRunnerOp::ReadFile {
            rel: "f.txt".to_owned(),
            max_bytes: 16,
        },
    );
    assert!(matches!(
        file_op,
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert_eq!(env.workspace.gate_entry_count(), 0);

    let destroy = env.workspace.guarded_destroy(dead.clone(), None);
    assert!(matches!(
        destroy,
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert_eq!(env.workspace.gate_entry_count(), 0);

    let admission = env.command.exec_command(exec_input(Some(dead.clone()), 0));
    assert!(admission.is_err());
    assert_eq!(env.workspace.gate_entry_count(), 0);

    let swept = env.workspace.remount_session(&dead);
    assert_eq!(
        swept.disposition,
        sandbox_runtime::workspace_session::SweptDisposition::SessionGone
    );
    assert_eq!(env.workspace.gate_entry_count(), 0);

    assert!(env.workspace.destroy_faulty_session(&dead).is_empty());
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn sessions_map_stays_free_while_destroy_io_runs() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-io", "lease-1")));
    fake.push_create_result(Ok(workspace_handle("ws-other", "lease-2")));
    let env = support::build_services(Arc::clone(&fake));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;
    let other_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("second session create succeeds")
        .workspace_session_id;

    let (entered, release) = fake.park_next_destroy();
    let destroy_workspace = Arc::clone(&env.workspace);
    let destroy_id = workspace_session_id.clone();
    let destroyer = std::thread::spawn(move || destroy_workspace.guarded_destroy(destroy_id, None));
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("destroy reached the workspace hook");

    let (done_tx, done_rx) = mpsc::channel();
    let read_workspace = Arc::clone(&env.workspace);
    let read_id = other_id.clone();
    std::thread::spawn(move || {
        let resolved = read_workspace.resolve_session(read_id).is_ok();
        let ids = read_workspace.session_ids();
        let _ = done_tx.send((resolved, ids));
    });
    let (resolved, ids) = done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("sessions map reads must not block behind destroy I/O");
    assert!(resolved);
    assert!(ids.contains(&other_id));

    release.send(()).expect("release the parked destroy");
    destroyer
        .join()
        .expect("destroy thread")
        .expect("guarded destroy succeeds");
}

// ---------------------------------------------------------------------------
// Daemon operation dispatch surface.
// ---------------------------------------------------------------------------

#[test]
fn workspace_session_create_operation_defaults_host_profile_and_projects_minimal_json(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let operations = operations_with_fake(&fake)?;

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request("create_workspace_session", json!({})),
    )
    .into_json_value();

    assert_eq!(
        response,
        json!({
            "workspace_session_id": "workspace-1",
            "network_profile": "shared",
            "finalize_policy": "no_op",
        })
    );
    assert_eq!(
        fake.create_requests(),
        vec![support::raw_create_request("workspace-1")]
    );
    Ok(())
}

#[test]
fn workspace_session_create_operation_accepts_isolated_profile(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle_with_profile(
        "workspace-1",
        "lease-1",
        NetworkProfile::Isolated,
    )));
    let operations = operations_with_fake(&fake)?;

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "create_workspace_session",
            json!({ "network_profile": "isolated" }),
        ),
    )
    .into_json_value();

    assert_eq!(
        response,
        json!({
            "workspace_session_id": "workspace-1",
            "network_profile": "isolated",
            "finalize_policy": "no_op",
        })
    );
    Ok(())
}

#[test]
fn internal_legacy_scratch_adapter_marks_only_its_created_session(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let operations = operations_with_fake(&fake)?;

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request("create_workspace_session_legacy_scratch_adapter", json!({})),
    )
    .into_json_value();

    assert_eq!(response["workspace_session_id"], "workspace-1");
    let handler = operations
        .workspace_session
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))?;
    assert_eq!(handler.execution_scratch_route().as_str(), "legacy_compat");
    Ok(())
}

#[test]
fn workspace_session_create_operation_rejects_invalid_profiles(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for args in [
        json!({ "network_profile": "unknown" }),
        json!({ "network_profile": "" }),
        json!({ "network_profile": 7 }),
    ] {
        let fake = Arc::new(FakeWorkspaceService::new());
        let operations = operations_with_fake(&fake)?;

        let response = sandbox_runtime::dispatch_operation(
            &operations,
            &runtime_request("create_workspace_session", args),
        )
        .into_json_value();

        assert_eq!(response["error"]["kind"], "invalid_request");
        assert!(fake.create_requests().is_empty());
    }
    Ok(())
}

#[test]
fn publish_mpla_workspace_session_fails_closed_for_missing_session(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let layerstack = layerstack_service()?;
    let mpla_root = temp_root();
    let workspace = Arc::new(
        WorkspaceSessionService::new(
            support::fake_workspace_runtime(Arc::clone(&fake)),
            Arc::clone(&layerstack),
            Observer::disabled(),
        )
        .with_mpla_lifecycle_roots(MplaLifecycleRoots {
            payload_root: mpla_root.join("payload"),
            control_root: mpla_root.join("control"),
            storage_admin_profile: StorageAdminCapabilityProfile::Production,
        }),
    );
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&workspace),
        sandbox_runtime::command::CommandConfig::default(),
        Observer::disabled(),
    ));
    let operations =
        SandboxRuntimeOperations::new(command, workspace, layerstack, support::test_file_service());

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "publish_mpla_workspace_session",
            json!({
                "workspace_session_id": "workspace-mpla-1",
                "branch": "main",
            }),
        ),
    )
    .into_json_value();

    assert_eq!(response["error"]["kind"], "operation_failed");
    assert!(response["error"]["message"]
        .as_str()
        .expect("operation failure has a message")
        .contains("workspace session not found"));
    assert_eq!(
        response["error"]["details"]["workspace_session_id"],
        "workspace-mpla-1"
    );
    assert!(fake.create_requests().is_empty());
    assert!(fake.destroy_calls().is_empty());
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_rejects_invalid_args_without_raw_destroy(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for args in [
        json!({}),
        json!({ "workspace_session_id": "" }),
        json!({ "workspace_session_id": 7 }),
        json!({ "workspace_session_id": "workspace-1", "grace_s": "NaN" }),
        json!({ "workspace_session_id": "workspace-1", "grace_s": -0.1 }),
    ] {
        let fake = Arc::new(FakeWorkspaceService::new());
        let operations = operations_with_fake(&fake)?;

        let response = sandbox_runtime::dispatch_operation(
            &operations,
            &runtime_request("destroy_workspace_session", args),
        )
        .into_json_value();

        assert_eq!(response["error"]["kind"], "invalid_request");
        assert!(fake.destroy_calls().is_empty());
    }
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_unknown_session_does_not_call_raw_destroy(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let operations = operations_with_fake(&fake)?;

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "destroy_workspace_session",
            json!({ "workspace_session_id": "missing" }),
        ),
    )
    .into_json_value();

    assert_eq!(response["error"]["kind"], "operation_failed");
    assert!(fake.destroy_calls().is_empty());
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_rejects_active_commands_without_raw_destroy(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(support::FakeWorkspaceService::new());
    fake.push_create_result(Ok(support::workspace_handle(
        "workspace-1",
        "lease-1",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    )));
    let services = support::build_services(Arc::clone(&fake));
    let workspace_session_id = services
        .workspace
        .create_workspace_session(support::create_request())
        .expect("session create succeeds")
        .workspace_session_id;
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service()?,
        support::test_file_service(),
    );

    let exec_response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "exec_command",
            json!({
                "workspace_session_id": workspace_session_id.0.clone(),
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        ),
    )
    .into_json_value();
    assert_eq!(exec_response["command_session_id"], "namespace_execution_1");
    assert_eq!(
        exec_response["workspace_session_id"],
        workspace_session_id.0
    );

    let destroy_response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "destroy_workspace_session",
            json!({ "workspace_session_id": workspace_session_id.0 }),
        ),
    )
    .into_json_value();

    assert_eq!(destroy_response["error"]["kind"], "operation_failed");
    assert_eq!(
        destroy_response["error"]["details"]["active_command_session_ids"],
        json!(["namespace_execution_1"])
    );
    assert!(fake.destroy_calls().is_empty());
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_success_projects_minimal_json(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let operations = operations_with_fake(&fake)?;
    operations
        .workspace_session
        .create_workspace_session(create_request())
        .expect("session create succeeds");

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "destroy_workspace_session",
            json!({ "workspace_session_id": "workspace-1", "grace_s": 2.5 }),
        ),
    )
    .into_json_value();

    assert_eq!(
        response,
        json!({
            "workspace_session_id": "workspace-1",
            "destroyed": true,
            "evicted_upperdir_bytes": 4096,
        })
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_failure_retains_session(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "destroy failed".to_owned(),
    }));
    let operations = operations_with_fake(&fake)?;
    operations
        .workspace_session
        .create_workspace_session(create_request())
        .expect("session create succeeds");

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "destroy_workspace_session",
            json!({ "workspace_session_id": "workspace-1" }),
        ),
    )
    .into_json_value();

    assert_eq!(response["error"]["kind"], "operation_failed");
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
    assert!(operations
        .workspace_session
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .is_ok());
    Ok(())
}

#[test]
fn workspace_session_files_do_not_import_command_service() {
    let core = include_str!("../src/workspace_session/service/core.rs");
    let admission = include_str!("../src/workspace_session/service/impls/admission.rs");
    let finalize_session =
        include_str!("../src/workspace_session/service/impls/finalize_session.rs");
    let publish_session = include_str!("../src/workspace_session/service/impls/publish_session.rs");
    let guarded_destroy = include_str!("../src/workspace_session/service/impls/guarded_destroy.rs");
    let create_workspace_session =
        include_str!("../src/workspace_session/service/impls/create_workspace_session.rs");
    let destroy_session = include_str!("../src/workspace_session/service/impls/destroy_session.rs");
    let resolve_session = include_str!("../src/workspace_session/service/impls/resolve_session.rs");
    let model = include_str!("../src/workspace_session/service/model.rs");
    let service = include_str!("../src/workspace_session/service.rs");
    let error = include_str!("../src/workspace_session/error.rs");

    for source in [
        core,
        admission,
        finalize_session,
        publish_session,
        guarded_destroy,
        create_workspace_session,
        destroy_session,
        resolve_session,
        model,
        service,
        error,
    ] {
        assert!(!source.contains("crate::command"));
        assert!(!source.contains("CommandOperationService"));
    }
}

fn operations_with_fake(
    fake: &Arc<FakeWorkspaceService>,
) -> Result<SandboxRuntimeOperations, Box<dyn std::error::Error + Send + Sync>> {
    let layerstack = layerstack_service()?;
    let workspace = Arc::new(WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(fake)),
        Arc::clone(&layerstack),
        Observer::disabled(),
    ));
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&workspace),
        sandbox_runtime::command::CommandConfig::default(),
        Observer::disabled(),
    ));
    Ok(SandboxRuntimeOperations::new(
        command,
        workspace,
        layerstack,
        support::test_file_service(),
    ))
}

fn runtime_request(op: &str, args: serde_json::Value) -> OperationRequest {
    OperationRequest::new(op, "req-test", OperationScope::sandbox("sbox-test"), args)
}

fn layerstack_service() -> Result<Arc<LayerStackService>, Box<dyn std::error::Error + Send + Sync>>
{
    let base = temp_root();
    let root = base.join("layer-stack");
    let workspace = base.join("workspace");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&workspace)?;
    sandbox_runtime_layerstack::build_workspace_base(&root, &workspace, false)?;
    Ok(Arc::new(LayerStackService::new(
        root,
        base.join("scratch"),
        sandbox_runtime::LayerstackRuntimeConfig::default(),
        Observer::disabled(),
        support::test_file_service(),
    )?))
}

fn temp_root() -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "sandbox-runtime-workspace-session-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ))
}
