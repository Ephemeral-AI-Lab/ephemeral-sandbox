#[test]
fn config_prd_runtime_section_deserializes_and_validates() {
    let config = prd_config();
    config.validate().expect("prd runtime config is valid");
    assert_eq!(
        config.workspace.scratch_root,
        std::path::PathBuf::from("/eos/workspace")
    );
    assert_eq!(
        config.namespace_execution.scratch_root,
        std::path::PathBuf::from("/eos/namespace_execution")
    );
}

#[test]
fn config_default_workspace_section_is_valid() {
    let config = WorkspaceConfig::default();
    config.validate().expect("default config is valid");
}

#[test]
fn config_validation_rejects_invalid_runtime_workspace_values() {
    let mut cfg = prd_config();
    cfg.workspace.layer_stack_root = std::path::PathBuf::from("relative");
    assert_invalid(cfg, "runtime.workspace.layer_stack_root");

    let mut cfg = prd_config();
    cfg.workspace.layer_stack_root = std::path::PathBuf::from("/");
    assert_invalid(cfg, "runtime.workspace.layer_stack_root");

    let mut cfg = prd_config();
    cfg.workspace.scratch_root = std::path::PathBuf::from("relative");
    assert_invalid(cfg, "runtime.workspace.scratch_root");

    let mut cfg = prd_config();
    cfg.workspace.scratch_root = std::path::PathBuf::from("/");
    assert_invalid(cfg, "runtime.workspace.scratch_root");

    let mut cfg = prd_config();
    cfg.workspace.exit_grace_s = -0.1;
    assert_invalid(cfg, "runtime.workspace.exit_grace_s");

    let mut cfg = prd_config();
    cfg.namespace_execution.scratch_root = std::path::PathBuf::from("/");
    assert_invalid(cfg, "runtime.namespace_execution.scratch_root");
}

#[test]
fn config_layerstack_defaults_preserve_shipped_policy() {
    // prd.yml carries no runtime.layerstack key, so the section must load to
    // today's exact constants.
    let config = prd_config();
    assert_eq!(config.layerstack, LayerstackConfig::default());
    assert_eq!(config.layerstack.remount_sweep_width, 4);
    assert_eq!(config.layerstack.export_chunk_bytes, 2 * 1024 * 1024);
    assert_eq!(config.layerstack.spool_zstd_level, 3);
}

#[test]
fn config_layerstack_overrides_deserialize() {
    let config = layerstack_config(
        "  layerstack:
    remount_sweep_width: 1
    export_chunk_bytes: 4096
    spool_zstd_level: 19
",
    )
    .expect("layerstack overrides deserialize");
    config.validate().expect("layerstack overrides are valid");
    assert_eq!(config.layerstack.remount_sweep_width, 1);
    assert_eq!(config.layerstack.export_chunk_bytes, 4096);
    assert_eq!(config.layerstack.spool_zstd_level, 19);
}

#[test]
fn config_layerstack_rejects_unknown_key() {
    let error = layerstack_config("  layerstack:\n    sweep_width: 4\n")
        .expect_err("unknown layerstack key must be rejected");
    assert!(error.to_string().contains("sweep_width"), "{error}");
}

#[test]
fn config_validation_rejects_layerstack_edge_values() {
    let mut cfg = prd_config();
    cfg.layerstack.remount_sweep_width = 0;
    assert_invalid(cfg, "runtime.layerstack.remount_sweep_width");

    let mut cfg = prd_config();
    cfg.layerstack.export_chunk_bytes = 0;
    assert_invalid(cfg, "runtime.layerstack.export_chunk_bytes");

    let mut cfg = prd_config();
    cfg.layerstack.spool_zstd_level = 0;
    assert_invalid(cfg, "runtime.layerstack.spool_zstd_level");

    let mut cfg = prd_config();
    cfg.layerstack.spool_zstd_level = 23;
    assert_invalid(cfg, "runtime.layerstack.spool_zstd_level");

    // The zstd bounds themselves are accepted.
    let mut cfg = prd_config();
    cfg.layerstack.spool_zstd_level = 1;
    cfg.validate().expect("zstd level 1 is valid");
    cfg.layerstack.spool_zstd_level = 22;
    cfg.validate().expect("zstd level 22 is valid");
}

fn layerstack_config(layerstack_yaml: &str) -> Result<RuntimeConfig, crate::ConfigError> {
    let yaml = format!(
        "runtime:
  workspace:
    layer_stack_root: /eos/layer-stack
    scratch_root: /eos/workspace
    setup_timeout_s: 30
    exit_grace_s: 0.25
    rfc1918_egress: allow
  namespace_execution:
    scratch_root: /eos/namespace_execution
{layerstack_yaml}"
    );
    crate::ConfigDocument::parse(std::path::Path::new("<test>"), &yaml)?.section("runtime")
}

fn prd_config() -> RuntimeConfig {
    crate::load_baseline()
        .expect("prd config loads")
        .section("runtime")
        .expect("runtime section deserializes")
}

fn assert_invalid(config: RuntimeConfig, field: &str) {
    let err = config.validate().expect_err("config should be invalid");
    let message = err.to_string();
    assert!(message.contains(field), "{message}");
}
