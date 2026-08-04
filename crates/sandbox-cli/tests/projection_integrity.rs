#![cfg(all(feature = "manager", feature = "runtime", feature = "observability"))]

use std::collections::HashSet;

use sandbox_cli::projection::document::catalog_document;
use sandbox_cli::projection::CatalogProjection;
use sandbox_operation_catalog::{manager, observability, runtime};
use sandbox_operation_contract::{OperationCatalog, OperationDomain, OperationVisibility};

#[test]
fn cli_projection_is_bidirectional_with_public_routes() {
    assert_projection_integrity(
        manager::manager_catalog(),
        sandbox_cli::projection::manager::catalog_projection(),
    );
    assert_projection_integrity(
        runtime::runtime_catalog(),
        sandbox_cli::projection::runtime::catalog_projection(),
    );
    assert_projection_integrity(
        observability::observability_catalog(),
        sandbox_cli::projection::observability::catalog_projection(),
    );
}

#[test]
fn missing_public_operation_projection_is_rejected() {
    let error = catalog_document(
        runtime::runtime_catalog(),
        CatalogProjection {
            operation_execution_space: OperationDomain::Runtime,
            operations: &[],
        },
    )
    .expect_err("missing public projections must fail");

    assert_eq!(
        error.message(),
        "semantic catalog and CLI projection operation counts differ"
    );
}

fn assert_projection_integrity(catalog: OperationCatalog, projection: CatalogProjection) {
    let document = catalog_document(catalog, projection).expect("valid CLI projection");
    let routed_operations = document
        .semantic
        .routes
        .iter()
        .map(|route| route.operation.as_str())
        .collect::<HashSet<_>>();

    for projected in document.projection.operations {
        assert!(document.semantic.routes.iter().any(|route| {
            route.operation == projected.name && route.visibility == OperationVisibility::Public
        }));
        assert_unique_argument_bindings(projected);
    }

    for routed in routed_operations {
        assert_eq!(
            document
                .projection
                .operations
                .iter()
                .filter(|projected| projected.name == routed)
                .count(),
            1,
            "public operation must have exactly one CLI projection: {routed}"
        );
    }

    for internal in [
        "squash_layerstack",
        "export_layerstack",
        "read_export_chunk",
        "file_list",
    ] {
        assert!(document
            .projection
            .operations
            .iter()
            .all(|projected| projected.name != internal));
    }
}

fn assert_unique_argument_bindings(operation: &sandbox_cli::projection::OperationProjection) {
    let mut flags = HashSet::new();
    let mut positionals = HashSet::new();

    for argument in operation.arguments {
        if let Some(flag) = argument.flag {
            assert!(
                flags.insert(flag),
                "duplicate flag in {}: {flag}",
                operation.name
            );
        }
        for flag in argument.additional_flags {
            assert!(
                flags.insert(flag),
                "duplicate flag in {}: {flag}",
                operation.name
            );
        }
        if let Some(flag) = argument.value_file_flag {
            assert!(
                flags.insert(flag),
                "duplicate flag in {}: {flag}",
                operation.name
            );
        }
        if let Some(positional) = argument.positional {
            assert!(
                positionals.insert(positional),
                "duplicate positional in {}: {positional}",
                operation.name
            );
        }
    }
}
