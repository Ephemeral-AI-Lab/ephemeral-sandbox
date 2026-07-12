use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::yaml::Value;
use super::*;

#[test]
fn load_path_reads_committed_baseline() {
    let path = ConfigPath::prd().expect("resolve prd config");
    let doc = load_path(path.as_path()).expect("prd.yml loads");
    let section = doc
        .section::<Value>("daemon")
        .expect("daemon section exists");

    assert!(matches!(section, Value::Mapping(_)));
}

#[test]
fn merge_recurses_objects_replaces_scalars_and_replaces_arrays() {
    let mut baseline = parse_doc(
        r#"
runtime:
  namespace_execution:
    scratch_root: /eos/namespace_execution
runner:
  mount_mask:
    hidden_paths: [/eos, /tmp/eos]
"#,
    );
    let override_doc = parse_doc(
        r#"
runtime:
  namespace_execution:
    scratch_root: /tmp/eos/namespace_execution
runner:
  mount_mask:
    hidden_paths: [/eos]
"#,
    );

    baseline.merge(override_doc).expect("merge succeeds");

    let runtime = baseline
        .section::<Value>("runtime")
        .expect("runtime section deserializes");
    let runner = baseline
        .section::<Value>("runner")
        .expect("runner section deserializes");
    assert_eq!(
        runtime["namespace_execution"]["scratch_root"],
        Value::String("/tmp/eos/namespace_execution".to_owned())
    );
    assert_eq!(
        runner["mount_mask"]["hidden_paths"],
        Value::Sequence(vec![Value::String("/eos".to_owned())])
    );
}

#[test]
fn section_reports_unknown_field_errors() {
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictSection {
        #[serde(rename = "expected")]
        _expected: u64,
    }

    let doc = parse_doc(
        r#"
daemon:
  expected: 1
  unexpected: true
"#,
    );

    let err = doc
        .section::<StrictSection>("daemon")
        .expect_err("unknown field should fail");
    let message = err.to_string();

    assert!(message.contains("daemon"), "{message}");
    assert!(message.contains("unexpected"), "{message}");
}

#[test]
fn section_reports_wrong_type_errors() {
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictSection {
        #[serde(rename = "expected")]
        _expected: u64,
    }

    let doc = parse_doc(
        r#"
daemon:
  expected: wrong
"#,
    );

    let err = doc
        .section::<StrictSection>("daemon")
        .expect_err("wrong field type should fail");
    let message = err.to_string();

    assert!(message.contains("daemon"), "{message}");
    assert!(message.contains("expected"), "{message}");
}

#[test]
fn document_deserializes_the_strict_root_schema() {
    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct StrictDocument {
        schema_version: u32,
        name: String,
    }

    let doc = parse_doc("schema_version: 1\nname: benchmark\n");
    let parsed = doc
        .document::<StrictDocument>()
        .expect("complete document deserializes");

    assert_eq!(
        parsed,
        StrictDocument {
            schema_version: 1,
            name: "benchmark".to_owned(),
        }
    );
}

#[test]
fn document_reports_unknown_root_fields() {
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictDocument {
        #[serde(rename = "schema_version")]
        _schema_version: u32,
    }

    let doc = parse_doc("schema_version: 1\nunexpected: true\n");
    let error = doc
        .document::<StrictDocument>()
        .expect_err("unknown root field should fail");

    assert!(error.to_string().contains("unexpected"), "{error}");
}

#[test]
fn load_test_override_merges_sandbox_local_test_yaml() {
    let root = test_workspace_dir("load-test-override");
    fs::create_dir_all(&root).expect("create test dir");
    let override_path = root.join("local.test.yml");
    fs::write(
        &override_path,
        r#"
runner:
  mount_mask:
    hidden_paths: [/tmp/eos]
"#,
    )
    .expect("write override");

    let doc = load_test_override(&override_path).expect("override loads");
    let section = doc.section::<Value>("runner").expect("section loads");

    assert_eq!(
        section["mount_mask"]["hidden_paths"],
        Value::Sequence(vec![Value::String("/tmp/eos".to_owned())])
    );
    assert!(matches!(section["mount_mask"], Value::Mapping(_)));
}

#[test]
fn load_test_override_rejects_non_test_yml_path() {
    let root = test_workspace_dir("non-test-yml");
    fs::create_dir_all(&root).expect("create test dir");
    let override_path = root.join("local.yml");
    fs::write(&override_path, "version: 1\n").expect("write override");

    let err = load_test_override(&override_path).expect_err("path suffix should fail");

    assert!(matches!(err, ConfigError::InvalidOverridePath { .. }));
    assert!(err.to_string().contains(".test.yml"));
}

#[test]
fn load_test_override_rejects_path_outside_sandbox_workspace() {
    let override_path = std::env::temp_dir().join(format!(
        "config-outside-{}-{}.test.yml",
        std::process::id(),
        unique_suffix()
    ));
    fs::write(&override_path, "version: 1\n").expect("write override");

    let err = load_test_override(&override_path).expect_err("outside path should fail");

    let _ = fs::remove_file(&override_path);
    assert!(matches!(err, ConfigError::InvalidOverridePath { .. }));
    assert!(err.to_string().contains("inside sandbox workspace"));
}

#[cfg(unix)]
#[test]
fn load_test_override_rejects_symlink_to_prd_baseline() {
    let root = test_workspace_dir("prd-symlink");
    fs::create_dir_all(&root).expect("create test dir");
    let link_path = root.join("prd-link.test.yml");
    let _ = fs::remove_file(&link_path);
    std::os::unix::fs::symlink(
        ConfigPath::prd().expect("resolve prd config").as_path(),
        &link_path,
    )
    .expect("create prd symlink");

    let err = load_test_override(&link_path).expect_err("prd symlink should fail");

    assert!(matches!(err, ConfigError::InvalidOverridePath { .. }));
    assert!(err.to_string().contains("prd.yml"));
}

#[test]
fn bench_template_round_trips_after_width_substitution() {
    // The bench driver substitutes the sweep width straight into
    // runtime.layerstack; a rendered arm file must load through every schema
    // section with no env side channel left in the template.
    let template = fs::read_to_string(workspace_root().join("config").join("bench.yml"))
        .expect("bench.yml is readable");
    assert!(
        !template.contains("EOS_"),
        "bench.yml must carry no env side channels"
    );
    let rendered = template
        .replace("__SWEEP_WIDTH__", "8")
        .replace("__DAEMON_CONFIG_PATH__", "/tmp/eos-bench-daemon.yml");
    let doc = parse_doc(&rendered);

    let runtime: configs::runtime::RuntimeConfig = doc.section("runtime").expect("runtime section");
    runtime.validate().expect("bench runtime config is valid");
    assert_eq!(runtime.layerstack.remount_sweep_width, 8);

    let daemon: configs::daemon::DaemonConfig = doc.section("daemon").expect("daemon section");
    daemon.validate().expect("bench daemon config is valid");

    let manager: configs::manager::ManagerConfig = doc.section("manager").expect("manager section");
    manager.validate().expect("bench manager config is valid");

    doc.section::<configs::observability::ObservabilityConfig>("observability")
        .expect("observability section");
}

#[test]
fn maximal_config_shape_loads_through_every_section_schema() {
    // The spec's maximal YAML after all four consolidation phases (minus the
    // phase-1 drift: the export stream surface was removed, so there is no
    // daemon.http.export subsection and no token_ttl_s). Every section must
    // deserialize and validate in one piece.
    let doc = parse_doc(
        r"
daemon:
  server:
    socket_path: /eos/runtime/daemon/runtime.sock
    pid_path: /eos/runtime/daemon/runtime.pid
    max_worker_threads: 32
    max_concurrent_connections: 256
    max_request_bytes: 16777216
    request_read_timeout_s: 30.0
  http:
    forward:
      connect_timeout_s: 10.0
      response_timeout_s: 30.0

runtime:
  workspace:
    layer_stack_root: /eos/layer-stack
    scratch_root: /eos/workspace
    setup_timeout_s: 30
    exit_grace_s: 0.25
    rfc1918_egress: allow
  namespace_execution:
    scratch_root: /eos/namespace_execution
    freeze_budget_s: 0.5
    stdin_write_deadline_s: 2.0
    max_terminal_entries: 512
    max_transcript_window_bytes: 1048576
    max_runner_result_bytes: 8388608
  command:
    max_active: 256
    read_lines_default: 200
    read_lines_max: 1000
  file:
    read_lines_default: 2000
    max_output_bytes: 262144
    max_edit_bytes: 4194304
    max_list_entries: 2000
  layerstack:
    remount_sweep_width: 4
    export_chunk_bytes: 2097152
    spool_zstd_level: 3

runner:
  mount_mask:
    hidden_paths:
      - /eos

observability:
  enabled: true
  max_file_bytes: 8388608
  max_line_bytes: 16384
  sampling:
    max_walk_nodes: 1024
    max_walk_depth: 64
  views:
    resource_window_ms: 600000
    layer_delta_default_limit: 500
    layer_delta_max_limit: 5000

gateway:
  bind_addr: 127.0.0.1:7878
  pid_path: /tmp/eos-gateway.pid
  max_concurrent_connections: 256

console:
  bind_addr: 127.0.0.1:7880
  rpc_timeout_s: 120.0
  health_probe_timeout_s: 2.0
  proxy_connect_timeout_s: 10.0
  proxy_response_timeout_s: 30.0
  endpoint_resolve_timeout_s: 5.0
  endpoint_cache_ttl_s: 3.0

manager:
  registry_path: null
  export:
    max_stream_bytes: 2147483648
    max_decompressed_bytes: 8589934592
    max_apply_entries: 1000000
  observability_snapshot:
    max_concurrent_requests: 8
    timeout_ms: 1500
  local_daemon:
    ready_timeout_s: 2.0
    stop_timeout_s: 2.0
  docker:
    privileged: false
    daemon_binary_path: dist/sandbox-daemon-linux-arm64
    daemon_config_yaml_path: config/prd.yml
    connect_timeout_s: 120
    stop_timeout_s: 5
    readiness_poll_ms: 250
    port_publish_attempts: 200
    port_publish_retry_delay_ms: 50
    container_env:
      HTTP_PROXY: http://http.docker.internal:3128
      HTTPS_PROXY: http://http.docker.internal:3128
      NO_PROXY: localhost,127.0.0.1,::1
",
    );

    let daemon: configs::daemon::DaemonConfig = doc.section("daemon").expect("daemon section");
    daemon.validate().expect("maximal daemon config is valid");

    let runtime: configs::runtime::RuntimeConfig = doc.section("runtime").expect("runtime section");
    runtime.validate().expect("maximal runtime config is valid");

    doc.section::<configs::runner::RunnerConfig>("runner")
        .expect("runner section");

    let observability: configs::observability::ObservabilityConfig =
        doc.section("observability").expect("observability section");
    observability
        .validate()
        .expect("maximal observability config is valid");

    let gateway: configs::gateway::GatewayConfig = doc.section("gateway").expect("gateway section");
    gateway.validate().expect("maximal gateway config is valid");

    let console: configs::console::ConsoleConfig = doc.section("console").expect("console section");
    console.validate().expect("maximal console config is valid");

    let manager: configs::manager::ManagerConfig = doc.section("manager").expect("manager section");
    manager.validate().expect("maximal manager config is valid");
    assert!(manager.docker.is_some());
}

fn test_workspace_dir(label: &str) -> PathBuf {
    workspace_root()
        .join("target")
        .join("config-tests")
        .join(format!(
            "{label}-{}-{}",
            std::process::id(),
            unique_suffix()
        ))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .find(|path| path.join("config").join("prd.yml").is_file())
        .expect("crate lives below sandbox workspace")
        .to_path_buf()
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_nanos()
}

fn parse_doc(text: &str) -> ConfigDocument {
    ConfigDocument::parse(Path::new("<test>"), text).expect("document parses")
}
