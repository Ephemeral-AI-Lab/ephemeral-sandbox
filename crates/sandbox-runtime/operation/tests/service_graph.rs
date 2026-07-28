use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sandbox_observability_telemetry::Observer;
use sandbox_operation_contract::OperationDomain;
use sandbox_runtime::command::{CommandConfig, CommandOperationService, ExecCommandInput};
use sandbox_runtime::file::FileService;
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::workspace_session::WorkspaceSessionService;
use sandbox_runtime::{
    NamespaceExecutionRuntimeConfig, Rfc1918Egress, SandboxRuntimeConfig, SandboxRuntimeOperations,
    WorkspaceResourceCaps, WorkspaceRuntimeConfig,
};
use sandbox_runtime_workspace::{
    CaptureChangesRequest, CreateWorkspaceRequest, DestroyWorkspaceRequest, HolderFinalization,
    HolderProbe, WorkspaceError, WorkspaceHandle, WorkspaceRuntimeHooks, WorkspaceRuntimeService,
    WorkspaceSessionId,
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
    Arc::new(
        FileService::open(dir, sandbox_runtime::FileRuntimeConfig::default())
            .expect("create file auditability test service"),
    )
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
            take_holder_exit_subscription: Box::new(|| Ok(None)),
            isolated_ip: Box::new(|_| Ok(None)),
            holder_is_live: Box::new(WorkspaceHandle::holder_is_live),
            holder_probe: Box::new(|handle| {
                if handle.holder_is_live() {
                    HolderProbe::Running
                } else {
                    HolderProbe::Exited
                }
            }),
            holder_finalization: Box::new(|_| HolderFinalization::Exited),
            holder_exit_reason: Box::new(WorkspaceHandle::holder_exit_reason),
            allocate_workspace_session_id: Box::new(|_| {
                Ok(WorkspaceSessionId("not-configured".to_owned()))
            }),
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
            capture_changes_after_holder_quiesced: Box::new(|_handle, _proof, _request| {
                Err(WorkspaceError::Capture {
                    message: "not configured".to_owned(),
                })
            }),
            destroy_workspace: Box::new(
                |_handle: WorkspaceHandle, _request: DestroyWorkspaceRequest| {
                    Err(WorkspaceError::Setup {
                        step: "not configured".to_owned(),
                    })
                },
            ),
            commit_workspace_destroy: Box::new(|_| {}),
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
fn command_runtime_default_matches_the_shipped_admission_cap() {
    assert_eq!(
        sandbox_runtime::CommandRuntimeConfig::default().max_active,
        32
    );
    assert_eq!(CommandConfig::default().max_active, 32);
    assert_eq!(
        sandbox_runtime_namespace_execution::ExecutionCaps::default().max_active,
        32
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
                    freeze_budget_s: 0.5,
                },
            },
            namespace_execution: NamespaceExecutionRuntimeConfig {
                scratch_root: Some(command_scratch_root),
                caps: sandbox_runtime::NamespaceExecutionCaps::default(),
            },
            layerstack: sandbox_runtime::LayerstackRuntimeConfig::default(),
            command: sandbox_runtime::CommandRuntimeConfig::default(),
            file: sandbox_runtime::FileRuntimeConfig::default(),
            cgroup_root: None,
            workload_cgroup_limits: None,
            workload_cgroup_unavailable_reason: Some("test host has no delegation".to_owned()),
            mpla_storage_admin_profile: sandbox_runtime::StorageAdminCapabilityProfile::Production,
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
    let catalog = sandbox_operation_catalog::runtime::runtime_catalog();
    let names = catalog
        .operations
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();

    assert_eq!(catalog.operation_execution_space, OperationDomain::Runtime);
    assert_eq!(
        catalog
            .families
            .iter()
            .map(|family| family.id)
            .collect::<Vec<_>>(),
        [
            "command",
            "file",
            "daemon_http",
            "layerstack_baseline",
            "layerstack_phase1",
            "layerstack-phase1.portable-root",
            "network_isolation",
            "reserved_paths",
            "shell_security",
            "workspace_session",
        ]
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
            "create_workspace_session",
            "create_mpla_workspace_session",
            "publish_workspace_session",
            "destroy_workspace_session",
            "mpla_storage_admin",
        ]
    );
}

#[test]
fn service_graph_catalog_contains_the_public_operation_families() {
    let catalog = sandbox_operation_catalog::runtime::runtime_catalog();
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

    assert_eq!(
        used_families,
        BTreeSet::from(["command", "file", "workspace_session"])
    );
    assert!(used_families.is_subset(&families));
    assert!(catalog
        .operations
        .iter()
        .all(|spec| families.contains(spec.family)));
}

#[test]
fn service_graph_catalog_keeps_internal_helpers_out() {
    let catalog = sandbox_operation_catalog::runtime::runtime_catalog();
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
        "create_workspace_session_legacy_scratch_adapter",
    ] {
        assert!(!names.contains(&helper), "{helper} leaked into catalog");
    }
}

#[test]
fn service_graph_workspace_session_source_boundaries_stay_private() {
    let workspace_session_sources = rust_sources("src/workspace_session");
    for (path, source) in workspace_session_sources {
        for forbidden in [
            "sandbox_operation_contract::OperationRequest",
            "sandbox_operation_contract::OperationResponse",
            "OperationSpec",
            "OperationEntry",
            "CommandOperationService",
            "crate::operations",
        ] {
            assert!(
                !source.contains(forbidden),
                "{forbidden} leaked into {}",
                path.display()
            );
        }
    }

    let adapter = include_str!("../src/operations/registry/workspace_session_operations.rs");
    assert!(adapter.contains(".create_workspace_session("));
    assert!(adapter.contains(".create_mpla_workspace_session("));
    assert!(adapter.contains(".guarded_destroy("));
    assert!(adapter.contains("dispatch_mpla_storage_admin"));
    assert!(adapter.contains("binding.storage_admin_profile != selected_profile"));
    assert!(adapter.contains("require_daemon_selected_storage_admin_profile"));
    assert_eq!(adapter.matches("OperationEntry::public").count(), 5);
    assert!(adapter.contains("name: CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER"));
    assert_eq!(adapter.matches("spec: None").count(), 1);
    assert!(!adapter.contains("WorkspaceDestroyAdmission"));
    assert!(!adapter.contains("begin_workspace_destroy_admission"));

    let file_adapter = include_str!("../src/operations/registry/file_operations.rs");
    assert!(file_adapter.contains("const FILE_LIST_ENTRY: OperationEntry = OperationEntry {"));
    assert!(file_adapter.contains("name: FILE_LIST,"));
    assert_eq!(file_adapter.matches("spec: None").count(), 1);

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
