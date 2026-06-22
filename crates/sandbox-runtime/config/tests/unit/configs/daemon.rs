#[test]
fn config_prd_daemon_section_deserializes_and_validates() {
    let cfg = prd_config();
    cfg.validate().expect("prd daemon config is valid");
    assert!(!cfg.telemetry.enabled);
    assert!(cfg.telemetry.sink.is_none());
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

    let mut cfg = prd_config();
    cfg.commands.scratch_root = std::path::PathBuf::from("/");
    assert_invalid(cfg, "daemon.commands.scratch_root");

    let mut cfg = prd_config();
    cfg.cgroup_monitor.sample_interval_ms = 0;
    assert_invalid(cfg, "daemon.cgroup_monitor.sample_interval_ms");

    let mut cfg = prd_config();
    cfg.cgroup_monitor.retained_samples_per_target = 0;
    assert_invalid(cfg, "daemon.cgroup_monitor.retained_samples_per_target");
}

#[test]
fn telemetry_disabled_config_deserializes_without_sink() {
    let cfg = telemetry_config(
        r#"
enabled: false
service_name: sandbox-daemon
level: info
"#,
    );

    assert!(!cfg.enabled);
    assert!(cfg.sink.is_none());
    cfg.validate().expect("disabled telemetry validates");
    cfg.validate_for_serve_mode(DaemonServeMode::Spawn)
        .expect("disabled telemetry is valid under spawned serve");
}

#[test]
fn telemetry_section_defaults_to_disabled_when_omitted() {
    let cfg = daemon_config(
        r#"
server:
  socket_path: /eos/runtime/daemon/runtime.sock
  pid_path: /eos/runtime/daemon/runtime.pid
  max_worker_threads: 2
commands:
  scratch_root: /eos/scratch/commands
cgroup_monitor:
  enabled: true
  sample_interval_ms: 1000
  retained_samples_per_target: 10
  include_pids: true
  include_pressure: true
  include_disk: true
idle_workspace_eviction:
  interval_ms: 500
"#,
    );

    assert_eq!(cfg.telemetry, TelemetryConfig::default());
    cfg.validate()
        .expect("omitted telemetry defaults to disabled config");
}

#[test]
fn telemetry_local_json_accepts_stdout_and_stderr_in_foreground_mode() {
    for stream in ["stdout", "stderr"] {
        let cfg = telemetry_config(&format!(
            r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: local_json
  stream: {stream}
"#
        ));

        assert!(matches!(cfg.sink, Some(TelemetrySink::LocalJson { .. })));
        cfg.validate_for_serve_mode(DaemonServeMode::Foreground)
            .expect("local json stream is valid in foreground mode");
    }
}

#[test]
fn telemetry_rejects_invalid_stream() {
    let err = telemetry_deserialize_error(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: local_json
  stream: file
"#,
    );

    assert!(
        err.contains("stream") || err.contains("file"),
        "unexpected error: {err}"
    );
}

#[test]
fn telemetry_rejects_invalid_level() {
    let cfg = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: verbose
sink:
  kind: local_json
  stream: stdout
"#,
    );

    let err = cfg.validate().expect_err("invalid telemetry level rejected");

    assert_eq!(err.field, "daemon.telemetry.level");
}

#[test]
fn telemetry_rejects_unknown_sink_kind() {
    let err = telemetry_deserialize_error(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: otlp
  stream: stdout
"#,
    );

    assert!(
        err.contains("otlp") || err.contains("kind"),
        "unexpected error: {err}"
    );
}

#[test]
fn telemetry_enabled_config_requires_sink() {
    let cfg = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
"#,
    );

    let err = cfg
        .validate()
        .expect_err("enabled telemetry without sink is rejected");

    assert_eq!(err.field, "daemon.telemetry.sink");
}

#[test]
fn telemetry_rejects_local_json_in_spawn_mode() {
    let cfg = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: local_json
  stream: stderr
"#,
    );

    let err = cfg
        .validate_for_serve_mode(DaemonServeMode::Spawn)
        .expect_err("local json stdout/stderr is foreground-only");

    assert_eq!(err.field, "daemon.telemetry.sink");
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

fn daemon_config(yaml: &str) -> DaemonConfig {
    serde_yaml_ng::from_str(yaml).expect("daemon config deserializes")
}

fn telemetry_config(yaml: &str) -> TelemetryConfig {
    serde_yaml_ng::from_str(yaml).expect("telemetry config deserializes")
}

fn telemetry_deserialize_error(yaml: &str) -> String {
    serde_yaml_ng::from_str::<TelemetryConfig>(yaml)
        .expect_err("telemetry config should fail to deserialize")
        .to_string()
}
