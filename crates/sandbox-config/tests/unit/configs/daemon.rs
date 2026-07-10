#[test]
fn config_prd_daemon_section_deserializes_and_validates() {
    let cfg = prd_config();
    cfg.validate().expect("prd daemon config is valid");
}

#[test]
fn config_prd_daemon_section_does_not_carry_dynamic_sandbox_identity() {
    let config_path = crate::ConfigPath::prd().expect("prd config path resolves");
    let raw = std::fs::read_to_string(config_path.as_path()).expect("prd config is readable");

    assert!(
        !raw.contains("sandbox_id"),
        "static daemon YAML must not contain dynamic sandbox identity"
    );
}

#[test]
fn config_validation_rejects_invalid_daemon_values() {
    let mut cfg = prd_config();
    cfg.server.max_worker_threads = 0;
    assert_invalid(cfg, "daemon.server.max_worker_threads");
}

#[test]
fn config_http_export_defaults_preserve_shipped_policy() {
    // prd.yml carries no daemon.http key, so the section must load to today's
    // exact constants.
    let cfg = prd_config();
    assert_eq!(cfg.http.export, DaemonHttpExportConfig::default());
    assert_eq!(cfg.http.export.frame_bytes, 1024 * 1024);
    assert_eq!(cfg.http.export.channel_frames, 4);
}

#[test]
fn config_http_export_overrides_deserialize() {
    let cfg = http_config("  http:\n    export:\n      frame_bytes: 4096\n      channel_frames: 1\n")
        .expect("http export overrides deserialize");
    cfg.validate().expect("http export overrides are valid");
    assert_eq!(cfg.http.export.frame_bytes, 4096);
    assert_eq!(cfg.http.export.channel_frames, 1);
}

#[test]
fn config_http_rejects_unknown_keys() {
    let error = http_config("  http:\n    forward: {}\n")
        .expect_err("unknown daemon.http key must be rejected");
    assert!(error.to_string().contains("forward"), "{error}");

    let error = http_config("  http:\n    export:\n      frames: 1\n")
        .expect_err("unknown daemon.http.export key must be rejected");
    assert!(error.to_string().contains("frames"), "{error}");
}

#[test]
fn config_validation_rejects_http_export_edge_values() {
    let mut cfg = prd_config();
    cfg.http.export.frame_bytes = 4095;
    assert_invalid(cfg, "daemon.http.export.frame_bytes");

    let mut cfg = prd_config();
    cfg.http.export.frame_bytes = 4096;
    cfg.validate().expect("frame_bytes 4096 is valid");

    let mut cfg = prd_config();
    cfg.http.export.channel_frames = 0;
    assert_invalid(cfg, "daemon.http.export.channel_frames");
}

fn http_config(http_yaml: &str) -> Result<DaemonConfig, crate::ConfigError> {
    let yaml = format!(
        "daemon:
  server:
    socket_path: /eos/runtime/daemon/runtime.sock
    pid_path: /eos/runtime/daemon/runtime.pid
    max_worker_threads: 32
{http_yaml}"
    );
    crate::ConfigDocument::parse(std::path::Path::new("<test>"), &yaml)?.section("daemon")
}

fn prd_config() -> DaemonConfig {
    crate::load_baseline()
        .expect("prd config loads")
        .section("daemon")
        .expect("daemon section deserializes")
}

fn assert_invalid(config: DaemonConfig, field: &str) {
    let err = config.validate().expect_err("config should be invalid");
    let message = err.to_string();
    assert!(message.contains(field), "{message}");
}
