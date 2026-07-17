#![cfg(feature = "observability")]

use sandbox_operation_catalog::observability::{observability_catalog, CGROUP_SPEC, SNAPSHOT_SPEC};
use sandbox_operation_contract::{catalog_to_value, OperationDomain};

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
        catalog.families.iter().map(|family| family.id).collect::<Vec<_>>(),
        ["snapshot", "trace", "events", "cgroup", "layerstack"]
    );
    assert_eq!(
        names,
        ["snapshot", "trace", "events", "cgroup", "layerstack"]
    );
    assert!(catalog
        .operations
        .iter()
        .zip(["snapshot", "trace", "events", "cgroup", "layerstack"])
        .all(|(operation, family)| operation.family == family));
    let serialized = catalog_to_value(catalog).to_string();
    assert!(!serialized.contains("sandbox-manager-cli observability"));
}

#[test]
fn snapshot_is_canonical_and_only_aggregate_capable_operation() {
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
            operation.name != "snapshot",
            "only snapshot supports aggregate routing"
        );
    }
}

#[test]
fn cgroup_catalog_describes_explicit_topology_composition() {
    assert!(CGROUP_SPEC.summary.contains("topology"));
    assert!(CGROUP_SPEC.description.contains("daemon"));
    assert!(CGROUP_SPEC.description.contains("manager"));
    assert!(!CGROUP_SPEC.description.contains("never contacts"));
}
