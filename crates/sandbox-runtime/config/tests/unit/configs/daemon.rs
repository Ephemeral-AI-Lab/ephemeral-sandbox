#[test]
fn config_prd_daemon_section_deserializes_and_validates() {
    prd_config().validate().expect("prd daemon config is valid");
}

#[test]
fn config_validation_rejects_invalid_daemon_values() {
    let mut cfg = prd_config();
    cfg.server.max_worker_threads = 0;
    assert_invalid(cfg, "daemon.server.max_worker_threads");

    let mut cfg = prd_config();
    cfg.commands.scratch_root = std::path::PathBuf::from("/");
    assert_invalid(cfg, "daemon.commands.scratch_root");
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
