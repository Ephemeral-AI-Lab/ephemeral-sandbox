#![cfg(feature = "observability")]

use sandbox_operation_catalog::observability::{
    observability_catalog, CGROUP_SPEC, DAEMON_SPEC, RESOURCES_SPEC, SNAPSHOT_SPEC, TOPOLOGY_SPEC,
};
use sandbox_operation_contract::{
    catalog_to_value, OperationDomain, OperationExecutionOwner, OperationScopeKind,
};

#[test]
fn observability_catalog_is_the_exact_public_set() {
    let catalog = observability_catalog();
    let names = catalog
        .operations
        .iter()
        .map(|operation| operation.name)
        .collect::<Vec<_>>();

    assert_eq!(
        catalog.operation_execution_space,
        OperationDomain::Observability
    );
    assert_eq!(
        catalog
            .families
            .iter()
            .map(|family| family.id)
            .collect::<Vec<_>>(),
        [
            "snapshot",
            "trace",
            "events",
            "resources",
            "daemon",
            "topology",
            "cgroup",
            "layerstack",
            "resource_isolation",
            "resource_efficiency"
        ]
    );
    assert_eq!(
        names,
        [
            "snapshot",
            "trace",
            "events",
            "resources",
            "daemon",
            "topology",
            "cgroup",
            "layerstack"
        ]
    );
    assert!(catalog
        .operations
        .iter()
        .zip([
            "snapshot",
            "trace",
            "events",
            "resources",
            "daemon",
            "topology",
            "cgroup",
            "layerstack",
        ])
        .all(|(operation, family)| operation.family == family));
    let serialized = catalog_to_value(catalog).to_string();
    assert!(!serialized.contains("sandbox-manager-cli observability"));
}

#[test]
fn snapshot_and_resources_are_the_only_aggregate_capable_operations() {
    let catalog = observability_catalog();
    assert!(std::ptr::eq(catalog.operations[0], &SNAPSHOT_SPEC));

    for operation in catalog.operations {
        let sandbox_id = operation
            .args
            .iter()
            .find(|argument| argument.name == "sandbox_id")
            .expect("observability sandbox selector");
        assert_eq!(
            sandbox_id.required,
            !matches!(operation.name, "snapshot" | "resources"),
            "only snapshot and resources support aggregate routing"
        );
    }
}

#[test]
fn resources_split_system_manager_and_sandbox_daemon_ownership() {
    let catalog = observability_catalog();
    assert!(std::ptr::eq(catalog.operations[3], &RESOURCES_SPEC));
    assert!(std::ptr::eq(catalog.operations[4], &DAEMON_SPEC));
    assert!(std::ptr::eq(catalog.operations[5], &TOPOLOGY_SPEC));

    let resources = sandbox_operation_catalog::routes::observability_routes()
        .iter()
        .filter(|route| route.operation == RESOURCES_SPEC.name)
        .collect::<Vec<_>>();
    assert_eq!(resources.len(), 2);
    assert!(resources.iter().any(|route| {
        route.scope_kind == OperationScopeKind::System
            && route.execution_owner == OperationExecutionOwner::Manager
    }));
    assert!(resources.iter().any(|route| {
        route.scope_kind == OperationScopeKind::Sandbox
            && route.execution_owner == OperationExecutionOwner::Observability
    }));

    let topology = sandbox_operation_catalog::routes::observability_routes()
        .iter()
        .filter(|route| route.operation == TOPOLOGY_SPEC.name)
        .collect::<Vec<_>>();
    assert_eq!(topology.len(), 1);
    assert_eq!(topology[0].scope_kind, OperationScopeKind::Sandbox);
    assert_eq!(
        topology[0].execution_owner,
        OperationExecutionOwner::Observability
    );

    let daemon = sandbox_operation_catalog::routes::observability_routes()
        .iter()
        .filter(|route| route.operation == DAEMON_SPEC.name)
        .collect::<Vec<_>>();
    assert_eq!(daemon.len(), 1);
    assert_eq!(daemon[0].scope_kind, OperationScopeKind::Sandbox);
    assert_eq!(
        daemon[0].execution_owner,
        OperationExecutionOwner::Observability
    );
}

#[test]
fn cgroup_catalog_retains_the_legacy_workspace_scope_schema() {
    assert_eq!(
        CGROUP_SPEC.summary,
        "Resource series for a scope (cpu/mem/io + disk)."
    );
    let scope = CGROUP_SPEC
        .args
        .iter()
        .find(|argument| argument.name == "scope")
        .expect("legacy cgroup scope selector");
    assert!(!scope.required);
    assert_eq!(scope.default, Some("sandbox"));
    assert!(scope.help.contains("workspace id"));
}
