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
        Some(std::path::PathBuf::from("/eos/namespace_execution"))
    );
}

#[test]
fn config_default_workspace_section_is_valid() {
    let config = WorkspaceConfig::default();
    config.validate().expect("default config is valid");
}

#[test]
fn deprecated_namespace_scratch_root_can_be_omitted() {
    let yaml = "runtime:
  workspace:
    layer_stack_root: /eos/layer-stack
    scratch_root: /eos/workspace
    setup_timeout_s: 30
    exit_grace_s: 0.25
    rfc1918_egress: allow
  namespace_execution: {}
";
    let config: RuntimeConfig = crate::ConfigDocument::parse(std::path::Path::new("<test>"), yaml)
        .expect("document parses")
        .section("runtime")
        .expect("runtime deserializes");
    assert_eq!(config.namespace_execution.scratch_root, None);
    config
        .validate()
        .expect("omitting the compatibility root is valid");
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
    cfg.namespace_execution.scratch_root = Some(std::path::PathBuf::from("/"));
    assert_invalid(cfg, "runtime.namespace_execution.scratch_root");

    let mut cfg = prd_config();
    cfg.workspace.scratch_root = cfg.workspace.layer_stack_root.join("workspace");
    assert_invalid(cfg, "runtime.workspace.scratch_root");

    let mut cfg = prd_config();
    cfg.namespace_execution.scratch_root = Some(cfg.workspace.scratch_root.join("legacy"));
    assert_invalid(cfg, "runtime.namespace_execution.scratch_root");

    let mut cfg = prd_config();
    cfg.namespace_execution.scratch_root = Some(cfg.workspace.layer_stack_root.join("legacy"));
    assert_invalid(cfg, "runtime.namespace_execution.scratch_root");
}

#[test]
fn config_layerstack_defaults_preserve_shipped_policy() {
    let config = prd_config();
    assert_eq!(
        config.layerstack.rollout_mode,
        StorageRolloutMode::Legacy
    );
    assert_eq!(config.layerstack.remount_sweep_width, 4);
    assert_eq!(config.layerstack.export_chunk_bytes, 2 * 1024 * 1024);
    assert_eq!(config.layerstack.spool_zstd_level, 3);
    assert_eq!(
        config.layerstack.autosquash_policies.squash_at_n_layers,
        Some(100)
    );

    let omitted = layerstack_config("").expect("omitted layerstack config deserializes");
    assert_eq!(omitted.layerstack, LayerstackConfig::default());
    assert_eq!(
        omitted.layerstack.autosquash_policies.squash_at_n_layers,
        None
    );
}

#[test]
fn config_layerstack_overrides_deserialize() {
    let config = layerstack_config(
        "  layerstack:
    rollout_mode: legacy
    remount_sweep_width: 1
    export_chunk_bytes: 4096
    spool_zstd_level: 19
    autosquash_policies:
      squash_at_n_layers: 123
",
    )
    .expect("layerstack overrides deserialize");
    config.validate().expect("layerstack overrides are valid");
    assert_eq!(
        config.layerstack.rollout_mode,
        StorageRolloutMode::Legacy
    );
    assert_eq!(config.layerstack.remount_sweep_width, 1);
    assert_eq!(config.layerstack.export_chunk_bytes, 4096);
    assert_eq!(config.layerstack.spool_zstd_level, 19);
    assert_eq!(
        config.layerstack.autosquash_policies.squash_at_n_layers,
        Some(123)
    );
}

#[test]
fn config_layerstack_rejects_unknown_rollout_mode() {
    let error = layerstack_config("  layerstack:\n    rollout_mode: candidate\n")
        .expect_err("unknown rollout mode must be rejected");
    assert!(error.to_string().contains("candidate"), "{error}");
}

#[test]
fn config_layerstack_rejects_unknown_key() {
    let error = layerstack_config("  layerstack:\n    sweep_width: 4\n")
        .expect_err("unknown layerstack key must be rejected");
    assert!(error.to_string().contains("sweep_width"), "{error}");

    let error = layerstack_config(
        "  layerstack:\n    autosquash_policies:\n      squash_every_n_layers: 100\n",
    )
    .expect_err("unknown autosquash policy key must be rejected");
    assert!(
        error.to_string().contains("squash_every_n_layers"),
        "{error}"
    );
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

    for threshold in 0..3 {
        let mut cfg = layerstack_config("").expect("omitted policy config deserializes");
        cfg.layerstack.autosquash_policies.squash_at_n_layers = Some(threshold);
        assert_invalid(
            cfg,
            "runtime.layerstack.autosquash_policies.squash_at_n_layers",
        );
    }

    let mut cfg = layerstack_config("").expect("omitted policy config deserializes");
    cfg.layerstack.autosquash_policies.squash_at_n_layers = Some(3);
    cfg.validate().expect("threshold three is valid");
}

#[test]
fn config_operation_caps_default_to_shipped_policy() {
    // prd.yml carries none of the tier-3 keys, so every cap must load to
    // today's exact constants.
    let config = prd_config();
    assert_eq!(config.command, CommandConfig::default());
    assert_eq!(config.command.max_active, 32);
    assert_eq!(config.command.read_lines_default, 200);
    assert_eq!(config.command.read_lines_max, 1000);
    assert_eq!(config.file, FileConfig::default());
    assert_eq!(config.file.read_lines_default, 2000);
    assert_eq!(config.file.max_output_bytes, 256 * 1024);
    assert_eq!(config.file.max_edit_bytes, 4 * 1024 * 1024);
    assert_eq!(config.file.max_list_entries, 2000);
    assert!((config.namespace_execution.freeze_budget_s - 0.5).abs() < f64::EPSILON);
    assert!((config.namespace_execution.stdin_write_deadline_s - 2.0).abs() < f64::EPSILON);
    assert_eq!(config.namespace_execution.max_terminal_entries, 512);
    assert_eq!(
        config.namespace_execution.max_transcript_window_bytes,
        1024 * 1024
    );
    assert_eq!(
        config.namespace_execution.max_runner_result_bytes,
        8 * 1024 * 1024
    );
}

#[test]
fn config_operation_caps_overrides_deserialize() {
    let config = layerstack_config(
        "  command:
    max_active: 1
    read_lines_default: 10
    read_lines_max: 10
  file:
    max_list_entries: 5
    max_edit_bytes: 1024
",
    )
    .expect("operation cap overrides deserialize");
    config
        .validate()
        .expect("operation cap overrides are valid");
    assert_eq!(config.command.max_active, 1);
    assert_eq!(config.command.read_lines_default, 10);
    assert_eq!(config.file.max_list_entries, 5);
    assert_eq!(config.file.max_edit_bytes, 1024);
    assert_eq!(config.file.read_lines_default, 2000);
}

#[test]
fn config_namespace_execution_cap_overrides_deserialize() {
    let config = layerstack_config("").map(|mut config| {
        config.namespace_execution.max_terminal_entries = 2;
        config
    });
    let config = config.expect("baseline deserializes");
    config.validate().expect("lowered retention is valid");

    let error = layerstack_config("  command:\n    max_parallel: 4\n")
        .expect_err("unknown command key must be rejected");
    assert!(error.to_string().contains("max_parallel"), "{error}");

    let error = layerstack_config("  file:\n    list_max: 4\n")
        .expect_err("unknown file key must be rejected");
    assert!(error.to_string().contains("list_max"), "{error}");
}

#[test]
fn config_validation_rejects_operation_cap_edge_values() {
    let mut cfg = prd_config();
    cfg.command.max_active = 0;
    assert_invalid(cfg, "runtime.command.max_active");

    let mut cfg = prd_config();
    cfg.command.max_active = usize::MAX;
    assert_invalid(cfg, "runtime.command.max_active");

    let mut cfg = prd_config();
    cfg.command.read_lines_default = 1001;
    assert_invalid(cfg, "runtime.command.read_lines_default");

    let mut cfg = prd_config();
    cfg.file.max_list_entries = 0;
    assert_invalid(cfg, "runtime.file.max_list_entries");

    let mut cfg = prd_config();
    cfg.namespace_execution.freeze_budget_s = -0.1;
    assert_invalid(cfg, "runtime.namespace_execution.freeze_budget_s");

    let mut cfg = prd_config();
    cfg.namespace_execution.freeze_budget_s = 0.0;
    cfg.validate().expect("a zero freeze budget is valid");

    let mut cfg = prd_config();
    cfg.namespace_execution.stdin_write_deadline_s = 0.0;
    assert_invalid(cfg, "runtime.namespace_execution.stdin_write_deadline_s");

    let mut cfg = prd_config();
    cfg.namespace_execution.max_terminal_entries = 0;
    assert_invalid(cfg, "runtime.namespace_execution.max_terminal_entries");

    let mut cfg = prd_config();
    cfg.namespace_execution.max_transcript_window_bytes = 0;
    assert_invalid(
        cfg,
        "runtime.namespace_execution.max_transcript_window_bytes",
    );

    let mut cfg = prd_config();
    cfg.namespace_execution.max_runner_result_bytes = 0;
    assert_invalid(cfg, "runtime.namespace_execution.max_runner_result_bytes");
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
