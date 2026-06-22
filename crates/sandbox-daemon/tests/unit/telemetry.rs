use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use sandbox_runtime_config::configs::daemon::{
    TelemetryConfig, TelemetryOutputStream, TelemetrySink,
};
use serde_json::{json, Value};
use tokio::runtime::Runtime;
use tracing_subscriber::fmt::MakeWriter;

use crate::server::{SandboxDaemonServer, ServerConfig};

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
fn daemon_request_span_records_dynamic_sandbox_id() -> Result<()> {
    let writer = CaptureWriter::default();
    let runtime = Runtime::new()?;
    let server = test_server(Some("dynamic-sbox"));
    let request = json!({
        "op": "unknown_op",
        "request_id": "req-sandbox-id",
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
    assert!(output.contains("req-sandbox-id"), "{output}");
    assert!(output.contains("unknown_op"), "{output}");
    assert!(output.contains("sandbox"), "{output}");
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
    }
}

fn json_lines(output: &str) -> Vec<Value> {
    output
        .lines()
        .map(|line| serde_json::from_str(line).expect("telemetry line is json"))
        .collect()
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

fn temp_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
}
