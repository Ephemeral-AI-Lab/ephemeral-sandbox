use std::fs;
use std::path::Path;

use sandbox_benchmark::config::BenchmarkPaths;
use sandbox_benchmark::model::{
    ConfigurationScope, ExpandedOperationCell, OperationComparisonIdentity, OperationEvidence,
    OperationId, OperationPlan, ProductAccess, WorkspaceAction,
};
use sandbox_benchmark::plan::{load_plan, slice_default, validate_and_expand};
use serde_json::json;

use crate::support::{create_fake_repository, TestRoot};

#[test]
fn future_plan_schema_is_explicitly_non_runnable() {
    let default_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../defaults/standard-local.yml");
    let default = load_plan(&default_path).expect("load standard default");
    let mut future = slice_default(&default, ConfigurationScope::Command);
    future.schema_version += 1;

    let root = TestRoot::new("future-plan-schema");
    let repository = root.join("repository");
    create_fake_repository(&repository);
    let paths = BenchmarkPaths::initialize(&root.join("workspace"), &repository)
        .expect("initialize isolated paths");
    let expanded = validate_and_expand(&future, &paths, None).expect("return validation response");

    assert!(!expanded.runnable);
    assert!(expanded.validation.iter().any(|finding| {
        finding.code == "unsupported_plan_schema"
            && finding.path.as_deref() == Some("schema_version")
    }));
}

#[test]
fn current_plan_schema_rejects_unknown_fields_during_loading() {
    let default_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../defaults/standard-local.yml");
    let plan = load_plan(&default_path).expect("load standard default");
    let mut value = serde_json::to_value(plan).expect("serialize plan");
    value["executor"] = json!("runtime-plugin");

    let root = TestRoot::new("unknown-plan-field");
    let path = root.join("invalid.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&value).expect("encode invalid plan"),
    )
    .expect("write invalid plan");
    assert!(load_plan(&path).is_err());
}

#[test]
fn future_closed_variants_are_rejected_at_every_typed_boundary() {
    assert!(serde_json::from_value::<OperationId>(json!("future_operation")).is_err());
    assert!(serde_json::from_value::<OperationPlan>(json!({
        "operation": "future_operation",
        "configuration": {}
    }))
    .is_err());
    assert!(serde_json::from_value::<ExpandedOperationCell>(json!({
        "operation": "future_operation",
        "cell": {}
    }))
    .is_err());
    assert!(serde_json::from_value::<OperationEvidence>(json!({
        "operation": "future_operation",
        "evidence": {}
    }))
    .is_err());
    assert!(
        serde_json::from_value::<OperationComparisonIdentity>(json!({
            "operation": "future_operation",
            "identity": {}
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<ProductAccess>(json!({
        "kind": "runtime_plugin",
        "action": "arbitrary"
    }))
    .is_err());
    assert!(serde_json::from_value::<WorkspaceAction>(json!("arbitrary_internal_action")).is_err());
}
