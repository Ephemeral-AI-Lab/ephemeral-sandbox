use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sandbox_protocol::{CliOperationExecutionSpace, CliOperationScope, Request};
use sandbox_runtime::command::{CommandOperationService, ExecCommandInput};
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::workspace_remount::{
    CommandRemountCoordinator, RemountWorkspaceSession, WorkspaceRemountService,
};
use sandbox_runtime::workspace_session::WorkspaceSessionService;
use sandbox_runtime::SandboxRuntimeOperations;
use sandbox_runtime_workspace::{
    CaptureChangesRequest, CreateWorkspaceRequest, DestroyWorkspaceRequest,
    RemountWorkspaceRequest, WorkspaceError, WorkspaceHandle, WorkspaceRuntimeHooks,
    WorkspaceRuntimeService, WorkspaceSessionId,
};
use serde_json::json;

fn workspace_session() -> Arc<WorkspaceSessionService> {
    Arc::new(WorkspaceSessionService::new(noop_workspace_runtime()))
}

fn layerstack_service() -> Result<Arc<LayerStackService>, Box<dyn std::error::Error + Send + Sync>>
{
    let base = temp_root("service-graph-layerstack");
    let root = base.join("layer-stack");
    let workspace = base.join("workspace");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&workspace)?;
    sandbox_runtime_layerstack::build_workspace_base(&root, &workspace, false)?;
    Ok(Arc::new(LayerStackService::new(root)?))
}

fn temp_root(label: &str) -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "sandbox-runtime-{label}-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ))
}

fn noop_workspace_runtime() -> Arc<WorkspaceRuntimeService> {
    Arc::new(WorkspaceRuntimeService::from_hooks_for_test(
        WorkspaceRuntimeHooks {
            create_workspace: Box::new(|_request: CreateWorkspaceRequest| {
                Err(WorkspaceError::Setup {
                    step: "not configured".to_owned(),
                })
            }),
            capture_changes: Box::new(
                |_handle: &WorkspaceHandle, _request: CaptureChangesRequest| {
                    Err(WorkspaceError::Capture {
                        message: "not configured".to_owned(),
                    })
                },
            ),
            remount_workspace: Box::new(
                |_handle: &WorkspaceHandle, _request: RemountWorkspaceRequest| {
                    Err(WorkspaceError::Setup {
                        step: "not configured".to_owned(),
                    })
                },
            ),
            destroy_workspace: Box::new(
                |_handle: WorkspaceHandle, _request: DestroyWorkspaceRequest| {
                    Err(WorkspaceError::Setup {
                        step: "not configured".to_owned(),
                    })
                },
            ),
            latest_snapshot: Box::new(|| {
                Err(WorkspaceError::SnapshotAcquire {
                    source: "not configured".to_owned(),
                })
            }),
        },
    ))
}

#[test]
fn service_graph_runtime_operations_exposes_command_lane(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = workspace_session();
    let layerstack = layerstack_service()?;
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&workspace),
        sandbox_runtime_command::CommandConfig::default(),
    ));
    let remount_workspace: Arc<dyn RemountWorkspaceSession> = workspace.clone();
    let remount_command: Arc<dyn CommandRemountCoordinator> = command.clone();
    let remount = Arc::new(WorkspaceRemountService::new(
        remount_workspace,
        remount_command,
    ));

    let _ = remount;
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&command),
        Arc::clone(&workspace),
        Arc::clone(&layerstack),
    );

    assert!(Arc::ptr_eq(&operations.command, &command));
    assert!(Arc::ptr_eq(&operations.workspace_session, &workspace));
    assert!(Arc::ptr_eq(&operations.layerstack, &layerstack));
    Ok(())
}

#[test]
#[should_panic(
    expected = "SandboxRuntimeOperations command service must use the same workspace_session Arc"
)]
fn service_graph_runtime_operations_rejects_mismatched_workspace_session_arc() {
    let command_workspace = workspace_session();
    let aggregate_workspace = workspace_session();
    let command = Arc::new(CommandOperationService::new(
        command_workspace,
        sandbox_runtime_command::CommandConfig::default(),
    ));
    let layerstack = layerstack_service().expect("layerstack service builds");

    let _operations = SandboxRuntimeOperations::new(command, aggregate_workspace, layerstack);
}

#[test]
fn command_contract_keeps_session_selector_in_exec_input() {
    let input = ExecCommandInput {
        workspace_session_id: Some(WorkspaceSessionId("workspace-1".to_owned())),
        cmd: "pwd".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(100),
    };

    assert_eq!(
        input.workspace_session_id,
        Some(WorkspaceSessionId("workspace-1".to_owned()))
    );
}

#[test]
fn service_graph_cli_operation_catalog_exports_runtime_cli_operations() {
    let catalog = sandbox_runtime::cli_operation_catalog();
    let names = catalog
        .operations
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    assert_eq!(
        catalog.operation_execution_space,
        CliOperationExecutionSpace::Runtime
    );
    assert_eq!(
        catalog
            .families
            .iter()
            .map(|family| family.id)
            .collect::<Vec<_>>(),
        ["command", "workspace_session", "layerstack"]
    );
    assert_eq!(
        names,
        [
            "exec_command",
            "write_command_stdin",
            "read_command_lines",
            "create_workspace_session",
            "destroy_workspace_session",
            "squash"
        ]
    );
    assert!(catalog.operations.iter().all(|spec| spec.cli.is_some()));
}

#[test]
fn service_graph_cli_catalog_families_match_cli_operations() {
    let catalog = sandbox_runtime::cli_operation_catalog();
    let families = catalog
        .families
        .iter()
        .map(|family| family.id)
        .collect::<BTreeSet<_>>();
    let used_families = catalog
        .operations
        .iter()
        .map(|spec| spec.family)
        .collect::<BTreeSet<_>>();

    assert_eq!(families, used_families);
    assert!(catalog
        .operations
        .iter()
        .all(|spec| families.contains(spec.family)));
}

#[test]
fn service_graph_cli_catalog_keeps_non_cli_helpers_out() {
    let catalog = sandbox_runtime::cli_operation_catalog();
    let names = catalog
        .operations
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    for helper in [
        "resolve_session",
        "begin_remount",
        "apply_and_finish_remount",
        "block_remount",
        "refresh_after_publish",
        "capture_session_changes",
        "with_workspace_destroy_admission",
        "publish_changes",
        "process_store",
        "transcript",
        "status_lookup",
        "finalize_command",
    ] {
        assert!(!names.contains(&helper), "{helper} leaked into catalog");
    }
}

#[test]
fn runtime_known_operation_name_uses_registered_operation_entries() {
    assert_eq!(
        sandbox_runtime::known_operation_name("exec_command"),
        Some("exec_command")
    );
    assert_eq!(
        sandbox_runtime::known_operation_name("squash"),
        Some("squash")
    );
    assert_eq!(
        sandbox_runtime::known_operation_name("create_workspace_session"),
        Some("create_workspace_session")
    );
    assert_eq!(
        sandbox_runtime::known_operation_name("destroy_workspace_session"),
        Some("destroy_workspace_session")
    );
    assert_eq!(
        sandbox_runtime::known_operation_name("RAW_UNKNOWN_OPERATION_SECRET"),
        None
    );
}

#[test]
fn cli_operation_catalog_metadata_uses_runtime_space() {
    let catalog = sandbox_runtime::cli_operation_catalog();

    for spec in catalog.operations {
        let cli = spec.cli.expect("runtime catalog spec must be CLI-visible");
        assert_eq!(cli.path.first(), Some(&"runtime"));
        assert!(cli.usage.starts_with("sandbox-cli runtime "));
        assert!(!cli.usage.contains("--sandbox-id"));
        assert!(cli.examples.iter().all(|example| {
            example.starts_with("sandbox-cli runtime ")
                && !example.contains("--sandbox-id")
                && !example.contains("daemon")
        }));
    }
}

#[test]
fn service_graph_workspace_session_source_boundaries_stay_private() {
    let workspace_session_sources = rust_sources("src/workspace_session");
    for (path, source) in workspace_session_sources {
        for forbidden in [
            "sandbox_protocol::Request",
            "sandbox_protocol::Response",
            "CliOperationSpec",
            "OperationEntry",
            "CommandOperationService",
            "crate::operation",
        ] {
            assert!(
                !source.contains(forbidden),
                "{forbidden} leaked into {}",
                path.display()
            );
        }
    }

    let adapter = include_str!("../src/workspace_session_operations.rs");
    assert!(adapter.contains("operations.workspace_session"));
    assert!(!adapter.contains("WorkspaceDestroyAdmission"));
    assert!(!adapter.contains("begin_workspace_destroy_admission"));

    for (path, source) in rust_sources("src/command") {
        assert!(
            !source.contains("fn workspace(&self)"),
            "generic workspace accessor leaked into {}",
            path.display()
        );
    }

    let services = include_str!("../src/services.rs");
    assert!(services.contains("pub workspace_session: Arc<WorkspaceSessionService>"));
    assert!(!services.contains("pub workspace_remount:"));
}

#[test]
fn squash_dispatch_projects_stable_no_op_json(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = workspace_session();
    let operations = SandboxRuntimeOperations::new(
        Arc::new(CommandOperationService::new(
            Arc::clone(&workspace),
            sandbox_runtime_command::CommandConfig::default(),
        )),
        workspace,
        layerstack_service()?,
    );

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "squash",
            "req-squash",
            CliOperationScope::system(),
            json!({}),
        ),
        None,
    )
    .into_json_value();

    assert_eq!(response["squashed"], false);
    assert!(response["revision"].is_null(), "{response}");
    assert_eq!(response["layer_paths"], json!([]));
    assert!(response.get("lease_release_error").is_some(), "{response}");
    Ok(())
}

fn rust_sources(relative_root: &str) -> Vec<(PathBuf, String)> {
    let mut pending = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_root)];
    let mut sources = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).expect("source directory is readable") {
            let entry = entry.expect("source entry is readable");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                let source = std::fs::read_to_string(&path).expect("source file is readable");
                sources.push((path, source));
            }
        }
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    sources
}
