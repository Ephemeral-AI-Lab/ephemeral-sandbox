use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sandbox_observability::Observer;
use sandbox_protocol::{CliOperationScope, Request};
use sandbox_runtime::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};
use sandbox_runtime::{CommandOperationService, LayerStackService, SandboxRuntimeOperations};
use sandbox_runtime_workspace::{
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, CreateWorkspaceRequest,
    DestroyWorkspaceRequest, NetworkProfile, WorkspaceError, WorkspaceHandle, WorkspaceSessionId,
};
use serde_json::json;

mod support;
use support::FakeWorkspaceService;

fn manager_with(fake: &Arc<FakeWorkspaceService>) -> WorkspaceSessionService {
    WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(fake)),
        Observer::disabled(),
    )
}

fn create_request() -> CreateWorkspaceRequest {
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

fn capture_result(
    handle: &WorkspaceHandle,
    version: i64,
    root_hash: &str,
) -> CapturedWorkspaceChanges {
    CapturedWorkspaceChanges {
        workspace_session_id: handle.id.clone(),
        base_revision: BaseRevision {
            version,
            root_hash: root_hash.to_owned(),
            layer_count: handle.snapshot.layer_paths.len(),
        },
        base_manifest: handle.snapshot.manifest.clone(),
        changed_paths: Vec::new(),
        changed_path_kinds: Default::default(),
        protected_drops: Vec::new(),
        stats: None,
        changes: Vec::new(),
        metadata_path_count: 0,
    }
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
fn workspace_session_create_rolls_back_raw_workspace_when_insert_fails() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-2")));
    let manager = manager_with(&fake);

    manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");
    let error = manager
        .create_workspace_session(create_request())
        .expect_err("test operation fails");

    assert!(matches!(
        error,
        WorkspaceSessionError::DuplicateWorkspaceSessionId { workspace_session_id }
            if workspace_session_id == WorkspaceSessionId("workspace-1".to_owned())
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
    assert!(manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .is_ok());
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

    let missing = manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .expect_err("test operation fails");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
}

#[test]
fn workspace_session_rejects_stale_handler_before_raw_capture() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request())
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
    let handle = workspace_handle("workspace-1", "lease-1");
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(capture_result(&handle, 2, "root-2")));
    let manager = manager_with(&fake);
    let mut handler = manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");
    handler.handle.id = WorkspaceSessionId("fabricated".to_owned());

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
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
}

#[test]
fn workspace_session_capture_updates_handler_snapshot_consistently() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle("workspace-1", "lease-1");
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(capture_result(&handle, 2, "root-2")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request())
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
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .expect("test operation succeeds");
    assert_eq!(resolved.handle.snapshot.manifest_version, 2);
    assert_eq!(resolved.handle.snapshot.root_hash, "root-2");
}

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
        })
    );
    assert_eq!(
        fake.create_requests(),
        vec![CreateWorkspaceRequest {
            network: NetworkProfile::Shared,
        }]
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
        })
    );
    assert_eq!(
        fake.create_requests(),
        vec![CreateWorkspaceRequest {
            network: NetworkProfile::Isolated,
        }]
    );
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
    let capture_session_changes =
        include_str!("../src/workspace_session/service/impls/capture_session_changes.rs");
    let create_workspace_session =
        include_str!("../src/workspace_session/service/impls/create_workspace_session.rs");
    let destroy_session = include_str!("../src/workspace_session/service/impls/destroy_session.rs");
    let resolve_session = include_str!("../src/workspace_session/service/impls/resolve_session.rs");
    let model = include_str!("../src/workspace_session/service/model.rs");
    let service = include_str!("../src/workspace_session/service.rs");
    let error = include_str!("../src/workspace_session/error.rs");

    for source in [
        core,
        capture_session_changes,
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

fn operations_with_fake(
    fake: &Arc<FakeWorkspaceService>,
) -> Result<SandboxRuntimeOperations, Box<dyn std::error::Error + Send + Sync>> {
    let workspace = Arc::new(manager_with(fake));
    let layerstack = layerstack_service()?;
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&workspace),
        Arc::clone(&layerstack),
        sandbox_runtime::command::CommandConfig::default(),
        Observer::disabled(),
    ));
    Ok(SandboxRuntimeOperations::new(
        command, workspace, layerstack,
    ))
}

fn runtime_request(op: &str, args: serde_json::Value) -> Request {
    Request::new(op, "req-test", CliOperationScope::system(), args)
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
        Observer::disabled(),
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
