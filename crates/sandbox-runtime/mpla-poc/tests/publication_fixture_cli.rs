use std::process::Command;

use sandbox_runtime_mpla_poc::{
    prepared_fixture_storage_requirement, PREPARED_FIXTURE_DEPTH_EIGHT_BYTES,
};

#[test]
fn prepared_fixture_storage_requirement_reserves_build_headroom() {
    let requirement = prepared_fixture_storage_requirement().expect("storage requirement");

    assert_eq!(requirement.chain_bytes, PREPARED_FIXTURE_DEPTH_EIGHT_BYTES);
    assert_eq!(
        requirement.control_source_bytes,
        1024 * 1024 * 1024 + 1024 * 1024
    );
    assert_eq!(requirement.working_headroom_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(
        requirement.required_available_bytes,
        requirement.working_headroom_bytes
    );
    assert!(requirement.required_available_bytes < requirement.chain_bytes);
    assert!(requirement.minimum_available_inodes >= 4 * 1024);
}

#[test]
fn fixture_preparation_rejects_a_missing_fixed_fixture_profile() {
    let output = Command::new(env!("CARGO_BIN_EXE_mpla-speed-poc-v1"))
        .args([
            "prepare-publication-fixture",
            "--run-id",
            "publication-fixture-cli-test",
            "--candidate-sandbox-id",
            "eos-candidate",
            "--build-commit",
            "0123456789abcdef0123456789abcdef01234567",
        ])
        .output()
        .expect("run fixture-preparation parser");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("required arguments were not provided"));
}

#[test]
fn fixture_preparation_accepts_the_fixed_prebuilt_fixture_profile() {
    let output = Command::new(env!("CARGO_BIN_EXE_mpla-speed-poc-v1"))
        .args([
            "prepare-publication-fixture",
            "--run-id",
            "publication-fixture-cache-cli-test",
            "--candidate-sandbox-id",
            "eos-candidate",
            "--build-commit",
            "0123456789abcdef0123456789abcdef01234567",
            "--fixture-profile",
            "s4-chain-sparse-v1",
        ])
        .output()
        .expect("run prebuilt fixture-preparation parser");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        !stderr.contains("invalid value 's4-chain-sparse-v1'"),
        "the fixed prepared-fixture profile must be accepted by the parser: {stderr}"
    );
    assert!(
        stdout.starts_with("MPLA_SCORECARD_ERROR "),
        "the accepted fixed profile must reach application validation: stdout={stdout}; stderr={stderr}"
    );
}

#[test]
fn fixture_preparation_rejects_the_retired_fixture_profile() {
    let output = Command::new(env!("CARGO_BIN_EXE_mpla-speed-poc-v1"))
        .args([
            "prepare-publication-fixture",
            "--run-id",
            "publication-fixture-cache-cli-test",
            "--candidate-sandbox-id",
            "eos-candidate",
            "--build-commit",
            "0123456789abcdef0123456789abcdef01234567",
            "--fixture-profile",
            "s4-chain-v9",
        ])
        .output()
        .expect("run retired fixture-preparation parser");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("invalid value 's4-chain-v9'"));
    assert!(stderr.contains("s4-chain-sparse-v1"));
}

#[test]
fn inspect_prepared_fixture_cache_parser_is_available() {
    let output = Command::new(env!("CARGO_BIN_EXE_mpla-speed-poc-v1"))
        .args(["inspect-prepared-fixture-cache", "--help"])
        .output()
        .expect("run prepared-fixture cache inspection parser");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "inspection command must parse: {stdout}"
    );
    assert!(stdout.contains("inspect-prepared-fixture-cache"));
    assert!(!stdout.contains("--fixture-root"));
    assert!(!stdout.contains("--branch"));
}
