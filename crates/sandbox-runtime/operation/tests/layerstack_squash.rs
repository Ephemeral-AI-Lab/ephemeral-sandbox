//! Squash operation surface: registration without a CLI catalog entry, the
//! minimal output contract, singleflight faults, and the per-session
//! admission gate as the single serializer.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sandbox_observability_telemetry::record::{names, proc};
use sandbox_observability_telemetry::{
    sample_layerstack, Observer, ObserverConfig, RawFilter, Reader, Record, Sink, Span, SpanStatus,
    TraceContext, WalkBudget,
};
use sandbox_operation_contract::{OperationRequest, OperationScope, OperationScopeKind};
use sandbox_runtime::workspace_session::{SweptDisposition, WorkspaceSessionService};
use sandbox_runtime::SandboxRuntimeOperations;
use sandbox_runtime_layerstack::{manifest_root_hash, LayerChange, LayerPath, LayerStack};
use sandbox_runtime_workspace::NetworkProfile;
use sandbox_runtime_workspace::WorkspaceSessionId;
use serde_json::{json, Value};

mod support;
use support::FakeWorkspaceService;

fn squash_request() -> OperationRequest {
    squash_request_for("req-squash-test")
}

fn squash_request_for(request_id: &str) -> OperationRequest {
    OperationRequest::new(
        "squash_layerstack",
        request_id,
        OperationScope::sandbox("sbox-test"),
        json!({}),
    )
}

fn operations_with_real_layerstack() -> (SandboxRuntimeOperations, std::path::PathBuf) {
    let fake = Arc::new(FakeWorkspaceService::new());
    let layerstack =
        support::observed_layerstack_service(sandbox_observability_telemetry::Observer::disabled());
    let root = layerstack.layer_stack_root().to_path_buf();
    let services = support::build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        Arc::new(support::FakeLaunchDriver::new()),
        Arc::clone(&layerstack),
    );
    let operations = SandboxRuntimeOperations::new(
        services.command,
        services.workspace,
        layerstack,
        support::test_file_service(),
    );
    (operations, root)
}

fn publish(root: &std::path::Path, path: &str, content: &str) {
    let mut stack = LayerStack::open(root.to_path_buf()).expect("open stack");
    stack
        .publish_layer(&[LayerChange::Write {
            path: LayerPath::parse(path).expect("path"),
            content: content.as_bytes().to_vec(),
        }])
        .expect("publish");
}

#[test]
fn enabled_observer_records_exact_squash_tree_at_zero_live_sessions() {
    assert_enabled_squash_trace(0);
}

#[test]
fn enabled_observer_records_exact_squash_tree_at_nonzero_live_sessions() {
    assert_enabled_squash_trace(2);
}

#[test]
fn squash_layerstack_is_internal_and_absent_from_the_public_catalog() {
    assert!(sandbox_runtime::runtime_internal_handler_keys()
        .any(|key| { key == (OperationScopeKind::Sandbox, "squash_layerstack") }));
    let catalog = sandbox_operation_catalog::runtime::runtime_catalog();
    let encoded = sandbox_operation_contract::catalog_to_value(catalog).to_string();
    assert!(
        !encoded.contains("squash_layerstack"),
        "internal registration must keep the op out of every catalog surface"
    );
}

// Test 17: the result carries exactly manifest_version + squashed_blocks +
// swept_sessions (+ faulty_sessions only when non-empty); blocked blocks carry
// non-empty free-form reasons; nothing-to-squash is the state speaking for itself.
#[test]
fn squash_output_contract() {
    let (operations, root) = operations_with_real_layerstack();
    publish(&root, "a.txt", "1");
    publish(&root, "b.txt", "2");
    publish(&root, "c.txt", "3");

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    let object = value.as_object().expect("result object");
    assert_eq!(
        object.keys().collect::<Vec<_>>(),
        vec!["manifest_version", "squashed_blocks", "swept_sessions"],
        "no layers, no leases, no no_op, no faulty_sessions when empty"
    );
    assert_eq!(value["swept_sessions"], json!([]));
    assert_eq!(value["manifest_version"], json!(5));
    let blocks = value["squashed_blocks"].as_array().expect("blocks");
    assert_eq!(blocks.len(), 1);
    let block = blocks[0].as_object().expect("block object");
    assert!(block["squashed_layer_id"]
        .as_str()
        .expect("id")
        .starts_with('S'));
    assert_eq!(
        block["replaced_layer_ids"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(block["replaced_layers"], json!("reclaimed"));
    assert!(
        !block.contains_key("blocked_reasons"),
        "reclaimed blocks carry no reasons"
    );

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    assert_eq!(value["manifest_version"], json!(5));
    assert_eq!(
        value["squashed_blocks"].as_array().map(Vec::len),
        Some(0),
        "nothing to squash: empty blocks, no no_op flag"
    );
}

// A leased block reports non-empty blocked_reasons even when no live session
// maps to the pinning lease (the plan-window holder fallback).
#[test]
fn squash_reports_leased_blocks_with_reasons() {
    let (operations, root) = operations_with_real_layerstack();
    publish(&root, "a.txt", "1");
    publish(&root, "b.txt", "2");
    publish(&root, "c.txt", "3");
    let stack = LayerStack::open(root.clone()).expect("open stack");
    let lease = stack.acquire_snapshot("pinning-holder").expect("lease");

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    let blocks = value["squashed_blocks"].as_array().expect("blocks");
    assert_eq!(
        blocks.len(),
        1,
        "block forms below the lease's newest layer"
    );
    assert_eq!(blocks[0]["replaced_layers"], json!("leased"));
    let reasons = blocks[0]["blocked_reasons"].as_array().expect("reasons");
    assert!(
        !reasons.is_empty(),
        "blocked_reasons is non-empty whenever leased"
    );
    drop(lease);
}

// Singleflight: a second invocation while one is in flight fails cleanly as
// operation_failed — no dedicated error kind exists.
#[test]
fn squash_singleflight_faults_as_operation_failed() {
    let (operations, root) = operations_with_real_layerstack();
    publish(&root, "a.txt", "1");
    let mut stack = LayerStack::open(root).expect("open stack");
    let in_flight = stack.squash().expect("first squash holds the flight");

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    assert_eq!(value["error"]["kind"], json!("operation_failed"));
    assert!(value["error"]["message"]
        .as_str()
        .expect("message")
        .contains("already in flight"));
    drop(in_flight);

    let response = sandbox_runtime::dispatch_operation(&operations, &squash_request());
    let value = response.into_json_value();
    assert!(
        value.get("error").is_none(),
        "squash succeeds after the flight releases"
    );
}

// Test 9 (deterministic core): the per-session gate is the single
// serializer — a destroy parked inside the gate blocks a concurrent session
// file op until it completes; a remount of an unknown session is a silent
// skip that leaks nothing.
#[test]
fn admission_gate_serializes_destroy_against_file_ops() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(support::workspace_handle(
        "ws-gate",
        "lease-gate",
        std::env::temp_dir().join("squash-gate-ws"),
        NetworkProfile::Shared,
    )));
    let services = support::build_services(Arc::clone(&fake));
    let handler = services
        .workspace
        .create_workspace_session(support::create_request())
        .expect("create session");
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        support::observed_layerstack_service(sandbox_observability_telemetry::Observer::disabled()),
        support::test_file_service(),
    );

    let (entered, release) = fake.park_next_destroy();
    let destroy_ops = operations.clone();
    let destroy_id = handler.workspace_session_id.0.clone();
    let destroyer = std::thread::spawn(move || {
        sandbox_runtime::dispatch_operation(
            &destroy_ops,
            &OperationRequest::new(
                "destroy_workspace_session",
                "req-destroy-gate",
                OperationScope::sandbox("sbox-test"),
                json!({ "workspace_session_id": destroy_id }),
            ),
        )
    });
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("destroy reached the workspace hook while holding the gate");

    let file_op_workspace = Arc::clone(&services.workspace);
    let file_op_id = handler.workspace_session_id.clone();
    let file_op = std::thread::spawn(move || {
        file_op_workspace.run_file_op(
            &file_op_id,
            sandbox_runtime_workspace::FileRunnerOp::ReadFile {
                rel: "f.txt".to_owned(),
                max_bytes: 16,
            },
        )
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        fake.run_file_op_calls().is_empty(),
        "the file op must wait on the session gate while destroy holds it"
    );

    release.send(()).expect("release the parked destroy");
    let _ = destroyer.join().expect("destroy thread");
    let file_op_result = file_op.join().expect("file op thread");
    assert!(
        matches!(
            file_op_result,
            Err(sandbox_runtime::workspace_session::WorkspaceSessionError::NotFound { .. })
        ),
        "a file op racing the destroy resolves inside the gate and loses cleanly"
    );
    assert!(
        fake.run_file_op_calls().is_empty(),
        "the file op never runs against the destroyed session"
    );

    let swept = services
        .workspace
        .remount_session(&WorkspaceSessionId("ws-never-existed".to_owned()));
    assert_eq!(swept.disposition, SweptDisposition::SessionGone);
    assert_eq!(services.workspace.gate_entry_count(), 0);
}

fn assert_enabled_squash_trace(live_sessions: usize) {
    let trace_id = format!("req-squash-trace-{live_sessions}");
    let log = TempTraceLog::new(&trace_id);
    let observer = Observer::new(
        ObserverConfig {
            proc: proc::DAEMON,
            enabled: true,
        },
        Sink::new(
            log.path.clone(),
            sandbox_observability_telemetry::MAX_LINE_BYTES,
        ),
    );
    let (operations, root) = observed_operations(observer.clone(), live_sessions);
    publish(&root, "a.txt", "1");
    publish(&root, "b.txt", "2");
    publish(&root, "c.txt", "3");

    let response = observer.with_context(
        TraceContext {
            trace: Arc::from(trace_id.as_str()),
            parent: None,
        },
        || {
            let dispatch = observer.span(names::DAEMON_DISPATCH);
            dispatch.attr("op", "squash_layerstack");
            sandbox_runtime::dispatch_operation(&operations, &squash_request_for(&trace_id))
                .into_json_value()
        },
    );
    assert!(
        response.get("error").is_none(),
        "squash response: {response}"
    );
    assert_eq!(
        response["squashed_blocks"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        response["swept_sessions"].as_array().map(Vec::len),
        Some(live_sessions)
    );

    let reader = Reader::new(log.path.clone(), log.path.with_extension("absent"));
    let records = reader
        .raw(RawFilter {
            trace: Some(trace_id.clone()),
            ..RawFilter::default()
        })
        .into_iter()
        .map(|line| serde_json::from_str::<Record>(&line).expect("valid trace record"))
        .collect::<Vec<_>>();
    assert!(
        records
            .iter()
            .all(|record| matches!(record, Record::Span(_))),
        "squash trace contains spans only"
    );
    let spans = records
        .into_iter()
        .filter_map(|record| match record {
            Record::Span(span) => Some(span),
            Record::Event(_) | Record::Sample(_) => None,
        })
        .collect::<Vec<_>>();

    let mut actual_names = spans
        .iter()
        .map(|span| span.name.to_string())
        .collect::<Vec<_>>();
    actual_names.sort();
    let mut expected_names = vec![
        names::DAEMON_DISPATCH.to_owned(),
        names::LAYERSTACK_SQUASH.to_owned(),
        names::LAYERSTACK_SQUASH_PLAN.to_owned(),
        names::LAYERSTACK_SQUASH_FLATTEN.to_owned(),
        names::LAYERSTACK_SQUASH_COMMIT.to_owned(),
        names::LAYERSTACK_SQUASH_REMOUNT_SWEEP.to_owned(),
    ];
    expected_names.extend(std::iter::repeat_n(
        names::WORKSPACE_SESSION_REMOUNT.to_owned(),
        live_sessions,
    ));
    expected_names.sort();
    assert_eq!(
        actual_names, expected_names,
        "closed squash span vocabulary"
    );
    assert!(
        spans
            .iter()
            .all(|span| span.status == SpanStatus::Completed),
        "every classified squash span completes"
    );

    let dispatch = only_span(&spans, names::DAEMON_DISPATCH);
    let squash = only_span(&spans, names::LAYERSTACK_SQUASH);
    let plan = only_span(&spans, names::LAYERSTACK_SQUASH_PLAN);
    let flatten = only_span(&spans, names::LAYERSTACK_SQUASH_FLATTEN);
    let commit = only_span(&spans, names::LAYERSTACK_SQUASH_COMMIT);
    let sweep = only_span(&spans, names::LAYERSTACK_SQUASH_REMOUNT_SWEEP);

    assert_eq!(dispatch.parent, None);
    assert_eq!(dispatch.attrs, attrs(json!({ "op": "squash_layerstack" })));
    assert_eq!(squash.parent.as_deref(), Some(dispatch.span.as_str()));
    for phase in [plan, flatten, commit, sweep] {
        assert_eq!(phase.parent.as_deref(), Some(squash.span.as_str()));
    }
    assert!(plan.attrs.is_empty());
    assert!(flatten.attrs.is_empty());
    assert!(commit.attrs.is_empty());
    assert_eq!(
        sweep.attrs,
        attrs(json!({ "sessions": live_sessions, "width": 4 }))
    );

    let mut remounts = spans
        .iter()
        .filter(|span| span.name == names::WORKSPACE_SESSION_REMOUNT)
        .collect::<Vec<_>>();
    remounts.sort_by(|left, right| {
        left.attrs["workspace_session_id"]
            .as_str()
            .cmp(&right.attrs["workspace_session_id"].as_str())
    });
    for (index, remount) in remounts.iter().enumerate() {
        assert_eq!(remount.parent.as_deref(), Some(sweep.span.as_str()));
        assert_eq!(
            remount.attrs,
            attrs(json!({
                "workspace_session_id": format!("ws-trace-{index}"),
                "disposition": concat!(
                    "Leased { reason: \"mount_uncertain:remount_transaction:",
                    "workspace setup failed at workspace runtime hooks do not implement remount\" }"
                ),
            }))
        );
    }

    let manifest = LayerStack::open(root.clone())
        .expect("open post-squash stack")
        .read_active_manifest()
        .expect("read post-squash manifest");
    let sampled = sample_layerstack(&root, WalkBudget::default());
    let mut expected_outer_keys = vec![
        "blocks",
        "manifest_version",
        "s2_active_logical_bytes",
        "s2_layer_count",
        "s2_root_hash",
        "s2_staging_entry_count",
        "s2_storage_logical_bytes",
        "sweep_width",
        "swept",
    ];
    #[cfg(unix)]
    expected_outer_keys.extend(["s2_active_allocated_bytes", "s2_storage_allocated_bytes"]);
    expected_outer_keys.sort_unstable();
    assert_eq!(sorted_attr_keys(squash), expected_outer_keys);
    assert_eq!(squash.attrs["manifest_version"], manifest.version);
    assert_eq!(squash.attrs["blocks"], 1);
    assert_eq!(squash.attrs["swept"], live_sessions);
    assert_eq!(squash.attrs["sweep_width"], 4);
    assert_eq!(squash.attrs["s2_root_hash"], manifest_root_hash(&manifest));
    assert_eq!(squash.attrs["s2_layer_count"], manifest.layers.len());
    assert_eq!(squash.attrs["s2_staging_entry_count"], 0);
    assert_eq!(
        squash.attrs["s2_active_logical_bytes"].as_u64(),
        sampled.total_bytes
    );
    assert!(squash.attrs["s2_storage_logical_bytes"]
        .as_u64()
        .is_some_and(|value| value <= sampled.storage_logical_bytes.expect("post storage bytes")));
    #[cfg(unix)]
    {
        assert_eq!(
            squash.attrs["s2_active_allocated_bytes"].as_u64(),
            sampled.total_allocated_bytes
        );
        assert!(squash.attrs["s2_storage_allocated_bytes"]
            .as_u64()
            .is_some_and(|value| {
                value
                    <= sampled
                        .storage_allocated_bytes
                        .expect("post allocated bytes")
            }));
    }
}

fn observed_operations(
    observer: Observer,
    live_sessions: usize,
) -> (SandboxRuntimeOperations, PathBuf) {
    let fake = Arc::new(FakeWorkspaceService::new());
    let layerstack = support::observed_layerstack_service(observer.clone());
    let root = layerstack.layer_stack_root().to_path_buf();
    let workspace = Arc::new(WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        Arc::clone(&layerstack),
        observer,
    ));
    let launch_driver = support::FakeLaunchDriver::new();
    let command = Arc::new(support::build_command_service(&workspace, &launch_driver));
    for index in 0..live_sessions {
        fake.push_create_result(Ok(support::workspace_handle(
            &format!("ws-trace-{index}"),
            &format!("lease-trace-{index}"),
            std::env::temp_dir().join(format!("squash-trace-ws-{index}")),
            NetworkProfile::Shared,
        )));
        workspace
            .create_workspace_session(support::create_request())
            .expect("create observed live session");
    }
    (
        SandboxRuntimeOperations::new(command, workspace, layerstack, support::test_file_service()),
        root,
    )
}

fn only_span<'a>(spans: &'a [Span], name: &str) -> &'a Span {
    let matches = spans
        .iter()
        .filter(|span| span.name == name)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "exactly one {name} span");
    matches[0]
}

fn attrs(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().expect("attrs object").clone()
}

fn sorted_attr_keys(span: &Span) -> Vec<&str> {
    let mut keys = span.attrs.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}

struct TempTraceLog {
    root: PathBuf,
    path: PathBuf,
}

impl TempTraceLog {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sandbox-layerstack-trace-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create trace directory");
        Self {
            path: root.join("observability.ndjson"),
            root,
        }
    }
}

impl Drop for TempTraceLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
