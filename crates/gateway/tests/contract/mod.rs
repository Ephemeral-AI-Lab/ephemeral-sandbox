//! `gateway` contract conformance: the router covers every
//! catalog entry, refuses non-public ops on the client socket, and produces
//! the documented API error kinds — proven over a real Unix-socket round trip
//! with a stub engine (no docker required).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::{
    handle, handle_connection, operator_socket_path, parse_request, serve_with_catalog, Catalog,
    ClientRequest, Engine, Route, Surface, Visibility,
};
use host::{ForwardError, ForwardTraceContext, HostForwardRequest};

const KNOWN_SANDBOX: &str = "sb-stub";

struct StubEngine;

impl Engine for StubEngine {
    fn acquire(&self, _trace: &ForwardTraceContext, _args: &Value) -> anyhow::Result<String> {
        Ok(KNOWN_SANDBOX.to_owned())
    }

    fn release(
        &self,
        sandbox_id: &str,
        _trace: &ForwardTraceContext,
        _args: &Value,
    ) -> anyhow::Result<bool> {
        Ok(sandbox_id == KNOWN_SANDBOX)
    }

    fn status(&self, sandbox_id: &str) -> Option<Value> {
        (sandbox_id == KNOWN_SANDBOX)
            .then(|| json!({"sandbox_id": sandbox_id, "daemon": {"ready": true}}))
    }

    fn list(&self) -> Vec<Value> {
        vec![json!({"sandbox_id": KNOWN_SANDBOX})]
    }

    fn trace_requests(&self, _trace: &ForwardTraceContext, _args: &Value) -> anyhow::Result<Value> {
        Ok(json!({"requests": []}))
    }

    fn trace_show(&self, _trace: &ForwardTraceContext, args: &Value) -> anyhow::Result<Value> {
        let Some(trace_id) = args.get("trace_id").and_then(Value::as_str) else {
            anyhow::bail!("trace_id is required");
        };
        Ok(json!({
            "trace_id": trace_id,
            "requests": [],
            "spans": [],
            "events": [],
            "resources": [],
            "links": [],
            "audit_entries": [],
        }))
    }

    fn trace_verify(&self, _trace: &ForwardTraceContext, args: &Value) -> anyhow::Result<Value> {
        Ok(json!({"ok": true, "trace_id": args.get("trace_id").cloned()}))
    }

    fn forward(&self, request: HostForwardRequest<'_>) -> Option<Result<Value, ForwardError>> {
        let HostForwardRequest {
            sandbox_id,
            mutates_state,
            family,
            op,
            invocation_id,
            ..
        } = request;
        if sandbox_id != KNOWN_SANDBOX {
            return None;
        }
        Some(match op {
            "sandbox.file.write" => Err(ForwardError::UncertainOutcome("stub".into())),
            "sandbox.command.poll" => Err(ForwardError::SandboxUnavailable("stub".into())),
            _ => Ok(ok_envelope_with_sidecar(json!({
                "forwarded_op": op,
                "family": family,
                "mutates_state": mutates_state,
                "invocation_id": invocation_id
            }))),
        })
    }
}

#[derive(Clone)]
struct RecordingEngine {
    events: Arc<Mutex<Vec<RecordedEvent>>>,
}

impl RecordingEngine {
    fn new(events: Arc<Mutex<Vec<RecordedEvent>>>) -> Self {
        Self { events }
    }
}

#[derive(Clone, Debug)]
struct RecordedEvent {
    module: String,
    event: String,
    details: Value,
}

impl Engine for RecordingEngine {
    fn acquire(&self, _trace: &ForwardTraceContext, _args: &Value) -> anyhow::Result<String> {
        Ok(KNOWN_SANDBOX.to_owned())
    }

    fn release(
        &self,
        sandbox_id: &str,
        _trace: &ForwardTraceContext,
        _args: &Value,
    ) -> anyhow::Result<bool> {
        Ok(sandbox_id == KNOWN_SANDBOX)
    }

    fn status(&self, sandbox_id: &str) -> Option<Value> {
        (sandbox_id == KNOWN_SANDBOX).then(|| json!({"sandbox_id": sandbox_id}))
    }

    fn list(&self) -> Vec<Value> {
        Vec::new()
    }

    fn forward(&self, request: HostForwardRequest<'_>) -> Option<Result<Value, ForwardError>> {
        let HostForwardRequest { sandbox_id, op, .. } = request;
        (sandbox_id == KNOWN_SANDBOX).then(|| Ok(ok_envelope(json!({"forwarded_op": op}))))
    }

    fn record_trace_event(
        &self,
        _sandbox_id: &str,
        _trace: &ForwardTraceContext,
        module: &str,
        event: &str,
        details: Value,
    ) {
        self.events
            .lock()
            .expect("events lock")
            .push(RecordedEvent {
                module: module.to_owned(),
                event: event.to_owned(),
                details,
            });
    }
}

fn request(op: &str, sandbox_id: Option<&str>) -> ClientRequest {
    let mut request =
        json!({"op": op, "invocation_id": "00000000000000000000000000000001", "args": {}});
    if let Some(id) = sandbox_id {
        request["sandbox_id"] = json!(id);
    }
    parse_request(&serde_json::to_vec(&request).expect("encode")).expect("parse")
}

#[test]
fn request_id_is_not_accepted_as_request_identity() {
    let request = json!({
        "op": "sandbox.file.read",
        "request_id": "not-an-invocation-id",
        "args": {}
    });
    let error = parse_request(&serde_json::to_vec(&request).expect("encode"))
        .expect_err("top-level request_id must not satisfy invocation_id");
    assert_eq!(error.kind, "invalid_request");
    assert_eq!(
        error.message,
        "invocation_id is required and must be a string"
    );
}

fn kind(response: &Value) -> Option<&str> {
    response.get("error")?.get("kind")?.as_str()
}

fn result(response: &Value) -> &Value {
    response.get("result").unwrap_or(response)
}

fn event_recorded(snapshot: &[RecordedEvent], module: &str, event: &str) -> bool {
    snapshot
        .iter()
        .any(|entry| entry.module == module && entry.event == event)
}

fn ok_envelope(result: Value) -> Value {
    json!({
        "status": "ok",
        "result": result,
        "meta": {
            "envelope_version": 2,
            "op": "stub",
            "request_id": "stub-request",
            "trace": {
                "trace_id": "stub-trace",
                "request_id": "stub-request",
                "store": "pending_host_ingest",
                "event_count": 0,
                "degraded": false,
            },
            "workspace_route": {"kind": "none"},
            "duration_ms": 0.0,
            "modules_touched": [],
            "steps": [],
            "resource_summary": {"fields": {}},
            "warnings": [],
        },
    })
}

fn ok_envelope_with_sidecar(result: Value) -> Value {
    let mut response = ok_envelope(result);
    response["_trace_events"] = json!("internal-sidecar");
    response
}

#[test]
fn router_covers_every_catalog_entry() {
    let catalog = Catalog::load_builtin().expect("catalog loads and every entry routes");
    let engine = StubEngine;
    for entry in catalog.entries() {
        let response = handle(
            &catalog,
            &engine,
            Surface::Operator,
            &request(&entry.name, Some(KNOWN_SANDBOX)),
        );
        if matches!(entry.visibility, Visibility::Internal | Visibility::Test) {
            assert_eq!(kind(&response), Some("forbidden"), "{}", entry.name);
            continue;
        }
        assert_ne!(
            kind(&response),
            Some("unknown_op"),
            "catalog entry must route: {}",
            entry.name
        );
    }
}

#[test]
fn daemon_ops_route_under_canonical_names_only() {
    let catalog = Catalog::load_builtin().expect("catalog");
    let engine = StubEngine;
    for (name, family) in [
        ("sandbox.file.read", "Files"),
        ("sandbox.call.heartbeat", "Control"),
    ] {
        let entry = catalog.lookup(name).expect("canonical name resolves");
        assert_eq!(entry.name, name);
        assert_eq!(entry.route, Route::Daemon);
        let response = handle(
            &catalog,
            &engine,
            Surface::Client,
            &request(name, Some(KNOWN_SANDBOX)),
        );
        // The daemon's response comes back verbatim.
        assert_eq!(result(&response)["forwarded_op"], json!(name));
        assert_eq!(result(&response)["family"], json!(family));
        assert_eq!(response["_trace_events"], Value::Null);
    }
    // The retired legacy spellings are no longer in the catalog.
    for legacy in ["api.v1.read_file", "api.v1.heartbeat"] {
        assert!(catalog.lookup(legacy).is_none(), "{legacy} must be retired");
        let response = handle(
            &catalog,
            &engine,
            Surface::Client,
            &request(legacy, Some(KNOWN_SANDBOX)),
        );
        assert_eq!(kind(&response), Some("unknown_op"), "{legacy}: {response}");
    }
}

#[test]
fn client_socket_refuses_non_public_ops() {
    let catalog = Catalog::load_builtin().expect("catalog");
    let engine = StubEngine;
    for (op, surface, expected_forbidden) in [
        // Operator ops: forbidden on the client socket, served on operator.
        ("sandbox.checkpoint.layer_metrics", Surface::Client, true),
        ("sandbox.checkpoint.layer_metrics", Surface::Operator, false),
        ("sandbox.run.cancel_all", Surface::Client, true),
        ("host.trace.requests", Surface::Client, true),
        ("host.trace.requests", Surface::Operator, false),
        ("host.trace.verify", Surface::Client, true),
        ("host.trace.verify", Surface::Operator, false),
        ("host.image_profiles.list", Surface::Client, false),
        ("host.image.list", Surface::Client, true),
        ("host.image.list", Surface::Operator, false),
        ("host.container.list", Surface::Client, true),
        ("host.container.list", Surface::Operator, false),
        // Internal and test ops: forbidden everywhere.
        ("sandbox.runtime.ready", Surface::Client, true),
        ("sandbox.runtime.ready", Surface::Operator, true),
        ("sandbox.isolation.test_reset", Surface::Operator, true),
        // Public ops pass the client gate.
        ("sandbox.file.read", Surface::Client, false),
        ("host.sandbox.acquire", Surface::Client, false),
    ] {
        let response = handle(
            &catalog,
            &engine,
            surface,
            &request(op, Some(KNOWN_SANDBOX)),
        );
        assert_eq!(
            kind(&response) == Some("forbidden"),
            expected_forbidden,
            "visibility gate mismatch for {op} on {surface:?}: {response}"
        );
    }
}

#[test]
fn api_error_kinds_are_produced() {
    let catalog = Catalog::load_builtin().expect("catalog");
    let engine = StubEngine;
    let cases = [
        ("api.totally.bogus.op", Some(KNOWN_SANDBOX), "unknown_op"),
        ("sandbox.file.read", Some("sb-missing"), "unknown_sandbox"),
        ("sandbox.file.read", None, "invalid_request"),
        (
            "sandbox.file.write",
            Some(KNOWN_SANDBOX),
            "uncertain_outcome",
        ),
        (
            "sandbox.command.poll",
            Some(KNOWN_SANDBOX),
            "sandbox_unavailable",
        ),
    ];
    for (op, sandbox, expected) in cases {
        let response = handle(&catalog, &engine, Surface::Client, &request(op, sandbox));
        assert_eq!(kind(&response), Some(expected), "{op}: {response}");
    }
    let response = handle(
        &catalog,
        &engine,
        Surface::Operator,
        &request("host.trace.show", Some(KNOWN_SANDBOX)),
    );
    assert_eq!(
        kind(&response),
        Some("invalid_request"),
        "operator trace arg errors should not be flattened: {response}"
    );
    // Dynamic plugin ops are no longer forwarded without a catalog entry.
    let response = handle(
        &catalog,
        &engine,
        Surface::Client,
        &request("plugin.legacy.query", Some(KNOWN_SANDBOX)),
    );
    assert_eq!(kind(&response), Some("unknown_op"), "{response}");
}

#[test]
fn unix_socket_round_trip_serves_one_request_per_connection() {
    let socket = test_socket_path("round-trip");
    let catalog = Arc::new(Catalog::load_builtin().expect("catalog"));
    let listen = socket.clone();
    std::thread::spawn(move || {
        let _ = serve_with_catalog(&listen, catalog, Arc::new(StubEngine));
    });
    let response = round_trip_when_ready(
        &socket,
        b"{\"op\":\"host.sandbox.acquire\",\"invocation_id\":\"i1\",\"args\":{}}\n",
    );
    assert_eq!(response["status"], json!("ok"));
    assert_eq!(result(&response)["sandbox_id"], json!(KNOWN_SANDBOX));

    // Malformed JSON surfaces bad_json; the server half-closes after one line.
    let response = round_trip_when_ready(&socket, b"{not json\n");
    assert_eq!(kind(&response), Some("bad_json"));

    // Operator ops are forbidden on the client socket but served on operator.
    let metrics = b"{\"op\":\"sandbox.checkpoint.layer_metrics\",\"sandbox_id\":\"sb-stub\",\"invocation_id\":\"i2\",\"args\":{}}\n";
    let response = round_trip_when_ready(&socket, metrics);
    assert_eq!(kind(&response), Some("forbidden"));
    let response = round_trip_when_ready(&operator_socket_path(&socket), metrics);
    assert_eq!(
        result(&response)["forwarded_op"],
        json!("sandbox.checkpoint.layer_metrics")
    );

    let _ = std::fs::remove_file(operator_socket_path(&socket));
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn unix_socket_records_forward_and_response_write_events() {
    let socket = test_socket_path("trace-events");
    let catalog = Arc::new(Catalog::load_builtin().expect("catalog"));
    let events = Arc::new(Mutex::new(Vec::new()));
    let listen = socket.clone();
    let engine = RecordingEngine::new(Arc::clone(&events));
    std::thread::spawn(move || {
        let _ = serve_with_catalog(&listen, catalog, Arc::new(engine));
    });

    let response = round_trip_when_ready(
        &socket,
        b"{\"op\":\"sandbox.file.read\",\"sandbox_id\":\"sb-stub\",\"invocation_id\":\"i3\",\"args\":{}}\n",
    );
    assert_eq!(response["status"], json!("ok"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let snapshot = events.lock().expect("events lock").clone();
        if event_recorded(&snapshot, "gateway.route", "engine_forward_finished")
            && event_recorded(&snapshot, "gateway.transport", "response_written")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "gateway events not recorded: {snapshot:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let _ = std::fs::remove_file(operator_socket_path(&socket));
    let _ = std::fs::remove_file(&socket);
}

#[test]
fn unix_socket_records_response_write_failure() {
    let catalog = Catalog::load_builtin().expect("catalog");
    let events = Arc::new(Mutex::new(Vec::new()));
    let engine = RecordingEngine::new(Arc::clone(&events));
    let (mut client, server) = UnixStream::pair().expect("socket pair");
    client
        .write_all(
            b"{\"op\":\"sandbox.file.read\",\"sandbox_id\":\"sb-stub\",\"invocation_id\":\"i4\",\"args\":{}}\n",
        )
        .expect("write request");
    client.shutdown(std::net::Shutdown::Both).ok();
    drop(client);

    handle_connection(
        server,
        Surface::Client,
        "/tmp/esg-pair.sock",
        &catalog,
        &engine,
    );

    let snapshot = events.lock().expect("events lock").clone();
    assert!(
        event_recorded(&snapshot, "gateway.transport", "write_failed"),
        "gateway events not recorded: {snapshot:?}"
    );
}

#[test]
fn rejected_routes_record_route_rejected_events() {
    let catalog = Catalog::load_builtin().expect("catalog");
    let events = Arc::new(Mutex::new(Vec::new()));
    let engine = RecordingEngine::new(Arc::clone(&events));

    let response = handle(
        &catalog,
        &engine,
        Surface::Client,
        &request("sandbox.runtime.ready", Some(KNOWN_SANDBOX)),
    );
    assert_eq!(kind(&response), Some("forbidden"));
    let response = handle(
        &catalog,
        &engine,
        Surface::Client,
        &request("api.totally.bogus.op", Some(KNOWN_SANDBOX)),
    );
    assert_eq!(kind(&response), Some("unknown_op"));

    let snapshot = events.lock().expect("events lock").clone();
    assert_eq!(
        snapshot
            .iter()
            .filter(|entry| entry.module == "gateway.route" && entry.event == "route_rejected")
            .count(),
        2,
        "forbidden and unknown_op both record route_rejected: {snapshot:?}"
    );
}

#[test]
fn host_routes_record_route_selected_events() {
    let catalog = Catalog::load_builtin().expect("catalog");
    let events = Arc::new(Mutex::new(Vec::new()));
    let engine = RecordingEngine::new(Arc::clone(&events));

    let response = handle(
        &catalog,
        &engine,
        Surface::Client,
        &request("host.sandbox.status", Some(KNOWN_SANDBOX)),
    );
    assert_eq!(response["status"], json!("ok"));

    let snapshot = events.lock().expect("events lock").clone();
    let route_event = snapshot
        .iter()
        .find(|entry| entry.module == "gateway.route" && entry.event == "route_selected")
        .expect("host route records route_selected");
    assert_eq!(route_event.details["family"], json!("Sandbox"));
}

#[test]
fn legacy_host_aliases_are_retired() {
    let catalog = Catalog::load_builtin().expect("catalog");
    let engine = StubEngine;
    for legacy in [
        "sandbox.acquire",
        "sandbox.release",
        "sandbox.status",
        "sandbox.list",
        "sandbox.trace.requests",
        "sandbox.trace.show",
        "sandbox.trace.verify",
    ] {
        assert!(catalog.lookup(legacy).is_none(), "{legacy} must be retired");
        let response = handle(
            &catalog,
            &engine,
            Surface::Operator,
            &request(legacy, Some(KNOWN_SANDBOX)),
        );
        assert_eq!(kind(&response), Some("unknown_op"), "{legacy}: {response}");
    }
}

#[test]
fn sandbox_attributed_parse_failures_record_parse_failed_events() {
    let catalog = Catalog::load_builtin().expect("catalog");
    let events = Arc::new(Mutex::new(Vec::new()));
    let engine = RecordingEngine::new(Arc::clone(&events));
    let (mut client, server) = UnixStream::pair().expect("socket pair");
    client
        .write_all(b"{\"sandbox_id\":\"sb-stub\",\"op\":\"sandbox.file.read\",\"args\":{}}\n")
        .expect("write request");
    client.shutdown(std::net::Shutdown::Write).ok();

    handle_connection(
        server,
        Surface::Client,
        "/tmp/esg-pair.sock",
        &catalog,
        &engine,
    );

    let snapshot = events.lock().expect("events lock").clone();
    assert!(
        event_recorded(&snapshot, "gateway.transport", "parse_failed"),
        "missing invocation_id records parse_failed: {snapshot:?}"
    );
    assert!(
        event_recorded(&snapshot, "gateway.transport", "response_written"),
        "parse-failure response write is recorded: {snapshot:?}"
    );
}

fn test_socket_path(tag: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/esg-{tag}-{}.sock", std::process::id()))
}

fn round_trip_when_ready(socket: &PathBuf, line: &[u8]) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(err) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "server socket {} never came up: {err}",
                    socket.display()
                );
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    };
    stream.write_all(line).expect("write request");
    stream.flush().ok();
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).expect("read response");
    serde_json::from_str(response.trim_end()).expect("decode response")
}
