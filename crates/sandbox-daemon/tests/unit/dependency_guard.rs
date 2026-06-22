#[test]
fn daemon_manifest_excludes_host_store_and_sqlite_dependencies() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("read daemon manifest");
    for forbidden in ["rusqlite", "host"] {
        assert!(
            !manifest.contains(forbidden),
            "daemon hot path must not depend on {forbidden}"
        );
    }
}

#[test]
fn forbidden_runtime_telemetry_infrastructure_is_absent() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root");

    for forbidden in [
        "crates/sandbox-runtime-trace",
        "crates/sandbox-runtime/operation/src/internal/telemetry.rs",
    ] {
        assert!(
            !workspace_root.join(forbidden).exists(),
            "forbidden telemetry infrastructure exists: {forbidden}"
        );
    }
}
