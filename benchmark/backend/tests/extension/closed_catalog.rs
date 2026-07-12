use std::collections::BTreeSet;
use std::path::Path;

use sandbox_benchmark::config::BenchmarkPaths;
use sandbox_benchmark::definitions::{definition, FactorValueKind, DEFINITION_SCHEMA_VERSION};
use sandbox_benchmark::model::{
    CleanupPolicy, CountSemantics, FactorRole, FamilyId, IsolationPolicy, NormalizedFactorValue,
    OperationId, ProductAccess,
};
use sandbox_benchmark::plan::{load_plan, validate_and_expand};
use serde::Deserialize;

use crate::support::{create_fake_repository, TestRoot};

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosedCatalog {
    schema_version: u32,
    operations: Vec<ClosedOperation>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosedOperation {
    id: OperationId,
    family: FamilyId,
    product_access: ProductAccess,
    isolation: IsolationPolicy,
    cleanup: CleanupPolicy,
    count_semantics: CountSemantics,
    semantic_revision: u32,
    factor_schema_revision: u32,
    comparison_projection_revision: u32,
}

fn parse_current_catalog(input: &str) -> Result<ClosedCatalog, String> {
    let catalog: ClosedCatalog = serde_json::from_str(input).map_err(|error| error.to_string())?;
    if catalog.schema_version != DEFINITION_SCHEMA_VERSION {
        return Err(format!(
            "unsupported definition fixture schema {}",
            catalog.schema_version
        ));
    }
    Ok(catalog)
}

#[test]
fn golden_catalog_registers_every_closed_operation_once_with_exact_semantics() {
    let expected = parse_current_catalog(include_str!(
        "../fixtures/definitions/closed-catalog-v2.json"
    ))
    .expect("current closed catalog fixture parses");
    let actual = ClosedCatalog {
        schema_version: DEFINITION_SCHEMA_VERSION,
        operations: OperationId::ALL
            .into_iter()
            .map(|id| {
                let definition = definition(id);
                ClosedOperation {
                    id: definition.id,
                    family: definition.family,
                    product_access: definition.product_access,
                    isolation: definition.isolation,
                    cleanup: definition.cleanup,
                    count_semantics: definition.count_semantics,
                    semantic_revision: definition.semantic_revision,
                    factor_schema_revision: definition.factor_schema_revision,
                    comparison_projection_revision: definition.comparison.semantic_revision,
                }
            })
            .collect(),
    };

    assert_eq!(actual, expected);
    assert_eq!(actual.operations.len(), OperationId::ALL.len());
    assert_eq!(
        actual
            .operations
            .iter()
            .map(|operation| operation.id)
            .collect::<BTreeSet<_>>(),
        OperationId::ALL.into_iter().collect()
    );
    assert!(actual.operations.iter().all(|operation| {
        operation.semantic_revision > 0
            && operation.factor_schema_revision > 0
            && operation.comparison_projection_revision > 0
    }));
}

#[test]
fn definition_fixture_rejects_unknown_fields_and_future_versions() {
    let current = include_str!("../fixtures/definitions/closed-catalog-v2.json");
    let mut unknown: serde_json::Value = serde_json::from_str(current).expect("fixture JSON");
    unknown["operations"][0]["executor"] = serde_json::json!("runtime-plugin");
    assert!(parse_current_catalog(&unknown.to_string()).is_err());

    let mut future: serde_json::Value = serde_json::from_str(current).expect("fixture JSON");
    future["schema_version"] = serde_json::json!(DEFINITION_SCHEMA_VERSION + 1);
    assert!(parse_current_catalog(&future.to_string()).is_err());
}

#[test]
fn every_typed_cell_projects_complete_normalized_factors_without_report_dispatch() {
    let plan =
        load_plan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../defaults/standard-local.yml"))
            .expect("load standard default");
    let root = TestRoot::new("normalized-factor-projection");
    let repository = root.join("repository");
    create_fake_repository(&repository);
    let paths = BenchmarkPaths::initialize(&root.join("workspace"), &repository)
        .expect("initialize isolated paths");
    let expanded = validate_and_expand(&plan, &paths, None).expect("expand standard default");
    assert!(expanded.runnable);

    let mut projected_operations = BTreeSet::new();
    for cell in &expanded.cells {
        let operation_plan = expanded
            .canonical_plan
            .operations
            .iter()
            .find(|operation| operation.id() == cell.operation_id)
            .expect("expanded cell has canonical typed plan");
        let projection = cell
            .operation
            .normalized_factor_projection(operation_plan)
            .expect("typed plan and cell project together");
        let metadata = definition(cell.operation_id).factors;

        assert_eq!(
            projection
                .iter()
                .map(|factor| factor.id)
                .collect::<Vec<_>>(),
            metadata.iter().map(|factor| factor.id).collect::<Vec<_>>()
        );
        for (factor, definition) in projection.iter().zip(metadata) {
            assert_eq!(factor.role == FactorRole::Varied, factor.control.is_some());
            assert!(matches!(
                (&factor.value, definition.value_kind),
                (
                    NormalizedFactorValue::UnsignedInteger(_),
                    FactorValueKind::UnsignedInteger
                ) | (NormalizedFactorValue::Ratio(_), FactorValueKind::UnitRatio)
                    | (NormalizedFactorValue::Choice(_), FactorValueKind::Choice)
            ));
        }
        projected_operations.insert(cell.operation_id);
    }

    assert_eq!(projected_operations, OperationId::ALL.into_iter().collect());
}
