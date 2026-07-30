#![cfg(all(feature = "manager", feature = "runtime", feature = "observability"))]

use std::process::Command;

use sandbox_cli::projection::document::{catalog_document, catalog_to_value};
use serde_json::{json, Value};

fn assert_phase_zero_operations_preserved(current: &Value, phase_zero: &Value) {
    for catalog in ["management", "runtime", "observability"] {
        assert_eq!(
            current[catalog]["operation_execution_space"],
            phase_zero[catalog]["operation_execution_space"],
            "{catalog} execution space"
        );
        let current_families = current[catalog]["families"]
            .as_array()
            .expect("current families");
        for expected in phase_zero[catalog]["families"]
            .as_array()
            .expect("Phase 0 families")
        {
            let id = expected["id"].as_str().expect("family id");
            let actual = current_families
                .iter()
                .find(|family| family["id"] == id)
                .unwrap_or_else(|| panic!("missing Phase 0 family: {catalog}.{id}"));
            assert_eq!(actual, expected, "{catalog}.{id} changed from Phase 0");
        }
        let current_operations = current[catalog]["operations"]
            .as_array()
            .expect("current operations");
        for expected in phase_zero[catalog]["operations"]
            .as_array()
            .expect("Phase 0 operations")
        {
            let name = expected["name"].as_str().expect("operation name");
            let actual = current_operations
                .iter()
                .find(|operation| operation["name"] == name)
                .unwrap_or_else(|| panic!("missing Phase 0 operation: {catalog}.{name}"));
            assert_eq!(actual, expected, "{catalog}.{name} changed from Phase 0");
        }
    }
}

#[test]
fn all_feature_compatibility_catalog_preserves_phase_zero_operations() {
    let management = catalog_document(
        sandbox_operation_catalog::manager::manager_catalog(),
        sandbox_cli::projection::manager::catalog_projection(),
    )
    .expect("management projection");
    let runtime = catalog_document(
        sandbox_operation_catalog::runtime::runtime_catalog(),
        sandbox_cli::projection::runtime::catalog_projection(),
    )
    .expect("runtime projection");
    let observability = catalog_document(
        sandbox_operation_catalog::observability::observability_catalog(),
        sandbox_cli::projection::observability::catalog_projection(),
    )
    .expect("observability projection");
    let catalog = json!({
        "management": catalog_to_value(&management),
        "runtime": catalog_to_value(&runtime),
        "observability": catalog_to_value(&observability),
    });
    let fixture: Value = serde_json::from_str(include_str!("fixtures/compatibility-catalog.json"))
        .expect("Phase 0 compatibility catalog fixture");

    assert_phase_zero_operations_preserved(&catalog, &fixture);
}

#[test]
fn unknown_operation_errors_and_exit_codes_match_phase_zero_fixture() {
    let cases = [
        (
            "sandbox-manager-cli",
            env!("CARGO_BIN_EXE_sandbox-manager-cli"),
            vec![
                "--gateway-socket",
                "127.0.0.1:1",
                "--gateway-auth-token",
                "phase-0-fixture",
                "phase0_unknown_operation",
            ],
        ),
        (
            "sandbox-runtime-cli",
            env!("CARGO_BIN_EXE_sandbox-runtime-cli"),
            vec![
                "--gateway-socket",
                "127.0.0.1:1",
                "--gateway-auth-token",
                "phase-0-fixture",
                "--sandbox-id",
                "eos-phase-0",
                "phase0_unknown_operation",
            ],
        ),
        (
            "sandbox-observability-cli",
            env!("CARGO_BIN_EXE_sandbox-observability-cli"),
            vec![
                "--gateway-socket",
                "127.0.0.1:1",
                "--gateway-auth-token",
                "phase-0-fixture",
                "phase0_unknown_operation",
            ],
        ),
    ];
    let actual = cases
        .iter()
        .map(|(name, binary, argv)| invocation(name, binary, argv))
        .collect::<Vec<_>>();

    let expected = include_str!("fixtures/unknown-operation-errors.json").replace("\r\n", "\n");
    assert_eq!(format!("{}\n", Value::Array(actual)), expected);
}

fn invocation(name: &str, binary: &str, argv: &[&str]) -> Value {
    let output = Command::new(binary)
        .args(argv)
        .output()
        .expect("run sandbox CLI");

    json!({
        "binary": name,
        "argv": argv,
        "exit_code": output.status.code().expect("numeric exit code"),
        "stdout": String::from_utf8(output.stdout).expect("stdout utf8"),
        "stderr": String::from_utf8(output.stderr).expect("stderr utf8"),
    })
}
