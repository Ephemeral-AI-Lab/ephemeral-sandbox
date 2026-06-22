mod support;

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use sandbox_protocol::{OperationScope, Request};
use sandbox_runtime::command::{ExecCommandInput, ReadCommandLinesInput, WriteCommandStdinInput};
use sandbox_runtime_command::yield_wait_loop::WaitOutcome;
use sandbox_runtime_workspace::WorkspaceProfile;
use serde_json::json;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use support::{
    build_services_with_launch_driver, create_request, success_exit, workspace_handle,
    FakeLaunchDriver, FakeWorkspaceService,
};

#[test]
fn command_trace_spans_omit_sensitive_values() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Running(
        "STDOUT_SECRET_SENTINEL initial\n".to_owned(),
    ));
    launch_driver.push_outcome(WaitOutcome::Completed(success_exit(
        "STDERR_STDOUT_SECRET_SENTINEL final\n",
    )));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    fake.push_create_result(Ok(workspace_handle(
        "workspace-secret",
        "lease-secret",
        PathBuf::from("/workspace/PATH_SECRET_SENTINEL"),
        WorkspaceProfile::HostCompatible,
    )));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;

    let traces = capture_traces(|| {
        let command_session_id = env
            .command
            .exec_command(ExecCommandInput {
                workspace_session_id: Some(workspace_session_id),
                cmd: "printf COMMAND_SECRET_SENTINEL && export TOKEN=AUTH_ENV_SECRET_SENTINEL"
                    .to_owned(),
                timeout_ms: Some(2500),
                yield_time_ms: Some(1),
            })
            .expect("command starts")
            .command_session_id
            .expect("running command id returned");

        env.command
            .write_command_stdin(WriteCommandStdinInput {
                command_session_id: command_session_id.clone(),
                stdin: "STDIN_SECRET_SENTINEL\n".to_owned(),
                yield_time_ms: Some(1),
            })
            .expect("stdin write completes command");

        env.command
            .read_command_lines(ReadCommandLinesInput {
                command_session_id,
                start_offset: Some(0),
                limit: Some(10),
            })
            .expect("completed command output remains readable");
    });

    for expected in [
        "runtime.exec_command",
        "command.spawn",
        "command.wait_initial_yield",
        "runtime.write_command_stdin",
        "command.finalize",
        "runtime.read_command_lines",
    ] {
        assert!(traces.contains(expected), "missing {expected} in {traces}");
    }
    for forbidden in [
        "COMMAND_SECRET_SENTINEL",
        "AUTH_ENV_SECRET_SENTINEL",
        "STDIN_SECRET_SENTINEL",
        "STDOUT_SECRET_SENTINEL",
        "STDERR_STDOUT_SECRET_SENTINEL",
        "PATH_SECRET_SENTINEL",
        "/workspace/",
        "/lower/one",
        "transcript.log",
        "lease-secret",
    ] {
        assert!(
            !traces.contains(forbidden),
            "forbidden value {forbidden} appeared in traces: {traces}"
        );
    }
}

#[test]
fn public_cgroup_read_operations_emit_no_trace_spans() {
    let operations = test_operations();

    let traces = capture_traces(|| {
        let inspect = Request::new(
            "inspect_cgroup_monitor",
            "req-inspect",
            OperationScope::sandbox("scope-sbox"),
            json!({
                "workspace_session_id": "workspace-secret",
                "command_session_id": "command-secret",
            }),
        );
        let read = Request::new(
            "read_cgroup_monitor_samples",
            "req-read",
            OperationScope::sandbox("scope-sbox"),
            json!({
                "workspace_session_id": "workspace-secret",
                "command_session_id": "command-secret",
                "limit": 5,
            }),
        );

        let _ = sandbox_runtime::dispatch_operation(&operations, &inspect);
        let _ = sandbox_runtime::dispatch_operation(&operations, &read);
    });

    assert!(
        traces.trim().is_empty(),
        "public cgroup read ops must not emit trace spans/events: {traces}"
    );
}

#[derive(Clone)]
struct TraceCapture {
    next_id: Arc<AtomicU64>,
    records: Arc<Mutex<Vec<String>>>,
}

impl Default for TraceCapture {
    fn default() -> Self {
        Self {
            next_id: Arc::new(AtomicU64::new(1)),
            records: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl TraceCapture {
    fn output(&self) -> String {
        self.records.lock().expect("trace lock").join("\n")
    }

    fn push(&self, line: String) {
        self.records.lock().expect("trace lock").push(line);
    }
}

impl Subscriber for TraceCapture {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut visitor = TextVisitor::new(format!("span {}", attrs.metadata().name()));
        attrs.record(&mut visitor);
        self.push(visitor.finish());
        Id::from_u64(id)
    }

    fn record(&self, _span: &Id, values: &Record<'_>) {
        let mut visitor = TextVisitor::new("record".to_owned());
        values.record(&mut visitor);
        self.push(visitor.finish());
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = TextVisitor::new(format!("event {}", event.metadata().name()));
        event.record(&mut visitor);
        self.push(visitor.finish());
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

struct TextVisitor {
    line: String,
}

impl TextVisitor {
    fn new(line: String) -> Self {
        Self { line }
    }

    fn finish(self) -> String {
        self.line
    }

    fn push_value(&mut self, field: &Field, value: impl fmt::Display) {
        use std::fmt::Write as _;

        let _ = write!(self.line, " {}={value}", field.name());
    }
}

impl Visit for TextVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push_value(field, format_args!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push_value(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push_value(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push_value(field, value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push_value(field, value);
    }
}

fn capture_traces(run: impl FnOnce()) -> String {
    let capture = TraceCapture::default();
    let reader = capture.clone();
    tracing::subscriber::with_default(capture, run);
    reader.output()
}

fn test_operations() -> sandbox_runtime::SandboxRuntimeOperations {
    let base = temp_root("trace-cgroup");
    let workspace_root = base.join("workspace");
    let layer_stack_root = base.join("layer-stack");
    std::fs::create_dir_all(&workspace_root).expect("create trace cgroup workspace");
    sandbox_runtime_layerstack::build_workspace_base(&layer_stack_root, &workspace_root, false)
        .expect("build trace cgroup layerstack workspace base");

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
