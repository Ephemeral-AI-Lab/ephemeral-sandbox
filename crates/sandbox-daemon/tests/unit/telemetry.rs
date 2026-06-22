use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use opentelemetry::Key;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::trace::{
    BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider, SpanData, SpanExporter,
};
use serde_json::{json, Value};
use tokio::runtime::Runtime;
use tokio::time::timeout;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

use crate::server::{SandboxDaemonServer, ServerConfig};
use crate::telemetry::{
    DaemonServeMode, OtlpProtocol, TelemetryConfig, TelemetryMetricsConfig,
    TelemetryOutputStream, TelemetrySink,
};

#[test]
fn local_json_telemetry_formats_span_close_records() -> Result<()> {
    let writer = CaptureWriter::default();
    let runtime = Runtime::new()?;
    let server = test_server(Some("sbox-json"));
    let request = json!({
        "op": "unknown_op",
        "request_id": "req-json",
        "scope": { "kind": "sandbox", "sandbox_id": "scope-sbox" },
        "args": {}
    });
    let request_bytes = serde_json::to_vec(&request)?;

    let response = crate::telemetry::with_test_json_subscriber(
        &local_json_telemetry(TelemetryOutputStream::Stdout),
        writer.clone(),
        || runtime.block_on(server.dispatch_bytes(request_bytes, false)),
    )?;

    assert_eq!(response["error"]["kind"], "unknown_op");
    let output = writer.output();
    let lines = json_lines(&output);
    assert!(
        lines.iter().any(|line| line["level"] == "INFO"),
        "expected JSON info line in {output}"
    );
    assert!(
        output.contains("\"daemon.request\""),
        "daemon.request span should be present in {output}"
    );
    assert!(
        output.contains("time.busy") && output.contains("time.idle"),
        "span close timing fields should be present in {output}"
    );
    Ok(())
}

#[test]
fn local_json_telemetry_applies_env_filter_expression() -> Result<()> {
    let writer = CaptureWriter::default();
    let mut cfg = local_json_telemetry(TelemetryOutputStream::Stdout);
    cfg.level = "sandbox_daemon=debug".to_owned();

    crate::telemetry::with_test_json_subscriber(&cfg, writer.clone(), || {
        tracing::debug!(target: "sandbox_daemon", "target visible");
        tracing::debug!(target: "sandbox_runtime", "target hidden");
    })?;

    let output = writer.output();
    assert!(
        output.contains("target visible"),
        "target-specific debug event should be captured: {output}"
    );
    assert!(
        !output.contains("target hidden"),
        "non-matching target should be filtered out: {output}"
    );
    Ok(())
}

#[test]
fn daemon_request_span_records_dynamic_sandbox_id_and_request_id() -> Result<()> {
    let writer = CaptureWriter::default();
    let runtime = Runtime::new()?;
    let server = test_server(Some("dynamic-sbox"));
    let request = json!({
        "op": "unknown_op_OPERATION_SECRET_SENTINEL",
        "request_id": "REQUEST_ID_SECRET_SENTINEL",
        "scope": { "kind": "sandbox", "sandbox_id": "scope-sbox" },
        "args": {}
    });
    let request_bytes = serde_json::to_vec(&request)?;

    crate::telemetry::with_test_json_subscriber(
        &local_json_telemetry(TelemetryOutputStream::Stderr),
        writer.clone(),
        || runtime.block_on(server.dispatch_bytes(request_bytes, false)),
    )?;

    let output = writer.output();
    assert!(output.contains("dynamic-sbox"), "{output}");
    assert!(output.contains("REQUEST_ID_SECRET_SENTINEL"), "{output}");
    assert!(output.contains("unknown_op"), "{output}");
    assert!(output.contains("sandbox"), "{output}");
    assert!(
        !output.contains("unknown_op_OPERATION_SECRET_SENTINEL"),
        "raw unknown operation must not appear in telemetry: {output}"
    );
    Ok(())
}

#[test]
fn pre_decode_failure_telemetry_is_sanitized() -> Result<()> {
    let writer = CaptureWriter::default();
    let runtime = Runtime::new()?;
    let server = test_server(Some("dynamic-sbox"));
    let raw = br#"{"op":"exec_command","_sandbox_daemon_auth_token":"SECRET_AUTH_SENTINEL""#.to_vec();

    let response = crate::telemetry::with_test_json_subscriber(
        &local_json_telemetry(TelemetryOutputStream::Stdout),
        writer.clone(),
        || runtime.block_on(server.dispatch_bytes(raw, true)),
    )?;

    assert_eq!(response["error"]["kind"], "bad_json");
    let output = writer.output();
    assert!(output.contains("bad_json"), "{output}");
    assert!(
        !output.contains("SECRET_AUTH_SENTINEL"),
        "raw auth-like payload must not appear in telemetry: {output}"
    );
    assert!(
        !output.contains("_sandbox_daemon_auth_token"),
        "auth field names from raw payload must not appear in telemetry: {output}"
    );
    Ok(())
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
    let cfg = telemetry_section(
        r#"
server:
  socket_path: /eos/runtime/daemon/runtime.sock
  pid_path: /eos/runtime/daemon/runtime.pid
  max_worker_threads: 2
"#,
    );

    assert_eq!(cfg, TelemetryConfig::default());
    cfg.validate()
        .expect("omitted telemetry defaults to disabled config");
}

#[test]
fn telemetry_loads_from_prd_config_document() {
    let config_path = sandbox_config::ConfigPath::prd().expect("prd config path resolves");
    let doc = sandbox_config::load_path(config_path.as_path()).expect("prd config loads");

    let cfg = crate::telemetry::from_config_document(&doc).expect("daemon telemetry deserializes");

    assert_eq!(cfg, TelemetryConfig::default());
    cfg.validate().expect("prd telemetry validates");
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
fn telemetry_otlp_accepts_http_sink() {
    let cfg = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: otlp
  endpoint: http://collector:4318
  protocol: http
  timeout_ms: 1000
  queue_size: 2048
"#,
    );

    assert!(matches!(
        cfg.sink,
        Some(TelemetrySink::Otlp {
            protocol: OtlpProtocol::Http,
            timeout_ms: 1000,
            queue_size: 2048,
            ..
        })
    ));
    cfg.validate()
        .expect("otlp http/protobuf telemetry validates");
}

#[test]
fn telemetry_otlp_http_endpoint_is_normalized_per_signal() {
    assert_eq!(
        crate::telemetry::otlp_http_signal_endpoint("http://collector:4318", "/v1/traces"),
        "http://collector:4318/v1/traces"
    );
    assert_eq!(
        crate::telemetry::otlp_http_signal_endpoint("http://collector:4318/", "/v1/metrics"),
        "http://collector:4318/v1/metrics"
    );
    assert_eq!(
        crate::telemetry::otlp_http_signal_endpoint(
            "http://collector:4318/v1/traces",
            "/v1/metrics"
        ),
        "http://collector:4318/v1/metrics"
    );
}

#[test]
fn telemetry_accepts_env_filter_level_expression() {
    let cfg = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: sandbox_daemon=debug,sandbox_runtime=info
sink:
  kind: local_json
  stream: stdout
"#,
    );

    cfg.validate()
        .expect("env-filter telemetry level validates");
}

#[test]
fn telemetry_metrics_config_requires_otlp_sink_when_enabled() {
    let cfg = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: otlp
  endpoint: http://collector:4318
  protocol: http
  timeout_ms: 1000
  queue_size: 2048
metrics:
  enabled: true
  export_interval_ms: 5000
  cgroup_samples_enabled: true
"#,
    );

    assert_eq!(
        cfg.metrics,
        Some(TelemetryMetricsConfig {
            enabled: true,
            export_interval_ms: 5000,
            cgroup_samples_enabled: true,
        })
    );
    cfg.validate().expect("otlp metrics config validates");

    let local_json = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: local_json
  stream: stdout
metrics:
  enabled: true
  export_interval_ms: 5000
  cgroup_samples_enabled: true
"#,
    );
    let err = local_json
        .validate()
        .expect_err("local json metrics are rejected");
    assert_eq!(err.field, "daemon.telemetry.metrics");
}

#[test]
fn telemetry_metrics_config_rejects_invalid_interval_and_disabled_parent() {
    let zero_interval = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: otlp
  endpoint: http://collector:4318
  protocol: http
  timeout_ms: 1000
  queue_size: 2048
metrics:
  enabled: true
  export_interval_ms: 0
  cgroup_samples_enabled: true
"#,
    );
    assert_eq!(
        zero_interval
            .validate()
            .expect_err("zero metrics interval rejected")
            .field,
        "daemon.telemetry.metrics.export_interval_ms"
    );

    let disabled_parent = telemetry_config(
        r#"
enabled: false
service_name: sandbox-daemon
level: info
metrics:
  enabled: true
  export_interval_ms: 1000
  cgroup_samples_enabled: false
"#,
    );
    assert_eq!(
        disabled_parent
            .validate()
            .expect_err("metrics require enabled telemetry")
            .field,
        "daemon.telemetry.metrics.enabled"
    );
}

#[test]
fn telemetry_rejects_invalid_otlp_settings() {
    let missing_endpoint = telemetry_deserialize_error(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: otlp
  protocol: http
  timeout_ms: 1000
  queue_size: 2048
"#,
    );
    assert!(
        missing_endpoint.contains("endpoint"),
        "unexpected error: {missing_endpoint}"
    );

    let zero_timeout = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: otlp
  endpoint: http://collector:4318
  protocol: http
  timeout_ms: 0
  queue_size: 2048
"#,
    );
    assert_eq!(
        zero_timeout.validate().expect_err("zero timeout rejected").field,
        "daemon.telemetry.sink.timeout_ms"
    );

    let zero_queue = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: otlp
  endpoint: http://collector:4318
  protocol: http
  timeout_ms: 1000
  queue_size: 0
"#,
    );
    assert_eq!(
        zero_queue.validate().expect_err("zero queue rejected").field,
        "daemon.telemetry.sink.queue_size"
    );
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
  kind: file
  path: /tmp/sandbox-daemon-telemetry.json
"#,
    );

    assert!(
        err.contains("file") || err.contains("kind"),
        "unexpected error: {err}"
    );
}

#[test]
fn telemetry_rejects_multiple_sink_list() {
    let err = telemetry_deserialize_error(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  - kind: local_json
    stream: stderr
  - kind: otlp
    endpoint: http://collector:4318
    protocol: http
    timeout_ms: 1000
    queue_size: 2048
"#,
    );

    assert!(
        err.contains("sequence") || err.contains("sink"),
        "unexpected error: {err}"
    );
}

#[test]
fn telemetry_rejects_unsupported_otlp_protocol() {
    let cfg = telemetry_config(
        r#"
enabled: true
service_name: sandbox-daemon
level: info
sink:
  kind: otlp
  endpoint: http://collector:4318
  protocol: grpc
  timeout_ms: 1000
  queue_size: 2048
"#,
    );

    let err = cfg.validate().expect_err("grpc protocol rejected");

    assert_eq!(err.field, "daemon.telemetry.sink.protocol");
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
fn telemetry_otlp_requires_dynamic_sandbox_id() {
    let cfg = otlp_telemetry("http://collector:4318", 1000, 2048);

    let err = cfg
        .validate_for_daemon_startup(DaemonServeMode::Spawn, None)
        .expect_err("otlp telemetry requires sandbox identity");
    assert_eq!(err.field, "daemon.telemetry.sink");

    cfg.validate_for_daemon_startup(DaemonServeMode::Spawn, Some("sbox-1"))
        .expect("otlp telemetry accepts dynamic sandbox identity");
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

#[test]
fn otlp_resource_contains_only_daemon_and_sandbox_identity() {
    let resource = crate::telemetry::otlp_resource("sandbox-daemon", "sbox-1");

    assert_eq!(
        resource_value(&resource, "service.name").as_deref(),
        Some("sandbox-daemon")
    );
    assert_eq!(
        resource_value(&resource, "service.instance.id").as_deref(),
        Some("sbox-1")
    );
    assert_eq!(
        resource_value(&resource, "sandbox.id").as_deref(),
        Some("sbox-1")
    );
    for forbidden in [
        "request_id",
        "command_id",
        "command_session_id",
        "workspace_session_id",
        "cgroup_path",
        "workspace_root",
        "root_hash",
        "error",
    ] {
        assert!(
            resource.get(&Key::from_static_str(forbidden)).is_none(),
            "resource must not include high-cardinality attribute {forbidden}"
        );
    }
}

#[test]
fn otlp_unreachable_collector_does_not_change_protocol_response() -> Result<()> {
    let runtime = Runtime::new()?;
    let server = test_server(Some("sbox-otlp"));
    let request = json!({
        "op": "unknown_op",
        "request_id": "req-otlp",
        "scope": { "kind": "sandbox", "sandbox_id": "scope-sbox" },
        "args": {}
    });
    let request_bytes = serde_json::to_vec(&request)?;
    let mut guard = crate::telemetry::install(
        &otlp_telemetry("http://127.0.0.1:9", 1, 1),
        Some("sbox-otlp"),
    )?;

    let response = runtime.block_on(server.dispatch_bytes(request_bytes, false));

    assert_eq!(response["error"]["kind"], "unknown_op");
    let _ = guard.shutdown();
    Ok(())
}

#[test]
fn otlp_queue_full_drop_does_not_block_protocol_responses() -> Result<()> {
    let runtime = Runtime::new()?;
    let exporter_state = Arc::new(BlockingExporterState::new());
    let processor = BatchSpanProcessor::builder(BlockingExporter {
        state: Arc::clone(&exporter_state),
    })
    .with_batch_config(
        BatchConfigBuilder::default()
            .with_max_queue_size(1)
            .with_max_export_batch_size(1)
            .with_scheduled_delay(Duration::from_secs(60))
            .build(),
    )
    .build();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(processor)
        .build();
    let tracer = provider.tracer("sandbox-daemon");
    let subscriber = tracing_subscriber::registry().with(
        tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
    );
    let server = test_server(Some("sbox-queue-drop"));

    let run_result = tracing::subscriber::with_default(subscriber, || -> Result<()> {
        let first_response = runtime.block_on(server.dispatch_bytes(
            unknown_operation_request_bytes("req-queue-prime"),
            false,
        ));
        anyhow::ensure!(
            first_response["error"]["kind"] == "unknown_op",
            "unexpected prime response: {first_response}"
        );
        anyhow::ensure!(
            exporter_state.wait_for_started(Duration::from_secs(1)),
            "blocking exporter did not receive the priming span"
        );

        runtime.block_on(async {
            for index in 0..32 {
                let request_id = format!("req-queue-drop-{index}");
                let response = match timeout(
                    Duration::from_millis(100),
                    server.dispatch_bytes(unknown_operation_request_bytes(&request_id), false),
                )
                .await
                {
                    Ok(response) => response,
                    Err(_) => {
                        anyhow::bail!(
                            "queue-full/drop path blocked daemon response for {request_id}"
                        );
                    }
                };
                anyhow::ensure!(
                    response["error"]["kind"] == "unknown_op",
                    "unexpected response while queue was full: {response}"
                );
            }
            Ok(())
        })
    });

    exporter_state.release();
    let mut guard =
        crate::telemetry::TelemetryGuard::from_provider_for_test(provider, Duration::from_secs(1));
    let shutdown_result = guard.shutdown();

    run_result?;
    shutdown_result.expect("blocked exporter shuts down after release");
    Ok(())
}

#[test]
fn telemetry_guard_shutdown_calls_provider() {
    let shutdown_called = Arc::new(AtomicBool::new(false));
    let exporter = ShutdownExporter {
        shutdown_called: Arc::clone(&shutdown_called),
    };
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .build();
    let mut guard =
        crate::telemetry::TelemetryGuard::from_provider_for_test(provider, Duration::from_millis(5));

    guard.shutdown().expect("provider shutdown succeeds");

    assert!(shutdown_called.load(Ordering::SeqCst));
}

#[test]
fn telemetry_guard_shutdown_surfaces_bounded_provider_error() {
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(FailingShutdownExporter {
            message: "shutdown flush failed ".repeat(80),
        })
        .build();
    let mut guard =
        crate::telemetry::TelemetryGuard::from_provider_for_test(provider, Duration::from_millis(5));

    let err = guard
        .shutdown()
        .expect_err("provider shutdown failure is surfaced");

    let crate::telemetry::TelemetryShutdownError::Provider(message) = err;
    assert!(
        message.contains("shutdown flush failed"),
        "shutdown error should include provider context: {message}"
    );
    assert!(
        message.len() <= 515,
        "shutdown error should be bounded: len={} message={message}",
        message.len()
    );
}

#[derive(Clone, Default)]
struct CaptureWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl CaptureWriter {
    fn output(&self) -> String {
        String::from_utf8(self.bytes.lock().expect("capture lock").clone())
            .expect("telemetry is utf8")
    }
}

impl<'writer> MakeWriter<'writer> for CaptureWriter {
    type Writer = Capture;

    fn make_writer(&'writer self) -> Self::Writer {
        Capture {
            bytes: Arc::clone(&self.bytes),
        }
    }
}

struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn local_json_telemetry(stream: TelemetryOutputStream) -> TelemetryConfig {
    TelemetryConfig {
        enabled: true,
        service_name: "sandbox-daemon".to_owned(),
        level: "info".to_owned(),
        sink: Some(TelemetrySink::LocalJson { stream }),
        metrics: None,
    }
}

fn otlp_telemetry(endpoint: &str, timeout_ms: u64, queue_size: usize) -> TelemetryConfig {
    TelemetryConfig {
        enabled: true,
        service_name: "sandbox-daemon".to_owned(),
        level: "info".to_owned(),
        sink: Some(TelemetrySink::Otlp {
            endpoint: endpoint.to_owned(),
            protocol: OtlpProtocol::Http,
            timeout_ms,
            queue_size,
        }),
        metrics: None,
    }
}

fn resource_value(resource: &opentelemetry_sdk::Resource, key: &'static str) -> Option<String> {
    resource
        .get(&Key::from_static_str(key))
        .map(|value| value.to_string())
}

fn telemetry_section(yaml: &str) -> TelemetryConfig {
    #[derive(serde::Deserialize)]
    struct DaemonTelemetrySection {
        #[serde(default)]
        telemetry: TelemetryConfig,
    }
    serde_yaml_ng::from_str::<DaemonTelemetrySection>(yaml)
        .expect("daemon telemetry section deserializes")
        .telemetry
}

fn telemetry_config(yaml: &str) -> TelemetryConfig {
    serde_yaml_ng::from_str(yaml).expect("telemetry config deserializes")
}

fn telemetry_deserialize_error(yaml: &str) -> String {
    serde_yaml_ng::from_str::<TelemetryConfig>(yaml)
        .expect_err("telemetry config should fail to deserialize")
        .to_string()
}

fn json_lines(output: &str) -> Vec<Value> {
    output
        .lines()
        .map(|line| serde_json::from_str(line).expect("telemetry line is json"))
        .collect()
}

fn unknown_operation_request_bytes(request_id: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "op": "unknown_op",
        "request_id": request_id,
        "scope": { "kind": "sandbox", "sandbox_id": "scope-sbox" },
        "args": {}
    }))
    .expect("unknown operation request serializes")
}

fn test_server(sandbox_id: Option<&str>) -> SandboxDaemonServer {
    SandboxDaemonServer::new(
        ServerConfig {
            socket_path: PathBuf::from("/tmp/sandbox-daemon-test.sock"),
            pid_path: PathBuf::from("/tmp/sandbox-daemon-test.pid"),
            tcp_host: None,
            tcp_port: None,
            auth_token: Some("configured-token".to_owned()),
            sandbox_id: sandbox_id.map(str::to_owned),
        },
        Arc::new(test_operations()),
    )
}

fn test_operations() -> sandbox_runtime::SandboxRuntimeOperations {
    let base = temp_root("sandbox-daemon-telemetry");
    let workspace_root = base.join("workspace");
    let layer_stack_root = base.join("layer-stack");
    std::fs::create_dir_all(&workspace_root).expect("create telemetry test workspace");
    sandbox_runtime_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, false)
        .expect("build telemetry test layerstack workspace base");

    sandbox_runtime::SandboxRuntimeOperations::from_config(sandbox_runtime::SandboxRuntimeConfig {
        workspace: sandbox_runtime::WorkspaceRuntimeConfig {
            workspace_root,
            layer_stack_root,
            scratch_root: base.join("workspace-scratch"),
            caps: sandbox_runtime::WorkspaceResourceCaps {
                ttl_s: 60.0,
                total_cap: 2,
                upperdir_bytes: 1024 * 1024,
                memavail_fraction: 0.5,
                setup_timeout_s: 1.0,
                exit_grace_s: 0.1,
                rfc1918_egress: sandbox_runtime::Rfc1918Egress::Allow,
            },
        },
        command: sandbox_runtime::CommandRuntimeConfig {
            scratch_root: base.join("command-scratch"),
        },
        cgroup_monitor: sandbox_runtime::CgroupMonitorRuntimeConfig {
            enabled: false,
            sample_interval_ms: 1000,
            retained_samples_per_target: 10,
            include_pids: false,
            include_pressure: false,
            include_disk: false,
        },
    })
}

#[derive(Debug)]
struct BlockingExporter {
    state: Arc<BlockingExporterState>,
}

#[derive(Debug)]
struct BlockingExporterState {
    started: (Mutex<usize>, Condvar),
    released: (Mutex<bool>, Condvar),
}

impl BlockingExporterState {
    fn new() -> Self {
        Self {
            started: (Mutex::new(0), Condvar::new()),
            released: (Mutex::new(false), Condvar::new()),
        }
    }

    fn mark_started(&self) {
        let (lock, condvar) = &self.started;
        let mut count = lock.lock().expect("export start lock");
        *count += 1;
        condvar.notify_all();
    }

    fn wait_for_started(&self, timeout: Duration) -> bool {
        let (lock, condvar) = &self.started;
        let count = lock.lock().expect("export start lock");
        let (count, _) = condvar
            .wait_timeout_while(count, timeout, |count| *count == 0)
            .expect("export start condvar");
        *count > 0
    }

    fn wait_until_released(&self) {
        let (lock, condvar) = &self.released;
        let mut released = lock.lock().expect("export release lock");
        while !*released {
            released = condvar.wait(released).expect("export release condvar");
        }
    }

    fn release(&self) {
        let (lock, condvar) = &self.released;
        let mut released = lock.lock().expect("export release lock");
        *released = true;
        condvar.notify_all();
    }
}

impl SpanExporter for BlockingExporter {
    async fn export(&self, _batch: Vec<SpanData>) -> OTelSdkResult {
        self.state.mark_started();
        self.state.wait_until_released();
        Ok(())
    }
}

#[derive(Debug)]
struct ShutdownExporter {
    shutdown_called: Arc<AtomicBool>,
}

impl SpanExporter for ShutdownExporter {
    async fn export(&self, _batch: Vec<SpanData>) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        self.shutdown_called.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Debug)]
struct FailingShutdownExporter {
    message: String,
}

impl SpanExporter for FailingShutdownExporter {
    async fn export(&self, _batch: Vec<SpanData>) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        Err(OTelSdkError::InternalFailure(self.message.clone()))
    }
}

fn temp_root(label: &str) -> PathBuf {
    static NEXT_TEMP_ROOT_ID: AtomicU64 = AtomicU64::new(1);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    let unique_id = NEXT_TEMP_ROOT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "{label}-{}-{nanos}-{unique_id}",
        std::process::id()
    ))
}
