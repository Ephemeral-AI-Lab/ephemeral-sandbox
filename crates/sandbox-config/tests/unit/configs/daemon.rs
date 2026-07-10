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
fn config_daemon_rejects_unknown_http_subsection() {
    // The daemon schema deliberately carries no `http` subsection: the export
    // spool stream (its one candidate consumer) was removed in favor of RPC
    // paging, so a config naming it must fail loudly instead of loading into
    // nothing.
    let yaml = "daemon:
  server:
    socket_path: /eos/runtime/daemon/runtime.sock
    pid_path: /eos/runtime/daemon/runtime.pid
    max_worker_threads: 32
  http:
    export:
      frame_bytes: 4096
";
    let error = crate::ConfigDocument::parse(std::path::Path::new("<test>"), yaml)
        .expect("document parses")
        .section::<DaemonConfig>("daemon")
        .expect_err("unknown daemon.http subsection must be rejected");
    assert!(error.to_string().contains("http"), "{error}");
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
