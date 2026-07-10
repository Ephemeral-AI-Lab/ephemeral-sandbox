mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sandbox_observability::Observer;
use sandbox_runtime::command::ExecCommandInput;
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::{CommandOperationService, SandboxRuntimeOperations};
use sandbox_runtime_workspace::{NetworkProfile, WorkspaceSessionId};

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
        NetworkProfile::Isolated,
    );
    let operations = operations_for(&services)?;

    let snapshot = operations.observability_snapshot();

    assert!(snapshot.partial_errors.is_empty());
    assert_eq!(snapshot.workspaces.len(), 1);
    let workspace = &snapshot.workspaces[0];
    assert_eq!(workspace.workspace_id, workspace_session_id);
    assert_eq!(workspace.network, NetworkProfile::Isolated);
    assert_eq!(
        workspace.workspace_root,
        PathBuf::from("/workspace/session")
    );
    assert!(workspace.upperdir.is_some());
    assert!(workspace.workdir.is_some());
    assert_eq!(workspace.namespace_fd_count, Some(4));
    assert_eq!(workspace.base_root_hash.as_deref(), Some("root"));
    assert_eq!(workspace.layer_count, Some(1));
    assert!(snapshot.active_namespace_executions.is_empty());
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
        NetworkProfile::Shared,
    );
    let command_yield = services.command.exec_command(ExecCommandInput {
        workspace_session_id: Some(workspace_session_id.clone()),
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(0),
    })?;
    let command_session_id = command_yield
        .command_session_id
        .expect("running command has a command id");
    let operations = operations_for(&services)?;

    let snapshot = operations.observability_snapshot();

    assert_eq!(snapshot.active_namespace_executions.len(), 1);
    let namespace_execution = &snapshot.active_namespace_executions[0];
    assert_eq!(
        namespace_execution.namespace_execution_id,
        command_session_id
    );
    assert_eq!(
        namespace_execution.workspace_session_id,
        workspace_session_id
    );
    assert_eq!(namespace_execution.operation_name, "exec_command");
    Ok(())
}

/// The runtime now depends on the `sandbox-observability` leaf (it carries the
/// span/event emit seams), so the old "operation excludes sandbox-observability"
/// assertion is intentionally gone. What must still hold is that the runtime
/// never pulls a storage engine: SQLite stays out. The leaf-boundary
/// invariant (obs must not depend on runtime/daemon/manager) is owned by the obs
/// crate's own `dependency_guard.rs`.
#[test]
fn runtime_never_pulls_sqlite_storage() {
    let manifest = include_str!("../Cargo.toml");
    assert!(!manifest.contains(concat!("rusq", "lite")));
}

fn create_session(
    fake: &Arc<FakeWorkspaceService>,
    services: &TestServices,
    workspace_session_id: &str,
    workspace_root: PathBuf,
    network: NetworkProfile,
) -> WorkspaceSessionId {
    fake.push_create_result(Ok(workspace_handle(
        workspace_session_id,
        "lease-1",
        workspace_root,
        network,
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
        support::test_file_service(),
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
        "sandbox-runtime-observability-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ))
}
