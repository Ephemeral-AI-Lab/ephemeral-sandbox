#[test]
fn config_prd_observability_section_deserializes_and_validates() {
    let cfg = prd_config();
    cfg.validate().expect("prd observability config is valid");
    assert!(cfg.enabled);
    assert_eq!(cfg.max_disk_bytes, 4 * 1024 * 1024);
    assert!(!cfg.used_legacy_max_file_bytes);
    assert_eq!(cfg.resource_stats, ResourceStatsConfig::default());
}

#[test]
fn config_observability_defaults_preserve_shipped_policy() {
    // prd.yml carries none of the new keys, so the section must load to
    // today's exact constants.
    let cfg = prd_config();
    assert_eq!(cfg.max_line_bytes, 16 * 1024);
    assert!(cfg.resource_stats.enabled);
    assert_eq!(cfg.resource_stats.sample_interval_ms, 2_000);
    assert_eq!(cfg.resource_stats.max_disk_bytes, 1024 * 1024);
    assert_eq!(cfg.sampling, SamplingConfig::default());
    assert_eq!(cfg.sampling.max_walk_nodes, 1024);
    assert_eq!(cfg.sampling.max_walk_depth, 64);
    assert_eq!(cfg.views, ViewsConfig::default());
    assert_eq!(cfg.views.resource_window_ms, 600_000);
    assert_eq!(cfg.views.layer_delta_default_limit, 500);
    assert_eq!(cfg.views.layer_delta_max_limit, 5_000);
    assert!(cfg.diagnostics.enabled);
    assert_eq!(cfg.diagnostics.cpu_threshold_percent, 2.0);
    assert_eq!(
        cfg.diagnostics.anonymous_memory_threshold_bytes,
        16 * 1024 * 1024
    );
    assert_eq!(cfg.diagnostics.sustained_window_ms, 30_000);
    assert_eq!(cfg.diagnostics.cooldown_ms, 300_000);
    assert_eq!(cfg.diagnostics.max_artifact_bytes, 1024 * 1024);
}

#[test]
fn config_observability_overrides_deserialize() {
    let cfg = observability_config(
        "  max_disk_bytes: 1048576
  max_line_bytes: 1024
  resource_stats:
    enabled: false
    sample_interval_ms: 250
    max_disk_bytes: 131072
  sampling:
    max_walk_nodes: 8
  views:
    layer_delta_default_limit: 3
    layer_delta_max_limit: 3
  diagnostics:
    cpu_threshold_percent: 0.5
    anonymous_memory_threshold_bytes: 1048576
    sustained_window_ms: 10
    cooldown_ms: 20
    max_artifact_bytes: 4096
",
    )
    .expect("observability overrides deserialize");
    cfg.validate().expect("observability overrides are valid");
    assert_eq!(cfg.max_disk_bytes, 1024 * 1024);
    assert_eq!(cfg.max_line_bytes, 1024);
    assert!(!cfg.resource_stats.enabled);
    assert_eq!(cfg.resource_stats.sample_interval_ms, 250);
    assert_eq!(cfg.resource_stats.max_disk_bytes, 131_072);
    assert_eq!(cfg.sampling.max_walk_nodes, 8);
    assert_eq!(cfg.sampling.max_walk_depth, 64);
    assert_eq!(cfg.views.layer_delta_default_limit, 3);
    assert_eq!(cfg.views.layer_delta_max_limit, 3);
    assert_eq!(cfg.views.resource_window_ms, 600_000);
    assert_eq!(cfg.diagnostics.cpu_threshold_percent, 0.5);
    assert_eq!(cfg.diagnostics.anonymous_memory_threshold_bytes, 1_048_576);
    assert_eq!(cfg.diagnostics.sustained_window_ms, 10);
    assert_eq!(cfg.diagnostics.cooldown_ms, 20);
    assert_eq!(cfg.diagnostics.max_artifact_bytes, 4_096);
}

#[test]
fn legacy_per_segment_budget_is_doubled_clamped_and_marked_deprecated() {
    let ordinary =
        observability_config("  max_file_bytes: 2097152\n").expect("legacy config deserializes");
    assert_eq!(ordinary.max_disk_bytes, 4 * 1024 * 1024);
    assert!(ordinary.used_legacy_max_file_bytes);

    let clamped = observability_config("  max_file_bytes: 268435456\n")
        .expect("oversized legacy config deserializes");
    assert_eq!(clamped.max_disk_bytes, 16 * 1024 * 1024);
    clamped.validate().expect("clamped legacy config validates");

    let error = observability_config("  max_disk_bytes: 4194304\n  max_file_bytes: 2097152\n")
        .expect_err("new and legacy budgets are mutually exclusive");
    assert!(error.to_string().contains("cannot both be set"), "{error}");
}

#[test]
fn config_observability_rejects_unknown_keys() {
    let error = observability_config("  sampling:\n    max_walk_files: 1\n")
        .expect_err("unknown sampling key must be rejected");
    assert!(error.to_string().contains("max_walk_files"), "{error}");

    let error = observability_config("  views:\n    resource_window_s: 1\n")
        .expect_err("unknown views key must be rejected");
    assert!(error.to_string().contains("resource_window_s"), "{error}");

    let error = observability_config("  diagnostics:\n    raw_command_lines: true\n")
        .expect_err("unknown diagnostics key must be rejected");
    assert!(error.to_string().contains("raw_command_lines"), "{error}");

    let error = observability_config("  resource_stats:\n    retry_count: 1\n")
        .expect_err("unknown resource_stats key must be rejected");
    assert!(error.to_string().contains("retry_count"), "{error}");
}

#[test]
fn config_validation_rejects_observability_edge_values() {
    let mut cfg = prd_config();
    cfg.max_disk_bytes = 1024 * 1024 - 1;
    assert_invalid(cfg, "observability.max_disk_bytes");

    let mut cfg = prd_config();
    cfg.max_disk_bytes = 16 * 1024 * 1024 + 1;
    assert_invalid(cfg, "observability.max_disk_bytes");

    let mut cfg = prd_config();
    cfg.max_line_bytes = 0;
    assert_invalid(cfg, "observability.max_line_bytes");

    let mut cfg = prd_config();
    cfg.max_line_bytes = 16 * 1024 + 1;
    assert_invalid(cfg, "observability.max_line_bytes");

    let mut cfg = prd_config();
    cfg.resource_stats.sample_interval_ms = 249;
    assert_invalid(cfg, "observability.resource_stats.sample_interval_ms");

    let mut cfg = prd_config();
    cfg.resource_stats.sample_interval_ms = 600_001;
    assert_invalid(cfg, "observability.resource_stats.sample_interval_ms");

    let mut cfg = prd_config();
    cfg.resource_stats.max_disk_bytes = 128 * 1024 - 1;
    assert_invalid(cfg, "observability.resource_stats.max_disk_bytes");

    let mut cfg = prd_config();
    cfg.resource_stats.max_disk_bytes = 4 * 1024 * 1024 + 2;
    assert_invalid(cfg, "observability.resource_stats.max_disk_bytes");

    let mut cfg = prd_config();
    cfg.resource_stats.max_disk_bytes = 128 * 1024 + 1;
    assert_invalid(cfg, "observability.resource_stats.max_disk_bytes");

    let mut cfg = prd_config();
    cfg.sampling.max_walk_nodes = 0;
    assert_invalid(cfg, "observability.sampling.max_walk_nodes");

    let mut cfg = prd_config();
    cfg.sampling.max_walk_depth = 0;
    assert_invalid(cfg, "observability.sampling.max_walk_depth");

    let mut cfg = prd_config();
    cfg.views.resource_window_ms = 0;
    assert_invalid(cfg, "observability.views.resource_window_ms");

    let mut cfg = prd_config();
    cfg.views.layer_delta_default_limit = 0;
    assert_invalid(cfg, "observability.views.layer_delta_default_limit");

    let mut cfg = prd_config();
    cfg.views.layer_delta_max_limit = 0;
    assert_invalid(cfg, "observability.views.layer_delta_max_limit");

    let mut cfg = prd_config();
    cfg.diagnostics.cpu_threshold_percent = 0.0;
    assert_invalid(cfg, "observability.diagnostics.cpu_threshold_percent");

    let mut cfg = prd_config();
    cfg.diagnostics.cpu_threshold_percent = 10_001.0;
    assert_invalid(cfg, "observability.diagnostics.cpu_threshold_percent");

    let mut cfg = prd_config();
    cfg.diagnostics.anonymous_memory_threshold_bytes = 0;
    assert_invalid(
        cfg,
        "observability.diagnostics.anonymous_memory_threshold_bytes",
    );

    let mut cfg = prd_config();
    cfg.diagnostics.sustained_window_ms = 0;
    assert_invalid(cfg, "observability.diagnostics.sustained_window_ms");

    let mut cfg = prd_config();
    cfg.diagnostics.cooldown_ms = 0;
    assert_invalid(cfg, "observability.diagnostics.cooldown_ms");

    let mut cfg = prd_config();
    cfg.diagnostics.max_artifact_bytes = 4_095;
    assert_invalid(cfg, "observability.diagnostics.max_artifact_bytes");

    let mut cfg = prd_config();
    cfg.diagnostics.max_artifact_bytes = 1024 * 1024 + 1;
    assert_invalid(cfg, "observability.diagnostics.max_artifact_bytes");
}

#[test]
fn config_validation_rejects_delta_default_above_max() {
    let mut cfg = prd_config();
    cfg.views.layer_delta_default_limit = 6_000;
    assert_invalid(cfg, "observability.views.layer_delta_default_limit");

    let mut cfg = prd_config();
    cfg.views.layer_delta_default_limit = 5_000;
    cfg.validate().expect("default equal to max is valid");
}

fn observability_config(extra_yaml: &str) -> Result<ObservabilityConfig, crate::ConfigError> {
    let yaml = format!("observability:\n  enabled: true\n{extra_yaml}");
    crate::ConfigDocument::parse(std::path::Path::new("<test>"), &yaml)?.section("observability")
}

fn prd_config() -> ObservabilityConfig {
    // prd.yml stays minimal and carries no observability section; the daemon
    // loads it with unwrap_or_default, mirrored here.
    match crate::load_baseline()
        .expect("prd config loads")
        .section("observability")
    {
        Ok(cfg) => cfg,
        Err(crate::ConfigError::MissingSection { .. }) => ObservabilityConfig::default(),
        Err(error) => panic!("observability section failed: {error}"),
    }
}

fn assert_invalid(config: ObservabilityConfig, field: &str) {
    let err = config.validate().expect_err("config should be invalid");
    let message = err.to_string();
    assert!(message.contains(field), "{message}");
}
