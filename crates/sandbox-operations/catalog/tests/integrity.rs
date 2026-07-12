#![cfg(all(feature = "manager", feature = "runtime", feature = "observability"))]

use std::collections::HashSet;

use sandbox_operation_catalog::{internal, manager, observability, routes, runtime};
use sandbox_operation_contract::{
    catalog_from_value, catalog_to_value, OperationExecutionOwner, OperationScopeKind,
    OperationScopePolicy, OperationVisibility,
};

#[test]
fn public_catalogs_are_route_complete() {
    let catalogs = [
        manager::manager_catalog(),
        runtime::runtime_catalog(),
        observability::observability_catalog(),
    ];
    let operation_count = catalogs
        .iter()
        .map(|catalog| catalog.operations.len())
        .sum::<usize>();

    for catalog in catalogs {
        let document = catalog_from_value(&catalog_to_value(catalog)).expect("valid catalog");
        let routed = document
            .routes
            .iter()
            .map(|route| route.operation.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(
            routed,
            document
                .operations
                .iter()
                .map(|operation| operation.name.as_str())
                .collect()
        );
    }

    assert_eq!(operation_count, 20);
    assert_eq!(routes::public_routes().count(), 21);
}

#[test]
fn public_route_manifest_is_exact_and_policy_consistent() {
    let manifest = routes::public_routes().collect::<Vec<_>>();
    let keys = manifest
        .iter()
        .map(|route| {
            (
                route.operation,
                route.scope_kind == OperationScopeKind::Sandbox,
            )
        })
        .collect::<HashSet<_>>();

    assert_eq!(keys.len(), manifest.len());
    assert_eq!(
        manifest
            .iter()
            .map(|route| (
                route.operation,
                route.scope_policy,
                route.scope_kind,
                route.execution_owner,
                route.visibility,
            ))
            .collect::<Vec<_>>(),
        [
            (
                "create_sandbox",
                OperationScopePolicy::System,
                OperationScopeKind::System,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public
            ),
            (
                "list_docker_images",
                OperationScopePolicy::System,
                OperationScopeKind::System,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public
            ),
            (
                "list_workspace_directories",
                OperationScopePolicy::System,
                OperationScopeKind::System,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public
            ),
            (
                "destroy_sandbox",
                OperationScopePolicy::System,
                OperationScopeKind::System,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public
            ),
            (
                "list_sandboxes",
                OperationScopePolicy::System,
                OperationScopeKind::System,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public
            ),
            (
                "inspect_sandbox",
                OperationScopePolicy::System,
                OperationScopeKind::System,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public
            ),
            (
                "squash_layerstacks",
                OperationScopePolicy::System,
                OperationScopeKind::System,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public
            ),
            (
                "export_changes",
                OperationScopePolicy::System,
                OperationScopeKind::System,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public
            ),
            (
                "exec_command",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Runtime,
                OperationVisibility::Public
            ),
            (
                "write_command_stdin",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Runtime,
                OperationVisibility::Public
            ),
            (
                "read_command_lines",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Runtime,
                OperationVisibility::Public
            ),
            (
                "file_read",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Runtime,
                OperationVisibility::Public
            ),
            (
                "file_write",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Runtime,
                OperationVisibility::Public
            ),
            (
                "file_edit",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Runtime,
                OperationVisibility::Public
            ),
            (
                "file_blame",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Runtime,
                OperationVisibility::Public
            ),
            (
                "snapshot",
                OperationScopePolicy::SystemOrSandbox,
                OperationScopeKind::System,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public,
            ),
            (
                "snapshot",
                OperationScopePolicy::SystemOrSandbox,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Observability,
                OperationVisibility::Public
            ),
            (
                "trace",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Observability,
                OperationVisibility::Public
            ),
            (
                "events",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Observability,
                OperationVisibility::Public
            ),
            (
                "cgroup",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Manager,
                OperationVisibility::Public
            ),
            (
                "layerstack",
                OperationScopePolicy::SandboxRequired,
                OperationScopeKind::Sandbox,
                OperationExecutionOwner::Observability,
                OperationVisibility::Public
            ),
        ]
    );
}

#[test]
fn internal_routes_never_leak_into_public_documents() {
    let mut public = HashSet::new();
    for catalog in [
        manager::manager_catalog(),
        runtime::runtime_catalog(),
        observability::observability_catalog(),
    ] {
        let document = catalog_from_value(&catalog_to_value(catalog)).expect("valid catalog");
        public.extend(
            document
                .operations
                .iter()
                .map(|operation| operation.name.clone()),
        );
        public.extend(document.routes.iter().map(|route| route.operation.clone()));
    }

    for internal in internal::runtime::ROUTES {
        assert!(!public.contains(internal.operation));
    }
    assert!(!public.contains(internal::runtime::FILE_LIST));
}

#[test]
fn internal_route_sets_are_exact() {
    assert_eq!(
        internal::runtime::ROUTES
            .iter()
            .map(|route| (
                route.operation,
                route.scope_policy,
                route.scope_kind,
                route.execution_owner,
                route.visibility,
            ))
            .collect::<Vec<_>>(),
        [
            internal_runtime_route("create_workspace_session"),
            internal_runtime_route("destroy_workspace_session"),
            internal_runtime_route("squash_layerstack"),
            internal_runtime_route("export_layerstack"),
            internal_runtime_route("read_export_chunk"),
        ]
    );
    assert_eq!(internal::runtime::FILE_LIST, "file_list");
}

fn internal_runtime_route(
    operation: &'static str,
) -> (
    &'static str,
    OperationScopePolicy,
    OperationScopeKind,
    OperationExecutionOwner,
    OperationVisibility,
) {
    (
        operation,
        OperationScopePolicy::SandboxRequired,
        OperationScopeKind::Sandbox,
        OperationExecutionOwner::Runtime,
        OperationVisibility::Internal,
    )
}
