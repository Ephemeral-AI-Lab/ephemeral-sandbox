use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_observability_query::ports::{
    NamespaceExecutionSnapshot, ObservabilityInput, ObservabilitySnapshot, QueryContext,
    QueryLimits, WorkspaceSnapshot,
};
use sandbox_observability_query::{dispatch_operation, observability_handler_keys};
use sandbox_observability_telemetry::collect::process_topology::{
    WorkspaceProcess, WorkspaceProcessKind, WorkspaceProcessState, WorkspaceProcessTopology,
    WorkspaceProcesses,
};
use sandbox_observability_telemetry::{LayerBytes, LayerStackBytes, Reader};
use sandbox_operation_contract::{
    OperationRequest, OperationScope, OperationScopeKind, OperationVisibility,
};
use sandbox_runtime_layerstack::service::{LayerStatus, StackObservation};
use sandbox_runtime_layerstack::{
    LayerDeltaDescription, LayerDeltaEntry, LayerDeltaEntryKind, LayerPath, LayerRef,
};
use serde_json::{json, Value};

struct FakeInput {
    log_path: Option<PathBuf>,
    snapshot: ObservabilitySnapshot,
    observation: Result<StackObservation, String>,
    bytes: LayerStackBytes,
    delta: Result<LayerDeltaDescription, String>,
    limits: QueryLimits,
    topology: WorkspaceProcessTopology,
    described_path: RefCell<Option<String>>,
    described_limit: Cell<Option<usize>>,
}

impl Default for FakeInput {
    fn default() -> Self {
        Self {
            log_path: None,
            snapshot: ObservabilitySnapshot::default(),
            observation: Ok(StackObservation {
                manifest_version: 1,
                root_hash: "root-1".to_owned(),
                active_lease_count: 0,
                layers: Vec::new(),
            }),
            bytes: LayerStackBytes::default(),
            delta: Ok(LayerDeltaDescription {
                entries: Vec::new(),
                truncated: false,
            }),
            limits: QueryLimits {
                resource_window_ms: 600_000,
                layer_delta_default_limit: 500,
                layer_delta_max_limit: 5_000,
            },
            topology: WorkspaceProcessTopology::unavailable("procfs unavailable"),
            described_path: RefCell::new(None),
            described_limit: Cell::new(None),
        }
    }
}

impl ObservabilityInput for FakeInput {
    fn query_context(&self) -> Option<QueryContext> {
        let primary = self.log_path.clone()?;
        Some(QueryContext {
            reader: Reader::new(primary.clone(), primary.with_extension("ndjson.1")),
            sandbox_id: "sandbox-1".to_owned(),
            daemon_pid: 42,
            runtime_dir: "/run/ephemeral/sandbox-1".to_owned(),
        })
    }

    fn query_limits(&self) -> QueryLimits {
        self.limits
    }

    fn cgroup_topology(&self) -> WorkspaceProcessTopology {
        self.topology.clone()
    }

    fn observability_snapshot(&self) -> ObservabilitySnapshot {
        self.snapshot.clone()
    }

    fn observe_layerstack(&self) -> Result<StackObservation, String> {
        self.observation.clone()
    }

    fn layerstack_bytes(&self) -> LayerStackBytes {
        self.bytes.clone()
    }

    fn describe_layer_delta(
        &self,
        layer_path: &str,
        limit: usize,
    ) -> Result<LayerDeltaDescription, String> {
        self.described_path.replace(Some(layer_path.to_owned()));
        self.described_limit.set(Some(limit));
        self.delta.clone()
    }
}

#[test]
fn sandbox_registry_is_bijective_with_observability_domain_routes() {
    let mut expected = sandbox_operation_catalog::routes::observability_routes()
        .iter()
        .filter(|route| {
            route.scope_kind == OperationScopeKind::Sandbox
                && route.visibility == OperationVisibility::Public
        })
        .map(|route| (route.scope_kind, route.operation))
        .collect::<Vec<_>>();
    let mut actual = observability_handler_keys().collect::<Vec<_>>();
    expected.sort_by_key(|(scope, name)| (scope_order(*scope), *name));
    actual.sort_by_key(|(scope, name)| (scope_order(*scope), *name));
    assert_eq!(actual, expected);
}

#[test]
fn system_snapshot_and_unknown_operations_are_not_observability_handlers() {
    let input = FakeInput::default();
    let system_snapshot = OperationRequest::new(
        "snapshot",
        "req-system",
        OperationScope::system(),
        json!({}),
    );
    let unknown = request("unknown", json!({}));

    assert_eq!(
        dispatch_operation(&input, &system_snapshot),
        sandbox_operation_contract::OperationResponse::unknown_op()
    );
    assert_eq!(
        dispatch_operation(&input, &unknown),
        sandbox_operation_contract::OperationResponse::unknown_op()
    );
}

#[test]
fn snapshot_renders_neutral_runtime_state_and_latest_resources() {
    let log_path = log_path("snapshot");
    write_lines(
        &log_path,
        &[sample_line("workspace-1", json!({ "disk_bytes": 3 }))],
    );
    let input = FakeInput {
        log_path: Some(log_path),
        snapshot: ObservabilitySnapshot {
            workspaces: vec![workspace("workspace-1", &["l0"])],
            active_namespace_executions: vec![NamespaceExecutionSnapshot {
                namespace_execution_id: "namespace_execution_1".to_owned(),
                workspace_session_id: "workspace-1".to_owned(),
                operation_name: "exec_command".to_owned(),
                command: Some("printf ok".to_owned()),
            }],
            partial_errors: vec!["partial workspace projection failed".to_owned()],
        },
        observation: Ok(StackObservation {
            manifest_version: 1,
            root_hash: "root-1".to_owned(),
            active_lease_count: 2,
            layers: vec![layer("l0", 1)],
        }),
        bytes: LayerStackBytes {
            layers: vec![bytes("l0", 120)],
            total_bytes: Some(120),
            total_allocated_bytes: Some(4096),
            storage_logical_bytes: Some(240),
            storage_allocated_bytes: Some(8192),
            staging_entry_count: Some(0),
        },
        ..FakeInput::default()
    };

    let value = dispatch_operation(&input, &request("snapshot", json!({}))).into_json_value();

    assert_eq!(value["sandbox_id"], "sandbox-1");
    assert_eq!(value["availability"], "partial");
    assert_eq!(value["errors"][0], "partial workspace projection failed");
    assert_eq!(value["daemon"]["daemon_pid"], 42);
    assert_eq!(value["workspaces"][0]["workspace_id"], "workspace-1");
    assert_eq!(
        value["workspaces"][0]["active_namespace_executions"][0]["namespace_execution_id"],
        "namespace_execution_1"
    );
    assert_eq!(
        value["workspaces"][0]["active_namespace_executions"][0]["command"],
        "printf ok"
    );
    assert_eq!(
        value["workspaces"][0]["resources"]["latest"]["metrics"]["disk_bytes"],
        3
    );
    assert_eq!(
        value["stack"],
        json!({
            "layer_count": 1,
            "layers_bytes": 120,
            "layers_allocated_bytes": 4096,
            "storage_allocated_bytes": 8192,
            "staging_entry_count": 0,
            "active_leases": 2
        })
    );
}

#[test]
fn configured_queries_fold_cgroup_events_and_trace_records() {
    let log_path = log_path("reader-folds");
    let now = unix_ms();
    write_lines(
        &log_path,
        &[
            json!({
                "ts": now - 200,
                "kind": "sample",
                "scope": "sandbox",
                "cpu_usec": 1000,
                "mem_cur": 2000,
                "_counters": ["cpu_usec"]
            }),
            json!({
                "ts": now - 100,
                "kind": "sample",
                "scope": "sandbox",
                "cpu_usec": 1600,
                "mem_cur": 2000,
                "_counters": ["cpu_usec"]
            }),
            json!({
                "ts": 900,
                "kind": "span",
                "trace": "req-old",
                "span": "old-root",
                "name": "daemon.dispatch",
                "dur_ms": 100.0,
                "status": "completed",
                "attrs": {}
            }),
            json!({
                "ts": 1000,
                "kind": "event",
                "trace": "req-old",
                "parent": "old-root",
                "name": "lease.acquired",
                "attrs": {}
            }),
            json!({
                "ts": 1500,
                "kind": "event",
                "trace": "req-old",
                "parent": "old-root",
                "name": "lease.released",
                "attrs": { "revision": "r4" }
            }),
            json!({
                "ts": 2000,
                "kind": "span",
                "trace": "req-new",
                "span": "d-0",
                "name": "daemon.dispatch",
                "dur_ms": 100.0,
                "status": "completed",
                "attrs": {}
            }),
            json!({
                "ts": 2050,
                "kind": "span",
                "trace": "req-new",
                "span": "d-1",
                "parent": "d-0",
                "name": "command.exec",
                "dur_ms": 50.0,
                "status": "completed",
                "attrs": {}
            }),
            json!({
                "ts": 2075,
                "kind": "span",
                "trace": "req-new",
                "span": "d-2",
                "parent": "d-1",
                "name": "workspace_session.create",
                "dur_ms": 25.0,
                "status": "completed",
                "attrs": {}
            }),
            json!({
                "ts": 2100,
                "kind": "event",
                "trace": "req-new",
                "parent": "d-2",
                "name": "lease.released",
                "attrs": { "revision": "r5" }
            }),
        ],
    );
    let input = FakeInput {
        log_path: Some(log_path),
        ..FakeInput::default()
    };

    let cgroup = dispatch_operation(
        &input,
        &request(
            "cgroup",
            json!({ "scope": "sandbox", "window_ms": 600_000 }),
        ),
    )
    .into_json_value();
    assert_eq!(cgroup["view"], "cgroup");
    assert_eq!(cgroup["topology"]["available"], false);
    assert_eq!(cgroup["topology"]["schema_version"], 2);
    assert_eq!(cgroup["topology"]["error"], "procfs unavailable");
    assert_eq!(cgroup["series"][1]["deltas"]["cpu_usec"], 600);
    assert!(cgroup["series"][1]["deltas"].get("mem_cur").is_none());

    let events =
        dispatch_operation(&input, &request("events", json!({ "last_n": 2 }))).into_json_value();
    assert_eq!(events["events"].as_array().map(Vec::len), Some(2));
    assert_eq!(events["events"][0]["ts"], 1500);
    assert_eq!(events["events"][1]["ts"], 2100);

    let filtered = dispatch_operation(
        &input,
        &request("events", json!({ "name": "lease.released", "last_n": 1 })),
    )
    .into_json_value();
    assert_eq!(filtered["events"].as_array().map(Vec::len), Some(1));
    assert_eq!(filtered["events"][0]["attrs"]["revision"], "r5");

    let trace = dispatch_operation(&input, &request("trace", json!({ "trace_id": "last" })))
        .into_json_value();
    assert_eq!(trace["trace"], "req-new");
    assert_eq!(trace["spans"][0]["span"]["name"], "daemon.dispatch");
    assert_eq!(
        trace["spans"][0]["children"][0]["span"]["name"],
        "command.exec"
    );
    assert_eq!(
        trace["spans"][0]["children"][0]["children"][0]["span"]["name"],
        "workspace_session.create"
    );
    assert_eq!(
        trace["spans"][0]["children"][0]["children"][0]["events"][0]["event"]["name"],
        "lease.released"
    );

    let old_trace = dispatch_operation(&input, &request("trace", json!({ "trace_id": "req-old" })))
        .into_json_value();
    assert_eq!(old_trace["trace"], "req-old");
    assert_eq!(old_trace["spans"][0]["span"]["span"], "old-root");
    assert_eq!(
        old_trace["spans"][0]["events"][1]["event"]["attrs"]["revision"],
        "r4"
    );
}

#[test]
fn cgroup_query_serializes_schema_v2_workspace_process_topology() {
    let input = FakeInput {
        log_path: Some(log_path("process-topology")),
        topology: WorkspaceProcessTopology {
            schema_version: 2,
            available: true,
            source: Some("proc_namespaces".to_owned()),
            error: None,
            truncated: false,
            warnings: vec!["one process raced with collection".to_owned()],
            workspaces: vec![WorkspaceProcesses {
                workspace_id: "workspace-1".to_owned(),
                state: WorkspaceProcessState::Active,
                holder_pid: 41,
                pid_namespace: Some("pid:[100]".to_owned()),
                mount_namespace: Some("mnt:[200]".to_owned()),
                processes: vec![WorkspaceProcess {
                    pid: 42,
                    namespace_pid: 2,
                    parent_pid: 41,
                    name: "worker".to_owned(),
                    state: "S (sleeping)".to_owned(),
                    kind: WorkspaceProcessKind::Process,
                    cgroup_memberships: vec!["0::/".to_owned()],
                }],
            }],
        },
        ..FakeInput::default()
    };

    let response = dispatch_operation(&input, &request("cgroup", json!({}))).into_json_value();

    assert_eq!(response["view"], "cgroup");
    assert_eq!(response["topology"]["schema_version"], 2);
    assert_eq!(response["topology"]["source"], "proc_namespaces");
    assert_eq!(response["topology"]["workspaces"][0]["state"], "active");
    assert_eq!(response["topology"]["workspaces"][0]["holder_pid"], 41);
    assert_eq!(
        response["topology"]["workspaces"][0]["processes"][0]["name"],
        "worker"
    );
    assert_eq!(
        response["topology"]["workspaces"][0]["processes"][0]["namespace_pid"],
        2
    );
    assert_eq!(
        response["topology"]["workspaces"][0]["processes"][0]["cgroup_memberships"][0],
        "0::/"
    );
}

#[test]
fn layerstack_inventory_merges_bytes_and_derives_bookings() {
    let input = FakeInput {
        observation: Ok(StackObservation {
            manifest_version: 5,
            root_hash: "root-5".to_owned(),
            active_lease_count: 2,
            layers: vec![
                layer("l4", 0),
                layer("l3", 1),
                layer("l2", 1),
                layer("l1", 0),
                layer("l0", 0),
            ],
        }),
        bytes: LayerStackBytes {
            layers: vec![
                bytes("l0", 120_000),
                bytes("l1", 84_000),
                bytes("l2", 20_000),
                bytes("l3", 20_000),
                bytes("l4", 5_000),
            ],
            total_bytes: Some(249_000),
            total_allocated_bytes: Some(249_000),
            storage_logical_bytes: Some(250_000),
            storage_allocated_bytes: Some(270_336),
            staging_entry_count: Some(2),
        },
        ..FakeInput::default()
    };

    let value = dispatch_operation(&input, &request("layerstack", json!({}))).into_json_value();
    let layers = value["layers"].as_array().expect("layers");

    assert_eq!(value["manifest_version"], 5);
    assert_eq!(value["total_bytes"], 249_000);
    assert_eq!(value["total_allocated_bytes"], 249_000);
    assert_eq!(value["storage_logical_bytes"], 250_000);
    assert_eq!(value["storage_allocated_bytes"], 270_336);
    assert_eq!(value["staging_entry_count"], 2);
    assert_eq!(layers[4]["bytes"], 120_000);
    assert_eq!(layers[4]["allocated_bytes"], 120_000);
    assert_eq!(layers[2]["booked_by"], json!(["l3"]));
    assert_eq!(layers[4]["booked_by"], json!(["l3", "l2"]));
}

#[test]
fn layerstack_preserves_missing_bytes_and_renders_workspace_sharing() {
    let log_path = log_path("workspace-upper-bytes");
    write_lines(
        &log_path,
        &[sample_line("ws-7", json!({ "disk_bytes": 156_000 }))],
    );
    let input = FakeInput {
        log_path: Some(log_path),
        snapshot: ObservabilitySnapshot {
            workspaces: vec![
                workspace("ws-7", &["l0", "l1", "l2"]),
                workspace("ws-9", &["l0", "l1"]),
            ],
            ..ObservabilitySnapshot::default()
        },
        observation: Ok(StackObservation {
            manifest_version: 1,
            root_hash: "root-1".to_owned(),
            active_lease_count: 0,
            layers: vec![layer("l0", 0)],
        }),
        ..FakeInput::default()
    };

    let inventory = dispatch_operation(&input, &request("layerstack", json!({}))).into_json_value();
    assert!(inventory["layers"][0]["bytes"].is_null());
    assert!(inventory["layers"][0]["allocated_bytes"].is_null());
    assert!(inventory["total_bytes"].is_null());

    let workspace = dispatch_operation(
        &input,
        &request("layerstack", json!({ "workspace_id": "ws-7" })),
    )
    .into_json_value();
    assert_eq!(
        workspace["mounts"][0],
        json!({ "layer_id": "l0", "shared_with": ["ws-9"] })
    );
    assert_eq!(
        workspace["mounts"][2],
        json!({ "layer_id": "l2", "shared_with": [] })
    );
    assert_eq!(workspace["upper_bytes"], 156_000);

    let missing = dispatch_operation(
        &input,
        &request("layerstack", json!({ "workspace_id": "missing" })),
    )
    .into_json_value();
    assert_eq!(missing["error"]["kind"], "invalid_request");
}

#[test]
fn layer_delta_uses_direct_layerstack_types_and_enforces_limits() {
    let input = FakeInput {
        observation: Ok(StackObservation {
            manifest_version: 1,
            root_hash: "root-1".to_owned(),
            active_lease_count: 0,
            layers: vec![layer("l1", 0)],
        }),
        delta: Ok(LayerDeltaDescription {
            entries: vec![
                delta_entry("src/main.rs", LayerDeltaEntryKind::File),
                delta_entry("src/old.rs", LayerDeltaEntryKind::Delete),
                delta_entry("config", LayerDeltaEntryKind::OpaqueDir),
            ],
            truncated: true,
        }),
        ..FakeInput::default()
    };

    let value = dispatch_operation(&input, &request("layerstack", json!({ "layer_id": "l1" })))
        .into_json_value();
    assert_eq!(input.described_path.borrow().as_deref(), Some("layers/l1"));
    assert_eq!(input.described_limit.get(), Some(500));
    assert_eq!(
        value["entries"][0],
        json!({ "path": "src/main.rs", "kind": "file" })
    );
    assert_eq!(
        value["entries"][1],
        json!({ "path": "src/old.rs", "kind": "delete" })
    );
    assert_eq!(
        value["entries"][2],
        json!({ "path": "config", "kind": "opaque_dir" })
    );
    assert_eq!(value["truncated"], true);

    let too_large = dispatch_operation(
        &input,
        &request("layerstack", json!({ "layer_id": "l1", "limit": 5001 })),
    )
    .into_json_value();
    assert_eq!(too_large["error"]["kind"], "invalid_request");

    let exclusive = dispatch_operation(
        &input,
        &request(
            "layerstack",
            json!({ "workspace_id": "ws-7", "layer_id": "l1" }),
        ),
    )
    .into_json_value();
    assert_eq!(exclusive["error"]["kind"], "invalid_request");
}

#[test]
fn configured_only_queries_fail_without_a_reader_but_layerstack_remains_available() {
    let input = FakeInput::default();
    for operation in ["snapshot", "trace", "events", "cgroup"] {
        let args = if operation == "trace" {
            json!({ "trace_id": "last" })
        } else {
            json!({})
        };
        let value = dispatch_operation(&input, &request(operation, args)).into_json_value();
        assert_eq!(value["error"]["kind"], "internal_error", "{operation}");
    }
    let layerstack =
        dispatch_operation(&input, &request("layerstack", json!({}))).into_json_value();
    assert_eq!(layerstack["view"], "layerstack");

    let malformed =
        dispatch_operation(&input, &request("cgroup", json!({ "scope": 7 }))).into_json_value();
    assert_eq!(malformed["error"]["kind"], "invalid_request");
}

fn request(operation: &str, args: Value) -> OperationRequest {
    OperationRequest::new(
        operation,
        format!("req-{operation}"),
        OperationScope::sandbox("sandbox-1"),
        args,
    )
}

fn workspace(id: &str, layer_ids: &[&str]) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        workspace_id: id.to_owned(),
        network_profile: "shared".to_owned(),
        finalize_policy: "no_op".to_owned(),
        namespace_fd_count: Some(3),
        base_root_hash: Some("root".to_owned()),
        layer_count: Some(layer_ids.len()),
        layer_ids: layer_ids.iter().map(|id| (*id).to_owned()).collect(),
    }
}

fn layer(id: &str, leased_by_workspaces: usize) -> LayerStatus {
    LayerStatus {
        layer: LayerRef {
            layer_id: id.to_owned(),
            path: format!("layers/{id}"),
        },
        leased_by_workspaces,
    }
}

fn bytes(id: &str, count: u64) -> LayerBytes {
    LayerBytes {
        layer_id: id.to_owned(),
        bytes: Some(count),
        allocated_bytes: Some(count),
    }
}

fn delta_entry(path: &str, kind: LayerDeltaEntryKind) -> LayerDeltaEntry {
    LayerDeltaEntry {
        path: LayerPath::parse(path).expect("valid test path"),
        kind,
    }
}

fn sample_line(scope: &str, metrics: Value) -> Value {
    let mut value = json!({
        "ts": unix_ms(),
        "kind": "sample",
        "scope": scope
    });
    value
        .as_object_mut()
        .expect("sample is an object")
        .extend(metrics.as_object().expect("metrics are an object").clone());
    value
}

fn write_lines(path: &PathBuf, lines: &[Value]) {
    let contents = lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{contents}\n")).expect("write log fixture");
}

fn log_path(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "sandbox-observability-query-{label}-{}-{}.ndjson",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch");
    i64::try_from(duration.as_millis()).expect("timestamp fits i64")
}

const fn scope_order(scope: OperationScopeKind) -> u8 {
    match scope {
        OperationScopeKind::System => 0,
        OperationScopeKind::Sandbox => 1,
    }
}
