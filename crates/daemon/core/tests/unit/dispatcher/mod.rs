use std::future;
use std::path::Path;
use std::process::Child;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use config::configs::daemon::PluginRuntimeConfig;
use config::configs::isolated_workspace::IsolatedWorkspaceConfig;
use namespace::protocol::{RunRequest, RunResult};

use crate::RuntimeServices;
use workspace::{LaunchError, NsRunnerLauncher};

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn upperdir_tree_resource_timings_capture_bounded_payload() -> TestResult {
    let fixture = Fixture::new("upperdir_tree_stats")?;
    let upperdir = fixture.base.join("upperdir");
    std::fs::create_dir_all(upperdir.join("nested"))?;
    std::fs::write(upperdir.join("nested/payload.bin"), vec![7_u8; 4096])?;

    let manifest = LayerStack::open(fixture.root.clone())?.read_active_manifest()?;
    let mut timings = resource_timings(&manifest, 1);
    let upperdir_stats = workspace::TreeResourceStats::collect(&upperdir);
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &TreeResourceStats::from_ephemeral(&upperdir_stats),
    );

    assert!(
        !timings.contains_key("resource.command_exec.workspace_tree_bytes"),
        "unwalked trees must not fabricate zero stats"
    );
    assert_eq!(
        timing_f64_value(&timings, "resource.command_exec.upperdir_tree_exists"),
        1.0
    );
    assert!(timing_f64_value(&timings, "resource.command_exec.upperdir_tree_bytes") >= 4096.0);
    assert_eq!(
        timing_f64_value(&timings, "resource.command_exec.upperdir_tree_truncated"),
        0.0
    );

    let truncated_upperdir_stats =
        workspace::TreeResourceStats::collect_with_entry_limit(&upperdir, 1);
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &TreeResourceStats::from_ephemeral(&truncated_upperdir_stats),
    );
    assert_eq!(
        timing_f64_value(&timings, "resource.command_exec.upperdir_tree_truncated"),
        1.0
    );
    Ok(())
}

#[test]
fn builtin_table_routes_commit_to_workspace() {
    let response = dispatch(&Request {
        op: "sandbox.checkpoint.commit_to_workspace".to_owned(),
        invocation_id: "commit-to-workspace-route-test".to_owned(),
        args: json!({}),
    });

    assert_eq!(response["status"], json!("error"));
    assert_ne!(response["error"]["kind"], json!("unknown_op"));
    assert_eq!(response["error"]["kind"], json!("invalid_request"));
    assert!(response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("layer_stack_root is required"));
}

#[test]
fn builtin_table_routes_commit_to_git() {
    let response = dispatch(&Request {
        op: "sandbox.checkpoint.commit_to_git".to_owned(),
        invocation_id: "commit-to-git-route-test".to_owned(),
        args: json!({}),
    });

    assert_eq!(response["status"], json!("error"));
    assert_ne!(response["error"]["kind"], json!("unknown_op"));
    assert_eq!(response["error"]["kind"], json!("invalid_request"));
    assert!(response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("layer_stack_root is required"));
}

#[test]
fn host_served_builtin_reaches_boundary_error_without_fallback() {
    let response = dispatch(&Request {
        op: "host.sandbox.acquire".to_owned(),
        invocation_id: "host-served-boundary".to_owned(),
        args: json!({}),
    });

    assert_eq!(response["status"], json!("error"));
    assert_eq!(response["error"]["kind"], json!("forbidden"));
    assert!(response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("served by the host gateway"));
    assert_eq!(
        response["error"]["details"]["fields"]["served_by"],
        json!("host")
    );
}

#[test]
fn builtin_parse_gate_preserves_error_response_channel() {
    let response = dispatch(&Request {
        op: "sandbox.file.edit".to_owned(),
        invocation_id: "edit-parse-gate".to_owned(),
        args: json!({}),
    });

    assert_eq!(response["status"], json!("error"));
    assert_eq!(response["error"]["kind"], json!("invalid_request"));
    assert_eq!(
        response["error"]["message"],
        json!("invalid request: edits must be a list")
    );
    assert_eq!(response["meta"]["warnings"], json!([]));
}

#[test]
fn file_write_dispatch_returns_status_envelope() -> TestResult {
    let fixture = Fixture::new("file-write-envelope")?;
    let workspace = fixture.base.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(
        fixture.root.join("workspace.json"),
        serde_json::to_vec_pretty(&json!({
            "workspace_root": workspace,
            "layer_stack_root": fixture.root,
            "active_manifest_version": 1,
            "active_root_hash": "root",
            "base_manifest_version": 1,
            "base_root_hash": "base",
        }))?,
    )?;
    let response = dispatch(&Request {
        op: "sandbox.file.write".to_owned(),
        invocation_id: "file-write-envelope".to_owned(),
        args: json!({
            "layer_stack_root": fixture.root.to_string_lossy(),
            "caller_id": "caller-file-write-envelope",
            "path": "envelope.txt",
            "content": "enveloped\n",
            "overwrite": true,
        }),
    });

    assert_eq!(response["status"], json!("ok"), "{response}");
    assert_eq!(
        response["result"]["status"],
        json!("committed"),
        "{response}"
    );
    assert_eq!(response["result"]["success"], json!(true), "{response}");
    assert!(response.get("meta").is_some(), "{response}");
    Ok(())
}

#[test]
fn command_count_dispatch_returns_status_envelope() {
    let services = test_services();
    let response = dispatch_with_context(
        &Request {
            op: "sandbox.command.count".to_owned(),
            invocation_id: "command-count-envelope".to_owned(),
            args: json!({"caller_id": "caller-command-count-envelope"}),
        },
        DispatchContext::with_services(&services),
    );

    assert_eq!(response["status"], json!("ok"), "{response}");
    assert_eq!(response["result"]["success"], json!(true), "{response}");
    assert_eq!(
        response["result"]["caller_id"],
        json!("caller-command-count-envelope"),
        "{response}"
    );
    assert_eq!(response["result"]["count"], json!(0), "{response}");
    assert!(response.get("success").is_none(), "{response}");
    assert!(response.get("timings").is_none(), "{response}");
}

#[test]
fn builtin_parse_gate_preserves_refused_channel() {
    let response = dispatch(&Request {
        op: "sandbox.isolation.enter".to_owned(),
        invocation_id: "isolation-parse-gate".to_owned(),
        args: json!({}),
    });

    assert_eq!(response["status"], json!("rejected"));
    assert_eq!(response["error"]["kind"], json!("invalid_argument"));
    assert_eq!(response["error"]["message"], json!("caller_id is required"));
    assert_eq!(
        response["error"]["details"]["fields"],
        json!({"key": "caller_id"})
    );
    assert_eq!(response["meta"]["warnings"], json!([]));
}

#[test]
fn command_poll_parse_gate_preserves_id_first_error() {
    let response = dispatch(&Request {
        op: "sandbox.command.poll".to_owned(),
        invocation_id: "command-poll-parse-gate".to_owned(),
        args: json!({"last_n_lines": u64::MAX}),
    });

    assert_eq!(response["status"], json!("error"));
    assert_eq!(response["error"]["kind"], json!("invalid_request"));
    assert_eq!(
        response["error"]["message"],
        json!("invalid request: command_id is required")
    );
}

#[test]
fn dispatch_does_not_synthesize_a_parallel_meta() {
    // Meta (op, request_id, duration, steps) is owned solely by the span-derived
    // transport stamp. The dispatcher must not hand-maintain a parallel meta map,
    // so at the dispatch boundary meta carries only envelope defaults and there is
    // no synthetic "runtime.dispatch" step.
    let response = dispatch_with_context(
        &Request {
            op: "sandbox.call.heartbeat".to_owned(),
            invocation_id: "timings-test".to_owned(),
            args: json!({"invocation_ids": []}),
        },
        DispatchContext::empty(),
    );

    assert_eq!(response["status"], json!("ok"));
    assert_eq!(response["result"]["success"], json!(true));
    assert_eq!(
        response["meta"]["op"],
        json!(""),
        "dispatcher must not fill meta.op; the transport stamp owns it: {response}"
    );
    assert!(
        response["meta"]["steps"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "dispatcher must not synthesize a runtime.dispatch step: {response}"
    );
    assert!(response.get("timings").is_none(), "{response}");
}

#[test]
fn traced_dispatch_hot_path_stays_sub_millisecond_without_host_store() {
    let request = Request {
        op: "sandbox.call.heartbeat".to_owned(),
        invocation_id: "hot-path-request".to_owned(),
        args: json!({"invocation_ids": []}),
    };
    let trace = crate::wire::RequestTraceContext {
        trace_id: "trace-hot-path".to_owned(),
        request_id: "hot-path-request".to_owned(),
        parent_span_id: None,
        link_hints: Vec::new(),
        capture_budget_version: 1,
    };
    let facts = crate::trace::RequestTraceFacts {
        connection_id: "daemon-conn-hot-path".to_owned(),
        accepted_at_unix_ms: u64::MAX,
        listener_kind: "tcp",
        peer_addr: Some("127.0.0.1:51000".to_owned()),
        local_addr: Some("127.0.0.1:50000".to_owned()),
        is_tcp: true,
        request_bytes: 256,
        read_duration_us: 10,
        auth_required: true,
        auth_ok: true,
        protocol_version: Some(1),
    };

    let iterations = 128_u128;
    let started = Instant::now();
    for _ in 0..iterations {
        let response = dispatch_with_context(&request, DispatchContext::empty());
        let response =
            crate::trace::attach_request_sidecar(response, Some(&trace), &request.op, &facts);
        assert_eq!(response["status"], json!("ok"));
        assert_eq!(
            response["_trace_events"]["schema"],
            "eos.trace.v1.TraceBatch"
        );
        assert_eq!(response["_trace_events"]["encoding"], "base64+protobuf");
        assert_eq!(response["_trace_events"]["spool_pending"], false);
        assert!(response["_trace_events"]["data"].is_string());
    }
    let average_us = started.elapsed().as_micros() / iterations;
    assert!(
        average_us < 1_000,
        "traced dispatch averaged {average_us}us"
    );
}

#[tokio::test]
async fn cancel_waits_for_bounded_cleanup() -> TestResult {
    let registry = Arc::new(InFlightRegistry::new(300.0, 30.0));
    let task = tokio::spawn(future::pending::<()>());
    registry.register("cancel-target", task.abort_handle(), "caller-a", true);
    let cleanup_registry = Arc::clone(&registry);
    let cleanup_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        cleanup_registry.deregister("cancel-target");
    });

    let response = dispatch_with_context(
        &Request {
            op: "sandbox.call.cancel".to_owned(),
            invocation_id: "cancel-request".to_owned(),
            args: json!({"invocation_id": "cancel-target"}),
        },
        DispatchContext::with_invocation_registry(&registry),
    );

    cleanup_thread
        .join()
        .map_err(|_| "cleanup helper panicked")?;
    assert_eq!(response["status"], json!("ok"));
    assert_eq!(response["result"]["cancelled"], json!(true));
    assert_eq!(response["result"]["already_done"], json!(false));
    assert_eq!(response["result"]["cleanup_done"], json!(true));
    match task.await {
        Ok(()) => Err("expected cancelled task".into()),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(format!("expected cancellation, got {error}").into()),
    }
}

#[tokio::test]
async fn cancel_reports_started_blocking_invocation_as_not_cancelled() -> TestResult {
    let registry = Arc::new(InFlightRegistry::new(300.0, 30.0));
    let task = tokio::spawn(future::pending::<()>());
    registry.register_blocking(
        "blocking-target",
        task.abort_handle(),
        Arc::new(AtomicBool::new(true)),
        "caller-a",
        true,
    );

    let response = dispatch_with_context(
        &Request {
            op: "sandbox.call.cancel".to_owned(),
            invocation_id: "cancel-request".to_owned(),
            args: json!({"invocation_id": "blocking-target"}),
        },
        DispatchContext::with_invocation_registry(&registry),
    );

    assert_eq!(response["status"], json!("ok"));
    assert_eq!(response["result"]["cancelled"], json!(false));
    assert_eq!(response["result"]["already_done"], json!(false));
    assert_eq!(response["result"]["cleanup_done"], json!(false));

    task.abort();
    match task.await {
        Ok(()) => Err("expected cancelled task".into()),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(format!("expected cancellation, got {error}").into()),
    }
}

#[test]
fn internal_error_response_adds_error_id() {
    let response = error_response(
        ErrorKind::InternalError,
        "daemon invocation failed",
        json!({"op": "api.test.failure"}),
    );

    assert_eq!(response["status"], json!("error"));
    assert_eq!(response["error"]["kind"], json!("internal_error"));
    assert_eq!(
        response["error"]["details"]["fields"]["op"],
        json!("api.test.failure")
    );
    let Some(error_id) = response["error"]["error_id"].as_str() else {
        panic!("internal errors carry error.error_id");
    };
    assert_eq!(error_id.len(), 32);
    assert!(error_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(error_id.as_bytes()[12], b'4');
    assert!(matches!(error_id.as_bytes()[16], b'8' | b'9' | b'a' | b'b'));
}

fn timing_f64_value(timings: &serde_json::Map<String, Value>, key: &str) -> f64 {
    timings.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn test_services() -> RuntimeServices {
    RuntimeServices::new(
        PluginRuntimeConfig::default(),
        IsolatedWorkspaceConfig::default(),
        command::CommandConfig::default(),
        Arc::new(NoLaunch),
    )
}

struct NoLaunch;

impl NsRunnerLauncher for NoLaunch {
    fn run(&self, _request: &RunRequest) -> Result<RunResult, LaunchError> {
        Err(LaunchError::Failed(
            "dispatcher unit tests do not start ns-runner".to_owned(),
        ))
    }

    fn spawn_detached(
        &self,
        _request: &RunRequest,
        _stderr_path: &Path,
    ) -> Result<Child, LaunchError> {
        Err(LaunchError::Failed(
            "dispatcher unit tests do not start ns-runner".to_owned(),
        ))
    }

    fn remount_in(
        &self,
        _target_pid: u32,
        _request: &RunRequest,
        _timeout: Duration,
    ) -> Result<(), LaunchError> {
        Err(LaunchError::Failed(
            "dispatcher unit tests do not start ns-runner".to_owned(),
        ))
    }
}

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        Self::new_with_gitignores(label, &[])
    }

    /// Seed one base layer with a `.gitignore` per `(dir, contents)` entry
    /// (`""` = workspace root) so nested / depth-sensitive routing is testable.
    fn new_with_gitignores(label: &str, gitignores: &[(&str, &str)]) -> TestResult<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "eosd-occ-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        let layer = root.join("layers").join("B000001-base");
        std::fs::create_dir_all(&layer)?;
        std::fs::create_dir_all(root.join("staging"))?;
        std::fs::write(layer.join("README.md"), "# README\n")?;
        for (dir, contents) in gitignores {
            let target = if dir.is_empty() {
                layer.join(".gitignore")
            } else {
                layer.join(dir).join(".gitignore")
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, contents)?;
        }
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "version": 1,
                "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
            }))?,
        )?;
        Ok(Self { base, root })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}
