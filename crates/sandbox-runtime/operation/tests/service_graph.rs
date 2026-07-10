use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sandbox_observability::Observer;
use sandbox_protocol::CliOperationExecutionSpace;
use sandbox_runtime::command::{CommandConfig, CommandOperationService, ExecCommandInput};
use sandbox_runtime::file::FileService;
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::workspace_session::WorkspaceSessionService;
use sandbox_runtime::{
    NamespaceExecutionRuntimeConfig, Rfc1918Egress, SandboxRuntimeConfig, SandboxRuntimeOperations,
    WorkspaceResourceCaps, WorkspaceRuntimeConfig,
};
use sandbox_runtime_workspace::{
    CaptureChangesRequest, CreateWorkspaceRequest, DestroyWorkspaceRequest, WorkspaceError,
    WorkspaceHandle, WorkspaceRuntimeHooks, WorkspaceRuntimeService, WorkspaceSessionId,
};

fn workspace_session(layerstack: &Arc<LayerStackService>) -> Arc<WorkspaceSessionService> {
    Arc::new(WorkspaceSessionService::new(
        noop_workspace_runtime(),
        Arc::clone(layerstack),
        Observer::disabled(),
    ))
}

fn layerstack_service() -> Result<Arc<LayerStackService>, Box<dyn std::error::Error + Send + Sync>>
{
    let base = temp_root("service-graph-layerstack");
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
        file_service(),
    )?))
}

fn file_service() -> Arc<FileService> {
    let dir = temp_root("file-auditability");
    let _ = std::fs::remove_dir_all(&dir);
    Arc::new(FileService::open(dir).expect("create file auditability test service"))
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
            destroy_workspace: Box::new(
                |_handle: WorkspaceHandle, _request: DestroyWorkspaceRequest| {
                    Err(WorkspaceError::Setup {
                        step: "not configured".to_owned(),
                    })
                },
            ),
            run_file_op: Box::new(|_handle, _op| {
                Err(WorkspaceError::Setup {
                    step: "not configured".to_owned(),
                })
            }),
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
    let layerstack = layerstack_service()?;
    let workspace = workspace_session(&layerstack);
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&workspace),
        CommandConfig::default(),
        Observer::disabled(),
    ));
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&command),
        Arc::clone(&workspace),
        Arc::clone(&layerstack),
        file_service(),
    );

    assert!(Arc::ptr_eq(&operations.command, &command));
    assert!(Arc::ptr_eq(&operations.workspace_session, &workspace));
    assert!(Arc::ptr_eq(&operations.layerstack, &layerstack));
    Ok(())
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
fn runtime_from_config_initializes_layerstack_workspace_base(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let base = temp_root("runtime-from-config-layerstack");
    let layer_stack_root = base.join("layer-stack");
    let workspace_root = base.join("workspace");
    let scratch_root = base.join("scratch");
    let command_scratch_root = base.join("commands");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&workspace_root)?;

    let _operations = SandboxRuntimeOperations::from_config(
        SandboxRuntimeConfig {
            workspace: WorkspaceRuntimeConfig {
                workspace_root: workspace_root.clone(),
                layer_stack_root: layer_stack_root.clone(),
                scratch_root,
                caps: WorkspaceResourceCaps {
                    setup_timeout_s: 1.0,
                    exit_grace_s: 0.1,
                    rfc1918_egress: Rfc1918Egress::Allow,
                },
            },
            namespace_execution: NamespaceExecutionRuntimeConfig {
                scratch_root: command_scratch_root,
            },
            layerstack: sandbox_runtime::LayerstackRuntimeConfig::default(),
            cgroup_root: None,
        },
        Observer::disabled(),
    );

    assert!(layer_stack_root.join("workspace.json").is_file());
    let binding = sandbox_runtime_layerstack::require_workspace_binding(&layer_stack_root)?;
    assert_eq!(binding.workspace_root, workspace_root.to_string_lossy());
    Ok(())
}

#[test]
fn runtime_operation_catalog_exports_only_public_runtime_operations() {
    let catalog = sandbox_runtime_operations::runtime_catalog();
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
        ["command", "file"]
    );
    assert_eq!(
        names,
        [
            "exec_command",
            "write_command_stdin",
            "read_command_lines",
            "file_read",
            "file_write",
            "file_edit",
            "file_blame",
        ]
    );
    assert!(catalog.operations.iter().all(|spec| spec.cli.is_some()));
}

#[test]
fn service_graph_cli_catalog_families_match_cli_operations() {
    let catalog = sandbox_runtime_operations::runtime_catalog();
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
    let catalog = sandbox_runtime_operations::runtime_catalog();
    let names = catalog
        .operations
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    for helper in [
        "resolve_session",
        "admit_command",
        "with_gated_session",
        "guarded_destroy",
        "finalize_session",
        "publish_changes",
        "process_store",
        "transcript",
        "status_lookup",
        "finalize_command",
        "file_list",
        "create_workspace_session",
        "destroy_workspace_session",
    ] {
        assert!(!names.contains(&helper), "{helper} leaked into catalog");
    }
}

#[test]
fn hidden_http_and_lifecycle_operations_remain_registered_for_daemon_dispatch() {
    assert_eq!(
        sandbox_runtime::known_operation_name("exec_command"),
        Some("exec_command")
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
        sandbox_runtime::known_operation_name("file_list"),
        Some("file_list")
    );
    assert_eq!(
        sandbox_runtime::known_operation_name("RAW_UNKNOWN_OPERATION_SECRET"),
        None
    );
}

#[test]
fn cli_operation_catalog_metadata_uses_runtime_space() {
    let catalog = sandbox_runtime_operations::runtime_catalog();

    for spec in catalog.operations {
        let cli = spec.cli.expect("runtime catalog spec must be CLI-visible");
        assert_eq!(cli.path.first(), Some(&"runtime"));
        assert!(cli
            .usage
            .starts_with("sandbox-runtime-cli --sandbox-id ID "));
        assert!(cli.examples.iter().all(|example| {
            example.starts_with("sandbox-runtime-cli --sandbox-id ID ")
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

    let adapter = include_str!("../src/operation_adapter/workspace_session_operations.rs");
    assert!(adapter.contains(".create_workspace_session("));
    assert!(adapter.contains(".guarded_destroy("));
    assert_eq!(adapter.matches("cli: None").count(), 2);
    assert!(!adapter.contains("WorkspaceDestroyAdmission"));
    assert!(!adapter.contains("begin_workspace_destroy_admission"));

    let file_adapter = include_str!("../src/operation_adapter/file_operations.rs");
    assert!(file_adapter.contains("const FILE_LIST: OperationEntry = OperationEntry {"));
    assert_eq!(file_adapter.matches("cli: None").count(), 1);

    for (path, source) in rust_sources("src/command") {
        assert!(
            !source.contains("fn workspace(&self)"),
            "generic workspace accessor leaked into {}",
            path.display()
        );
    }

    let services = include_str!("../src/services.rs");
    assert!(services.contains("pub workspace_session: Arc<WorkspaceSessionService>"));
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
