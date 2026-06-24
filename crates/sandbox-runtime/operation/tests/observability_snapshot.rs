mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sandbox_runtime::command::ExecCommandInput;
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::{
    CommandOperationService, NamespaceExecutionLifecycle, NamespaceExecutionStore,
    SandboxRuntimeOperations,
};
use sandbox_runtime_workspace::{RemountWorkspaceRequest, RemountWorkspaceResult};
use sandbox_runtime_workspace::{WorkspaceProfile, WorkspaceSessionId};

use support::{
    build_services, create_request, workspace_handle, FakeWorkspaceService, TestServices,
};

#[test]
fn observability_snapshot_copies_active_workspace_fields(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let services = build_services(Arc::clone(&fake));
    let workspace_session_id = create_session(
        &fake,
        &services,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::Isolated,
    );
    let operations = operations_for(&services)?;

    let snapshot = operations.observability_snapshot();

    assert!(snapshot.partial_errors.is_empty());
    assert_eq!(snapshot.workspaces.len(), 1);
    let workspace = &snapshot.workspaces[0];
    assert_eq!(workspace.workspace_id, workspace_session_id);
    assert_eq!(workspace.remount_state, "active");
    assert_eq!(workspace.profile, WorkspaceProfile::Isolated);
    assert_eq!(
        workspace.workspace_root,
        PathBuf::from("/workspace/session")
    );
    assert!(workspace.upperdir.is_some());
    assert!(workspace.workdir.is_some());
    assert_eq!(workspace.namespace_fd_count, Some(4));
    assert_eq!(workspace.base_manifest_version, Some(1));
    assert_eq!(workspace.base_root_hash.as_deref(), Some("root"));
    assert_eq!(workspace.layer_count, Some(1));
    assert!(snapshot.active_namespace_executions.is_empty());
    assert!(snapshot.completed_namespace_executions.is_empty());
    Ok(())
}

#[test]
fn observability_snapshot_reports_active_command_namespace_execution(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let services = build_services(Arc::clone(&fake));
    let workspace_session_id = create_session(
        &fake,
        &services,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );
    let command_yield = services.command.exec_command(
        ExecCommandInput {
            workspace_session_id: Some(workspace_session_id.clone()),
            cmd: "printf ok".to_owned(),
            timeout_ms: None,
            yield_time_ms: Some(0),
        },
        None,
    )?;
    let command_session_id = command_yield
        .command_session_id
        .expect("running command has a command id");
    let operations = operations_for(&services)?;

    let snapshot = operations.observability_snapshot();

    let command_namespace_execution_id = services
        .command
        .namespace_execution_id_for_command_for_test(&command_session_id)
        .expect("active command keeps namespace execution id");
    assert_eq!(snapshot.active_namespace_executions.len(), 1);
    let namespace_execution = &snapshot.active_namespace_executions[0];
    assert_eq!(
        namespace_execution.namespace_execution_id,
        command_namespace_execution_id
    );
    assert_eq!(
        namespace_execution.workspace_session_id,
        workspace_session_id
    );
    assert_eq!(namespace_execution.operation_name, "exec_command");
    assert_eq!(
        namespace_execution.lifecycle_state,
        NamespaceExecutionLifecycle::Running
    );
    assert!(snapshot.completed_namespace_executions.is_empty());
    Ok(())
}

#[test]
fn observability_snapshot_keeps_workspace_remount_out_of_namespace_executions(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let services = build_services(Arc::clone(&fake));
    let workspace_session_id = create_session(
        &fake,
        &services,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );
    let mut remounted = workspace_handle(
        "workspace-session",
        "lease-2",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );
    remounted.snapshot.layer_paths = vec![PathBuf::from("/lower/remounted")];
    remounted.base_revision = remounted.snapshot.base_revision();
    fake.push_remount_result(Ok(RemountWorkspaceResult { handle: remounted }));
    let handler = services
        .workspace
        .begin_remount(workspace_session_id)
        .expect("begin remount succeeds");

    services
        .workspace
        .apply_and_finish_remount(
            &handler,
            RemountWorkspaceRequest {
                layer_paths: vec![PathBuf::from("/lower/remounted")],
            },
        )
        .expect("workspace remount succeeds");
    let operations = operations_for(&services)?;
    let snapshot = operations.observability_snapshot();

    assert!(snapshot.active_namespace_executions.is_empty());
    assert!(snapshot.completed_namespace_executions.is_empty());
    Ok(())
}

#[test]
fn runtime_observability_snapshot_keeps_observability_crate_out() {
    let manifest = include_str!("../Cargo.toml");
    assert!(!manifest.contains("sandbox-observability"));
    assert!(!manifest.contains("rusqlite"));
}

#[test]
#[should_panic(
    expected = "SandboxRuntimeOperations command service must use the same namespace_execution Arc"
)]
fn runtime_operations_enforce_shared_namespace_execution_store() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let services = build_services(Arc::clone(&fake));
    let _operations = SandboxRuntimeOperations::new_with_namespace_execution_store(
        Arc::<CommandOperationService>::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service().expect("layerstack service"),
        Arc::new(NamespaceExecutionStore::new()),
    );
}

fn create_session(
    fake: &Arc<FakeWorkspaceService>,
    services: &TestServices,
    workspace_session_id: &str,
    workspace_root: PathBuf,
    profile: WorkspaceProfile,
) -> WorkspaceSessionId {
    fake.push_create_result(Ok(workspace_handle(
        workspace_session_id,
        "lease-1",
        workspace_root,
        profile,
    )));
    services
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id
}

fn operations_for(
    services: &TestServices,
) -> Result<SandboxRuntimeOperations, Box<dyn std::error::Error + Send + Sync>> {
    Ok(SandboxRuntimeOperations::new(
        Arc::<CommandOperationService>::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service()?,
    ))
}

fn layerstack_service() -> Result<Arc<LayerStackService>, Box<dyn std::error::Error + Send + Sync>>
{
    let base = temp_root();
    let root = base.join("layer-stack");
    let workspace = base.join("workspace");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&workspace)?;
    sandbox_runtime_layerstack::build_workspace_base(&root, &workspace, false)?;
    Ok(Arc::new(LayerStackService::new(root)?))
}

fn temp_root() -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "sandbox-runtime-observability-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ))
}
