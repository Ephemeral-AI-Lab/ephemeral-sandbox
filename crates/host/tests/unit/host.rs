use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use base64::Engine as _;
use serde_json::{json, Value};
use trace::{
    encode_trace_batch, EventRecord, RequestId, SpanKind, SpanRecord, SpanUid, TraceBatch, TraceId,
    TraceRecord,
};

use crate::container::override_docker_command_for_tests;
use crate::daemon_wire::{
    encode_request_with_metadata, ClientError, DAEMON_TRACE_SIDECAR_ENCODING,
    DAEMON_TRACE_SIDECAR_FIELD, DAEMON_TRACE_SIDECAR_SCHEMA,
};
use crate::service::forward::{
    forward_request, ingest_and_strip_sidecar, record_client_error, record_endpoint_refreshed,
    tcp_once, tcp_with_connect_backoff, ForwardAttempt, ForwardRequestInput,
};
use crate::service::registry::{SandboxRecord, SandboxRegistry};
use crate::service::trace_drain::{drain_trace_export_once, TraceDrainTarget, TraceExportDrainer};
use crate::service::{ForwardTraceContext, HostConfig, SandboxHost};
use crate::trace_store::{
    PendingSidecarInput, RequestStartInput, TraceEventInput, TraceEventRow, TraceStore,
};

#[test]
fn direct_daemon_ops_match_catalog_contracts() {
    use ::protocol::catalog::{
        BuiltinOp, OpVisibility, ServedBy, SANDBOX_TRACE_EXPORT, SANDBOX_TRACE_EXPORT_ACK,
    };

    for (name, visibility) in [
        (crate::daemon_wire::READY_OP, OpVisibility::Internal),
        (crate::daemon_wire::HEARTBEAT_OP, OpVisibility::Public),
        (SANDBOX_TRACE_EXPORT, OpVisibility::Internal),
        (SANDBOX_TRACE_EXPORT_ACK, OpVisibility::Internal),
    ] {
        let op = BuiltinOp::from_op_name(name).expect("direct host-daemon op is catalogued");
        let contract = op.contract();
        assert_eq!(contract.served_by, ServedBy::Daemon, "{name}");
        assert_eq!(contract.visibility, visibility, "{name}");
    }
}

#[test]
fn registry_round_trips_records_and_tokens() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("eos-host-registry-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let registry = SandboxRegistry::open(dir.clone())?;
    let record = SandboxRecord::new(
        "sb-1".into(),
        "sb-1".into(),
        "tok".into(),
        37_657,
        "test".into(),
        None,
    );
    registry.insert(record)?;
    let record = registry.get("sb-1").expect("inserted record");
    assert_eq!(registry.load_token("sb-1")?, "tok");
    assert!(registry.get("sb-1").is_some());
    assert_eq!(registry.list().len(), 1);

    record.cache_endpoint("127.0.0.1:9999".parse().expect("addr"));
    assert!(record.cached_endpoint().is_some());
    record.invalidate_endpoint();
    assert!(record.cached_endpoint().is_none());

    assert!(registry.remove("sb-1").is_some());
    assert!(registry.get("sb-1").is_none());
    assert!(registry.load_token("sb-1").is_err());
    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn sandbox_lifecycle_respawn_waits_for_active_forward() -> Result<()> {
    let record = Arc::new(SandboxRecord::new(
        "sb-lifecycle".to_owned(),
        "sb-lifecycle".to_owned(),
        "token".to_owned(),
        37_657,
        "test".to_owned(),
        None,
    ));
    let forward = record.begin_forward();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
    let waiting_record = Arc::clone(&record);
    let handle = std::thread::spawn(move || {
        started_tx.send(()).expect("send start");
        let _respawn = waiting_record.begin_respawn();
        acquired_tx.send(()).expect("send acquired");
    });

    started_rx.recv_timeout(Duration::from_secs(1))?;
    assert!(
        acquired_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "respawn acquired lifecycle while a forward was still active"
    );

    drop(forward);
    acquired_rx.recv_timeout(Duration::from_secs(1))?;
    handle.join().expect("respawn waiter joins");
    Ok(())
}

#[test]
fn sandbox_lifecycle_forward_waits_for_active_respawn() -> Result<()> {
    let record = Arc::new(SandboxRecord::new(
        "sb-lifecycle-respawn".to_owned(),
        "sb-lifecycle-respawn".to_owned(),
        "token".to_owned(),
        37_657,
        "test".to_owned(),
        None,
    ));
    let respawn = record.begin_respawn();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
    let waiting_record = Arc::clone(&record);
    let handle = std::thread::spawn(move || {
        started_tx.send(()).expect("send start");
        let _forward = waiting_record.begin_forward();
        acquired_tx.send(()).expect("send acquired");
    });

    started_rx.recv_timeout(Duration::from_secs(1))?;
    assert!(
        acquired_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "forward acquired lifecycle while a respawn was still active"
    );

    drop(respawn);
    acquired_rx.recv_timeout(Duration::from_secs(1))?;
    handle.join().expect("forward waiter joins");
    Ok(())
}

#[test]
fn forward_request_persists_transport_events_and_strips_sidecar() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = listener.local_addr()?;
    let server = std::thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let request: serde_json::Value = serde_json::from_str(line.trim_end())?;
        let trace = request
            .get("trace")
            .and_then(serde_json::Value::as_object)
            .expect("host sends trace context");
        assert_eq!(trace["request_id"], json!("request-forward"));

        let trace_id = TraceId::parse(trace["trace_id"].as_str().expect("trace id"))?;
        let request_id = RequestId::parse(trace["request_id"].as_str().expect("request id"))?;
        let mut record = TraceRecord::new(trace_id, SpanUid::ROOT);
        record.request_id = Some(request_id);
        record.spans.push(SpanRecord::new(
            SpanUid::ROOT,
            None,
            "op_request",
            SpanKind::OpRequest,
            json!({"op": "sandbox.runtime.ready"}),
        ));
        record.spans.push(SpanRecord::new(
            SpanUid::new(2),
            Some(SpanUid::ROOT),
            "daemon.transport",
            SpanKind::DaemonTransport,
            json!({"listener_kind": "tcp"}),
        ));
        record.spans.push(SpanRecord::new(
            SpanUid::new(3),
            Some(SpanUid::ROOT),
            "dispatch",
            SpanKind::Dispatch,
            json!({"op": "sandbox.runtime.ready"}),
        ));
        record.spans.push(SpanRecord::new(
            SpanUid::new(4),
            Some(SpanUid::new(3)),
            "op.runtime.ready",
            SpanKind::Operation,
            json!({"op": "sandbox.runtime.ready"}),
        ));
        record.events.push(EventRecord::new(
            SpanUid::new(2),
            "accepted",
            "daemon.transport",
            json!({"listener_kind": "tcp", "request_bytes": line.len()}),
        ));
        record.events.push(EventRecord::new(
            SpanUid::new(2),
            "read_finished",
            "daemon.transport",
            json!({"request_bytes": line.len()}),
        ));
        record.events.push(EventRecord::new(
            SpanUid::new(2),
            "auth_checked",
            "daemon.transport",
            json!({"auth_required": true, "auth_ok": true}),
        ));
        record.events.push(EventRecord::new(
            SpanUid::new(2),
            "decoded",
            "daemon.transport",
            json!({"protocol_version": 1}),
        ));
        record.events.push(EventRecord::new(
            SpanUid::new(3),
            "dispatch_started",
            "daemon.dispatch",
            json!({"op": "sandbox.runtime.ready"}),
        ));
        record.events.push(EventRecord::new(
            SpanUid::new(3),
            "op_resolved",
            "daemon.dispatch",
            json!({"op": "sandbox.runtime.ready"}),
        ));
        record.events.push(EventRecord::new(
            SpanUid::new(4),
            "route_selected",
            "workspace.route",
            json!({"kind": "none"}),
        ));
        record.events.push(EventRecord::new(
            SpanUid::new(4),
            "ready_checked",
            "sandbox.runtime",
            json!({"ready": true}),
        ));
        record.events.push(EventRecord::new(
            SpanUid::new(2),
            "response_write_finished",
            "daemon.transport",
            json!({"response_bytes": 64}),
        ));
        let sidecar = base64::engine::general_purpose::STANDARD
            .encode(encode_trace_batch(&TraceBatch::single(record)));
        let mut response = json!({
            "status": "ok",
            "result": {"ready": true},
            "meta": {
                "envelope_version": 2,
                "op": "sandbox.runtime.ready",
                "request_id": "pending",
                "trace": {
                    "trace_id": "pending",
                    "request_id": "pending",
                    "root_span_id": 1,
                    "store": "pending_host_ingest",
                    "event_count": 9,
                    "degraded": false
                },
                "workspace_route": {"kind": "none"},
                "duration_ms": 0.0,
                "modules_touched": [],
                "steps": [],
                "resource_summary": {"fields": {}},
                "warnings": []
            },
        });
        response[DAEMON_TRACE_SIDECAR_FIELD] = json!({
            "schema": DAEMON_TRACE_SIDECAR_SCHEMA,
            "encoding": DAEMON_TRACE_SIDECAR_ENCODING,
            "spool_pending": false,
            "data": sidecar,
        });
        writeln!(stream, "{}", serde_json::to_string(&response)?)?;
        Ok(())
    });

    let dir = temp_host_dir("forward-trace");
    let store = Arc::new(TraceStore::open(&dir)?);
    let config = HostConfig {
        image: "test-image".to_owned(),
        platform: None,
        docker_privileged: true,
        eosd_path: dir.join("eosd"),
        config_yaml_path: dir.join("config.yml"),
        remote_daemon_dir: PathBuf::from("/eos/runtime"),
        remote_eosd_path: PathBuf::from("/eos/eosd"),
        remote_config_path: PathBuf::from("/eos/config.yml"),
        tcp_port: endpoint.port(),
        ready_timeout: Duration::from_secs(1),
        request_timeout: Duration::from_secs(2),
        created_by: "test".to_owned(),
        state_dir: dir.clone(),
    };
    let record = Arc::new(SandboxRecord::new(
        "sb-forward".to_owned(),
        "sb-forward".to_owned(),
        "token".to_owned(),
        endpoint.port(),
        "test".to_owned(),
        Some(endpoint),
    ));

    let mut trace = ForwardTraceContext::new("request-forward");
    trace.push_gateway_event(
        "gateway.transport",
        "accepted",
        json!({"surface": "client"}),
    );
    trace.push_gateway_event(
        "gateway.transport",
        "request_read",
        json!({"surface": "client", "request_bytes": 81}),
    );
    trace.push_gateway_event(
        "gateway.route",
        "route_selected",
        json!({"op": "sandbox.runtime.ready", "route": "daemon"}),
    );

    let response = forward_request(ForwardRequestInput {
        record: Arc::clone(&record),
        config: &config,
        trace_store: &store,
        trace_drainer: &TraceExportDrainer::default(),
        trace_context: trace,
        mutates_state: false,
        family: "Control",
        op: "sandbox.runtime.ready",
        invocation_id: "request-forward",
        args: &json!({"caller_id": "caller-1"}),
    })?;
    assert_eq!(response["_trace_events"], serde_json::Value::Null);
    assert_eq!(response["result"]["ready"], json!(true));
    assert_eq!(response["meta"]["request_id"], json!("request-forward"));
    assert_eq!(
        response["meta"]["trace"]["request_id"],
        json!("request-forward")
    );
    assert_eq!(response["meta"]["trace"]["store"], json!("local_sqlite"));

    let request = store
        .request_by_id("request-forward")?
        .expect("request row");
    assert_eq!(
        response["meta"]["trace"]["event_count"],
        json!(store.event_count_for_trace(&request.trace_id)?)
    );
    assert_eq!(request.status.as_deref(), Some("ok"));
    let replay_trace_id = TraceId::parse(request.trace_id.clone())?;
    let replay_request_id = RequestId::parse(request.request_id.clone())?;
    store.append_trace_event(TraceEventInput {
        sandbox_id: &request.sandbox_id,
        trace_id: &replay_trace_id,
        request_id: Some(&replay_request_id),
        span_id: None,
        module: "gateway.transport",
        event: "response_written",
        details: json!({"response_bytes": 32}),
    })?;
    let events = store.events_for_trace(&request.trace_id)?;
    let event_names: Vec<_> = events
        .iter()
        .map(|event| (event.module.as_str(), event.event.as_str()))
        .collect();
    assert!(
        event_names.contains(&("host.transport", "connect_started")),
        "{event_names:?}"
    );
    assert!(
        event_names.contains(&("gateway.transport", "request_read")),
        "{event_names:?}"
    );
    assert!(
        event_names.contains(&("gateway.route", "route_selected")),
        "{event_names:?}"
    );
    assert!(
        event_names.contains(&("host.transport", "request_written")),
        "{event_names:?}"
    );
    assert!(
        event_names.contains(&("host.transport", "response_read")),
        "{event_names:?}"
    );
    assert!(
        event_names.contains(&("daemon.transport", "accepted")),
        "{event_names:?}"
    );
    assert_ordered_events(
        &event_names,
        &[
            ("gateway.transport", "accepted"),
            ("gateway.transport", "request_read"),
            ("gateway.route", "route_selected"),
            ("host.protocol", "forward_started"),
            ("host.transport", "connect_started"),
            ("host.transport", "request_written"),
            ("daemon.transport", "accepted"),
            ("daemon.transport", "read_finished"),
            ("daemon.transport", "auth_checked"),
            ("daemon.transport", "decoded"),
            ("daemon.dispatch", "dispatch_started"),
            ("daemon.dispatch", "op_resolved"),
            ("workspace.route", "route_selected"),
            ("sandbox.runtime", "ready_checked"),
            ("daemon.transport", "response_write_finished"),
            ("host.transport", "response_read"),
            ("gateway.transport", "response_written"),
        ],
    );

    server.join().expect("server thread")?;
    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn malformed_sidecar_is_stripped_and_recorded_as_host_event() -> Result<()> {
    let dir = temp_host_dir("malformed-sidecar");
    let store = TraceStore::open(&dir)?;
    let endpoint = "127.0.0.1:9".parse().expect("discard port");
    let config = HostConfig {
        image: "test-image".to_owned(),
        platform: None,
        docker_privileged: true,
        eosd_path: dir.join("eosd"),
        config_yaml_path: dir.join("config.yml"),
        remote_daemon_dir: PathBuf::from("/eos/runtime"),
        remote_eosd_path: PathBuf::from("/eos/eosd"),
        remote_config_path: PathBuf::from("/eos/config.yml"),
        tcp_port: 9,
        ready_timeout: Duration::from_millis(100),
        request_timeout: Duration::from_millis(100),
        created_by: "test".to_owned(),
        state_dir: dir.clone(),
    };
    let record = SandboxRecord::new(
        "sb-malformed-sidecar".to_owned(),
        "sb-malformed-sidecar".to_owned(),
        "token".to_owned(),
        9,
        "test".to_owned(),
        Some(endpoint),
    );
    let trace_id = TraceId::parse("trace-malformed-sidecar")?;
    let request_id = RequestId::parse("request-malformed-sidecar")?;
    let args = json!({});
    let mut tcp_line =
        encode_request_with_metadata("sandbox.runtime.ready", request_id.as_str(), &args, None);
    tcp_line.push(b'\n');
    let attempt = ForwardAttempt {
        record: &record,
        config: &config,
        trace_store: &store,
        trace_id: trace_id.clone(),
        request_id,
        mutates_state: false,
        tcp_line,
        op: "sandbox.runtime.ready",
        invocation_id: "malformed-sidecar",
        args: &args,
    };
    let mut response = json!({
        "success": true,
        "_trace_events": {
            "schema": DAEMON_TRACE_SIDECAR_SCHEMA,
            "encoding": DAEMON_TRACE_SIDECAR_ENCODING,
            "spool_pending": false,
            "data": "not base64",
        },
    });

    let sidecar = ingest_and_strip_sidecar(&attempt, &mut response);

    assert!(sidecar.present);
    assert!(!sidecar.ingested);
    assert!(response.get("_trace_events").is_none());
    let events = store.events_for_trace(trace_id.as_str())?;
    assert_event(&events, "host.transport", "sidecar_decode_failed");
    assert!(
        events.iter().any(|event| {
            event.event == "sidecar_decode_failed"
                && serde_json::from_str::<serde_json::Value>(&event.details_json)
                    .ok()
                    .and_then(|details| details.get("error_kind").cloned())
                    == Some(json!("invalid_base64"))
        }),
        "sidecar_decode_failed details missing: {events:?}"
    );

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn decoded_sidecar_ingest_failures_are_spooled_and_recovered() -> Result<()> {
    let dir = temp_host_dir("pending-sidecar-recovery");
    let store = TraceStore::open(&dir)?;
    let endpoint = "127.0.0.1:9".parse().expect("discard port");
    let config = HostConfig {
        image: "test-image".to_owned(),
        platform: None,
        docker_privileged: true,
        eosd_path: dir.join("eosd"),
        config_yaml_path: dir.join("config.yml"),
        remote_daemon_dir: PathBuf::from("/eos/runtime"),
        remote_eosd_path: PathBuf::from("/eos/eosd"),
        remote_config_path: PathBuf::from("/eos/config.yml"),
        tcp_port: 9,
        ready_timeout: Duration::from_millis(100),
        request_timeout: Duration::from_millis(100),
        created_by: "test".to_owned(),
        state_dir: dir.clone(),
    };
    let record = SandboxRecord::new(
        "sb-pending-sidecar".to_owned(),
        "sb-pending-sidecar".to_owned(),
        "token".to_owned(),
        9,
        "test".to_owned(),
        Some(endpoint),
    );
    let trace_id = TraceId::parse("trace-pending-sidecar")?;
    let request_id = RequestId::parse("request-pending-sidecar")?;
    store.append_request_start(RequestStartInput {
        sandbox_id: &record.sandbox_id,
        trace_id: trace_id.clone(),
        request_id: request_id.clone(),
        op: "sandbox.runtime.ready",
        family: "Runtime",
        caller_id: Some("caller-1"),
        mutates_state: false,
        args: json!({"caller_id": "caller-1"}),
    })?;

    let args = json!({});
    let mut tcp_line =
        encode_request_with_metadata("sandbox.runtime.ready", request_id.as_str(), &args, None);
    tcp_line.push(b'\n');
    let attempt = ForwardAttempt {
        record: &record,
        config: &config,
        trace_store: &store,
        trace_id: trace_id.clone(),
        request_id: request_id.clone(),
        mutates_state: false,
        tcp_line,
        op: "sandbox.runtime.ready",
        invocation_id: "pending-sidecar",
        args: &args,
    };

    let mut trace_record = TraceRecord::new(trace_id.clone(), SpanUid::ROOT);
    trace_record.request_id = Some(request_id);
    trace_record.events.push(EventRecord::new(
        SpanUid::ROOT,
        "ready_checked",
        "sandbox.runtime",
        json!({"ready": true}),
    ));
    let sidecar = base64::engine::general_purpose::STANDARD
        .encode(encode_trace_batch(&TraceBatch::single(trace_record)));
    let mut response = json!({
        "status": "ok",
        "result": {"ready": true},
        "meta": {"trace": {"event_count": 0}},
        "_trace_events": {
            "schema": DAEMON_TRACE_SIDECAR_SCHEMA,
            "encoding": DAEMON_TRACE_SIDECAR_ENCODING,
            "spool_pending": false,
            "data": sidecar,
        },
    });

    store.fail_next_trace_batch_ingest_for_tests();
    let sidecar = ingest_and_strip_sidecar(&attempt, &mut response);

    assert!(sidecar.present);
    assert!(!sidecar.ingested);
    assert!(sidecar.degraded);
    assert_eq!(store.pending_sidecar_count_for_tests()?, 1);
    assert_eq!(store.recover_pending_sidecars()?, 1);
    assert_eq!(store.pending_sidecar_count_for_tests()?, 0);
    assert_event(
        &store.events_for_trace(trace_id.as_str())?,
        "sandbox.runtime",
        "ready_checked",
    );

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn background_trace_export_ingest_failures_are_spooled_and_recovered() -> Result<()> {
    let trace_id = TraceId::parse("trace-background-drain-pending")?;
    let request_id = RequestId::parse("request-background-drain-pending")?;
    let mut trace_record = TraceRecord::new(trace_id.clone(), SpanUid::ROOT);
    trace_record.request_id = Some(request_id);
    trace_record.events.push(EventRecord::new(
        SpanUid::ROOT,
        "background_finished",
        "daemon.background",
        json!({"kind": "unit"}),
    ));
    let trace_batch = encode_trace_batch(&TraceBatch {
        records: vec![trace_record],
        dropped_traces: 0,
        daemon_boot_id: Some("daemon-boot-drain".to_owned()),
    });
    let response = json!({
        "status": "ok",
        "success": true,
        "record_count": 1,
        "spool_pending_after": 0,
        "dropped_traces": 0,
        "trace_batch_base64": base64::engine::general_purpose::STANDARD.encode(trace_batch),
    });
    let (endpoint, server) = spawn_trace_export_server(response)?;

    let dir = temp_host_dir("background-drain-pending");
    let store = TraceStore::open(&dir)?;
    store.fail_next_trace_batch_ingest_for_tests();
    let target = TraceDrainTarget {
        sandbox_id: "sb-background-drain-pending".to_owned(),
        forward_token: "token".to_owned(),
        record: Arc::new(SandboxRecord::new(
            "sb-background-drain-pending".to_owned(),
            "sb-background-drain-pending".to_owned(),
            "token".to_owned(),
            endpoint.port(),
            "test".to_owned(),
            Some(endpoint),
        )),
        request_timeout: Duration::from_secs(1),
    };

    assert_eq!(drain_trace_export_once(&target, &store)?, 0);
    server.join().expect("trace export server")?;
    assert_eq!(store.pending_sidecar_count_for_tests()?, 1);
    assert_event(
        &store.events_for_trace(trace_id.as_str())?,
        "host.trace_drain",
        "trace_batch_ingest_failed",
    );
    assert_eq!(store.recover_pending_sidecars()?, 1);
    assert_eq!(store.pending_sidecar_count_for_tests()?, 0);
    assert_event(
        &store.events_for_trace(trace_id.as_str())?,
        "daemon.background",
        "background_finished",
    );

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn trace_export_heartbeat_records_remaining_spool_depth() -> Result<()> {
    let response = json!({
        "status": "ok",
        "success": true,
        "record_count": 1,
        "spool_pending_after": 0,
        "dropped_traces": 7,
    });
    let (endpoint, server) = spawn_trace_export_server(response)?;

    let dir = temp_host_dir("trace-export-spool-depth");
    let store = TraceStore::open(&dir)?;
    let target = TraceDrainTarget {
        sandbox_id: "sb-trace-export-spool-depth".to_owned(),
        forward_token: "token".to_owned(),
        record: Arc::new(SandboxRecord::new(
            "sb-trace-export-spool-depth".to_owned(),
            "sb-trace-export-spool-depth".to_owned(),
            "token".to_owned(),
            endpoint.port(),
            "test".to_owned(),
            Some(endpoint),
        )),
        request_timeout: Duration::from_secs(1),
    };

    assert_eq!(drain_trace_export_once(&target, &store)?, 1);
    server.join().expect("trace export server")?;
    let conn = rusqlite::Connection::open(store.db_path())?;
    let (spool_pending, spool_dropped_total): (i64, i64) = conn.query_row(
        "SELECT spool_pending, spool_dropped_total
         FROM sandbox_heartbeats
         WHERE sandbox_id=?1
         ORDER BY ts_ms DESC
         LIMIT 1",
        [&target.sandbox_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(spool_pending, 0);
    assert_eq!(spool_dropped_total, 7);

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn trace_export_ack_is_sent_after_durable_ingest() -> Result<()> {
    let trace_id = TraceId::parse("trace-background-ack")?;
    let mut trace_record = TraceRecord::new(trace_id.clone(), SpanUid::ROOT);
    trace_record.events.push(EventRecord::new(
        SpanUid::ROOT,
        "background_finished",
        "daemon.background",
        json!({"kind": "unit"}),
    ));
    let trace_batch = encode_trace_batch(&TraceBatch {
        records: vec![trace_record],
        dropped_traces: 0,
        daemon_boot_id: Some("daemon-boot-ack".to_owned()),
    });
    let batch_sha256 = trace::sha256_hex(&trace_batch);
    let response = json!({
        "status": "ok",
        "success": true,
        "record_count": 1,
        "spool_pending_after": 1,
        "dropped_traces": 0,
        "export_id": "export-ack-1",
        "batch_sha256": batch_sha256,
        "trace_batch_base64": base64::engine::general_purpose::STANDARD.encode(trace_batch),
    });
    let (endpoint, server) =
        spawn_trace_export_ack_server(response, "export-ack-1", &batch_sha256, 1)?;

    let dir = temp_host_dir("trace-export-ack");
    let store = TraceStore::open(&dir)?;
    let target = TraceDrainTarget {
        sandbox_id: "sb-trace-export-ack".to_owned(),
        forward_token: "token".to_owned(),
        record: Arc::new(SandboxRecord::new(
            "sb-trace-export-ack".to_owned(),
            "sb-trace-export-ack".to_owned(),
            "token".to_owned(),
            endpoint.port(),
            "test".to_owned(),
            Some(endpoint),
        )),
        request_timeout: Duration::from_secs(1),
    };

    assert_eq!(drain_trace_export_once(&target, &store)?, 1);
    server.join().expect("trace export ack server")?;
    assert_event(
        &store.events_for_trace(trace_id.as_str())?,
        "daemon.background",
        "background_finished",
    );
    let conn = rusqlite::Connection::open(store.db_path())?;
    let acked_at_ms: Option<i64> = conn.query_row(
        "SELECT acked_at_ms FROM trace_export_batches WHERE export_id='export-ack-1'",
        [],
        |row| row.get(0),
    )?;
    assert!(acked_at_ms.is_some());

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn decode_client_errors_do_not_format_raw_response_body() {
    let raw = "{\"status\":\"error\",\"token\":\"super-secret\"";
    let source = serde_json::from_str::<Value>(raw).expect_err("invalid json");
    let error = ClientError::Decode {
        raw_len: raw.len(),
        raw_sha256: trace::sha256_hex(raw.as_bytes()),
        source,
    };
    let message = error.to_string();

    assert!(
        !message.contains("super-secret"),
        "decode error display must not expose daemon response bytes: {message}"
    );
    assert!(
        message.contains("raw_len=") && message.contains("raw_sha256="),
        "decode error display should preserve non-secret diagnostics: {message}"
    );
}

#[test]
fn tcp_once_records_transport_failure_events() -> Result<()> {
    let cases = [
        (
            "empty-response",
            "empty_response",
            Box::new(|stream: std::net::TcpStream| {
                let _ = stream.shutdown(std::net::Shutdown::Write);
                std::thread::sleep(Duration::from_millis(50));
            }) as Box<dyn FnOnce(std::net::TcpStream) + Send>,
        ),
        (
            "decode-failed",
            "decode_failed",
            Box::new(|mut stream: std::net::TcpStream| {
                let _ = writeln!(stream, "not json");
            }),
        ),
        (
            "read-timeout",
            "read_failed",
            Box::new(|_stream: std::net::TcpStream| {
                std::thread::sleep(Duration::from_millis(250));
            }),
        ),
    ];

    for (name, expected_event, handler) in cases {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = listener.local_addr()?;
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                handler(stream);
            }
        });
        let (store, trace_id) = run_tcp_once_failure(name, endpoint)?;
        let events = store.events_for_trace(trace_id.as_str())?;
        assert!(
            events
                .iter()
                .any(|event| event.module == "host.transport" && event.event == expected_event),
            "{name}: {events:?}"
        );
    }

    let endpoint = "127.0.0.1:9".parse().expect("discard port");
    let (store, trace_id) = run_tcp_once_failure("connect-refused", endpoint)?;
    let events = store.events_for_trace(trace_id.as_str())?;
    assert!(
        events
            .iter()
            .any(|event| event.module == "host.transport" && event.event == "connect_failed"),
        "{events:?}"
    );
    Ok(())
}

#[test]
fn mutating_response_persistence_failure_does_not_return_success() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = listener.local_addr()?;
    let server = std::thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&json!({
                "status": "ok",
                "result": {"written": true},
                "meta": {"trace": {"event_count": 0}},
            }))?
        )?;
        Ok(())
    });

    let dir = temp_host_dir("response-persist-failure");
    let store = TraceStore::open(&dir)?;
    store.fail_next_response_persisted_for_tests();
    let config = HostConfig {
        image: "test-image".to_owned(),
        platform: None,
        docker_privileged: true,
        eosd_path: dir.join("eosd"),
        config_yaml_path: dir.join("config.yml"),
        remote_daemon_dir: PathBuf::from("/eos/runtime"),
        remote_eosd_path: PathBuf::from("/eos/eosd"),
        remote_config_path: PathBuf::from("/eos/config.yml"),
        tcp_port: endpoint.port(),
        ready_timeout: Duration::from_millis(100),
        request_timeout: Duration::from_millis(100),
        created_by: "test".to_owned(),
        state_dir: dir.clone(),
    };
    let record = SandboxRecord::new(
        "sb-response-persist-failure".to_owned(),
        "sb-response-persist-failure".to_owned(),
        "token".to_owned(),
        endpoint.port(),
        "test".to_owned(),
        Some(endpoint),
    );
    let trace_id = TraceId::parse("trace-response-persist-failure")?;
    let request_id = RequestId::parse("request-response-persist-failure")?;
    let args = json!({});
    let mut tcp_line =
        encode_request_with_metadata("sandbox.file.write", request_id.as_str(), &args, None);
    tcp_line.push(b'\n');
    let attempt = ForwardAttempt {
        record: &record,
        config: &config,
        trace_store: &store,
        trace_id,
        request_id,
        mutates_state: true,
        tcp_line,
        op: "sandbox.file.write",
        invocation_id: "response-persist-failure",
        args: &args,
    };

    let error = tcp_once(&attempt, endpoint, 0).expect_err("must not return ordinary success");
    assert!(
        matches!(error, ClientError::Io(_)),
        "expected trace persistence failure as client error, got {error:?}"
    );

    server.join().expect("server thread")?;
    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn host_lifecycle_response_persistence_failure_does_not_return_success() -> Result<()> {
    let dir = temp_host_dir("host-lifecycle-response-persist-failure");
    let registry = Arc::new(SandboxRegistry::open(dir.clone())?);
    registry.insert(SandboxRecord::new(
        "sb-lifecycle-persist-failure".to_owned(),
        "sb-lifecycle-persist-failure".to_owned(),
        "token".to_owned(),
        37_657,
        "test".to_owned(),
        None,
    ))?;
    let trace_store = Arc::new(TraceStore::open(&dir)?);
    trace_store.fail_next_response_persisted_for_tests();
    let host = SandboxHost {
        config: HostConfig {
            image: "test-image".to_owned(),
            platform: None,
            docker_privileged: true,
            eosd_path: dir.join("eosd"),
            config_yaml_path: dir.join("config.yml"),
            remote_daemon_dir: PathBuf::from("/eos/runtime"),
            remote_eosd_path: PathBuf::from("/eos/eosd"),
            remote_config_path: PathBuf::from("/eos/config.yml"),
            tcp_port: 37_657,
            ready_timeout: Duration::from_millis(100),
            request_timeout: Duration::from_millis(100),
            created_by: "test".to_owned(),
            state_dir: dir.clone(),
        },
        config_yaml: String::new(),
        registry,
        trace_store: Arc::clone(&trace_store),
        trace_drainer: TraceExportDrainer::default(),
    };
    let trace = ForwardTraceContext::new("lifecycle-persist-failure");

    let error = host
        .release_with_trace("sb-lifecycle-persist-failure", &trace, &json!({}))
        .expect_err("release must not return ordinary success after trace persistence fails");
    assert!(
        error
            .to_string()
            .contains("host response persistence failed after lifecycle result"),
        "unexpected lifecycle persistence error: {error:#}"
    );

    let request = trace_store
        .request_by_id(trace.request_id.as_str())?
        .expect("release request row remains queryable");
    assert_eq!(request.status.as_deref(), Some("uncertain"));
    assert_eq!(
        request.error_kind.as_deref(),
        Some("trace_response_persist_failed")
    );

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
#[cfg(unix)]
fn release_keeps_registry_entry_when_container_removal_fails() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_host_dir("release-removal-failure");
    let registry = Arc::new(SandboxRegistry::open(dir.clone())?);
    registry.insert(SandboxRecord::new(
        "sb-release-failure".to_owned(),
        "sb-release-failure".to_owned(),
        "token".to_owned(),
        37_657,
        "test".to_owned(),
        None,
    ))?;
    let trace_store = Arc::new(TraceStore::open(&dir)?);
    let host = SandboxHost {
        config: HostConfig {
            image: "test-image".to_owned(),
            platform: None,
            docker_privileged: true,
            eosd_path: dir.join("eosd"),
            config_yaml_path: dir.join("config.yml"),
            remote_daemon_dir: PathBuf::from("/eos/runtime"),
            remote_eosd_path: PathBuf::from("/eos/eosd"),
            remote_config_path: PathBuf::from("/eos/config.yml"),
            tcp_port: 37_657,
            ready_timeout: Duration::from_millis(100),
            request_timeout: Duration::from_millis(100),
            created_by: "test".to_owned(),
            state_dir: dir.clone(),
        },
        config_yaml: String::new(),
        registry: Arc::clone(&registry),
        trace_store: Arc::clone(&trace_store),
        trace_drainer: TraceExportDrainer::default(),
    };
    let docker = dir.join("docker");
    fs::write(
        &docker,
        "#!/bin/sh\necho simulated docker removal failure >&2\nexit 42\n",
    )?;
    let mut permissions = fs::metadata(&docker)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&docker, permissions)?;
    let _docker = override_docker_command_for_tests(docker);
    let trace = ForwardTraceContext::new("release-removal-failure");

    let error = host
        .release_with_trace("sb-release-failure", &trace, &json!({}))
        .expect_err("container removal failure must be reported");
    assert!(
        error
            .to_string()
            .contains("remove sandbox container sb-release-failure"),
        "unexpected release failure: {error:#}"
    );
    assert!(
        registry.get("sb-release-failure").is_some(),
        "registry entry must remain retryable after cleanup failure"
    );
    assert_eq!(registry.load_token("sb-release-failure")?, "token");
    let request = trace_store
        .request_by_id(trace.request_id.as_str())?
        .expect("release request row remains queryable");
    assert_eq!(request.status.as_deref(), Some("error"));
    assert_eq!(request.error_kind.as_deref(), Some("sandbox_unavailable"));

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn operator_trace_queries_are_audit_events_even_without_sandbox_id() -> Result<()> {
    let dir = temp_host_dir("operator-trace-read-audit");
    let trace_store = Arc::new(TraceStore::open(&dir)?);
    let host = SandboxHost {
        config: HostConfig {
            image: "test-image".to_owned(),
            platform: None,
            docker_privileged: true,
            eosd_path: dir.join("eosd"),
            config_yaml_path: dir.join("config.yml"),
            remote_daemon_dir: PathBuf::from("/eos/runtime"),
            remote_eosd_path: PathBuf::from("/eos/eosd"),
            remote_config_path: PathBuf::from("/eos/config.yml"),
            tcp_port: 37_657,
            ready_timeout: Duration::from_millis(100),
            request_timeout: Duration::from_millis(100),
            created_by: "test".to_owned(),
            state_dir: dir.clone(),
        },
        config_yaml: String::new(),
        registry: Arc::new(SandboxRegistry::open(dir.clone())?),
        trace_store: Arc::clone(&trace_store),
        trace_drainer: TraceExportDrainer::default(),
    };
    let trace = ForwardTraceContext::new("operator-trace-read");

    let response = host.trace_requests(&trace, &json!({"limit": 5}))?;
    assert_eq!(response["requests"], json!([]));

    let events = trace_store.events_for_trace(trace.trace_id.as_str())?;
    let event = events
        .iter()
        .find(|event| event.module == "host.trace_query" && event.event == "operator_read")
        .expect("operator trace read is audit-backed");
    let details: Value = serde_json::from_str(&event.details_json)?;
    assert_eq!(details["op"], json!("host.trace.requests"));
    assert_eq!(details["outcome"]["status"], json!("ok"));
    assert_eq!(details["outcome"]["result_count"], json!(0));

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn trace_show_applies_section_limit_and_reports_truncation() -> Result<()> {
    let dir = temp_host_dir("trace-show-limit");
    let trace_store = Arc::new(TraceStore::open(&dir)?);
    let host = SandboxHost {
        config: HostConfig {
            image: "test-image".to_owned(),
            platform: None,
            docker_privileged: true,
            eosd_path: dir.join("eosd"),
            config_yaml_path: dir.join("config.yml"),
            remote_daemon_dir: PathBuf::from("/eos/runtime"),
            remote_eosd_path: PathBuf::from("/eos/eosd"),
            remote_config_path: PathBuf::from("/eos/config.yml"),
            tcp_port: 37_657,
            ready_timeout: Duration::from_millis(100),
            request_timeout: Duration::from_millis(100),
            created_by: "test".to_owned(),
            state_dir: dir.clone(),
        },
        config_yaml: String::new(),
        registry: Arc::new(SandboxRegistry::open(dir.clone())?),
        trace_store: Arc::clone(&trace_store),
        trace_drainer: TraceExportDrainer::default(),
    };
    let shown_trace = TraceId::parse("trace-show-limit")?;
    for index in 0..3 {
        trace_store.append_trace_event_or_loss(TraceEventInput {
            sandbox_id: "sb-trace-show-limit",
            trace_id: &shown_trace,
            request_id: None,
            span_id: None,
            module: "trace.show.test",
            event: "row",
            details: json!({"index": index}),
        })?;
    }

    let operator_trace = ForwardTraceContext::new("operator-trace-show-limit");
    let response = host.trace_show(
        &operator_trace,
        &json!({"trace_id": shown_trace.as_str(), "limit": 2}),
    )?;

    assert_eq!(response["limits"]["per_section"], json!(2));
    assert_eq!(response["counts"]["events"], json!(2));
    assert_eq!(response["events"].as_array().map_or(0, Vec::len), 2);
    assert_eq!(response["truncated"]["events"], json!(true));
    assert_eq!(response["audit_entries"].as_array().map_or(0, Vec::len), 2);
    assert_eq!(response["truncated"]["audit_entries"], json!(true));

    let events = trace_store.events_for_trace(operator_trace.trace_id.as_str())?;
    let event = events
        .iter()
        .find(|event| event.module == "host.trace_query" && event.event == "operator_read")
        .expect("trace show read is audit-backed");
    let details: Value = serde_json::from_str(&event.details_json)?;
    assert_eq!(details["outcome"]["limit"], json!(2));
    assert_eq!(details["outcome"]["truncated"]["events"], json!(true));

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn startup_pending_sidecar_recovery_is_bounded() -> Result<()> {
    let dir = temp_host_dir("startup-pending-sidecar-limit");
    let store = TraceStore::open(&dir)?;
    let limit = TraceStore::startup_pending_sidecar_recovery_limit_for_tests();
    for index in 0..(limit + 1) {
        let trace_id = TraceId::parse(format!("trace-pending-startup-{index}"))?;
        let request_id = RequestId::parse(format!("request-pending-startup-{index}"))?;
        let mut record = TraceRecord::new(trace_id.clone(), SpanUid::ROOT);
        record.request_id = Some(request_id.clone());
        record.events.push(EventRecord::new(
            SpanUid::ROOT,
            "pending_recovered",
            "trace.startup.test",
            json!({"index": index}),
        ));
        let batch = encode_trace_batch(&TraceBatch::single(record));
        store.record_pending_sidecar(PendingSidecarInput {
            sandbox_id: "sb-pending-startup-limit",
            trace_id: &trace_id,
            request_id: &request_id,
            batch_bytes: &batch,
            error: "seeded pending sidecar",
        })?;
    }
    drop(store);

    let reopened = TraceStore::open(&dir)?;
    assert_eq!(reopened.pending_sidecar_count_for_tests()?, 1);
    assert_eq!(reopened.recover_pending_sidecars()?, 1);
    assert_eq!(reopened.pending_sidecar_count_for_tests()?, 0);

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

#[test]
fn host_transport_records_retry_endpoint_refresh_write_and_connect_timeout_facts() -> Result<()> {
    let dir = temp_host_dir("transport-edge-facts");
    let store = TraceStore::open(&dir)?;
    let endpoint: std::net::SocketAddr = "127.0.0.1:9".parse().expect("discard port");
    let refreshed_endpoint: std::net::SocketAddr = "127.0.0.1:10".parse().expect("refresh port");
    let config = HostConfig {
        image: "test-image".to_owned(),
        platform: None,
        docker_privileged: true,
        eosd_path: dir.join("eosd"),
        config_yaml_path: dir.join("config.yml"),
        remote_daemon_dir: PathBuf::from("/eos/runtime"),
        remote_eosd_path: PathBuf::from("/eos/eosd"),
        remote_config_path: PathBuf::from("/eos/config.yml"),
        tcp_port: endpoint.port(),
        ready_timeout: Duration::from_millis(100),
        request_timeout: Duration::from_millis(100),
        created_by: "test".to_owned(),
        state_dir: dir.clone(),
    };
    let record = SandboxRecord::new(
        "sb-transport-edges".to_owned(),
        "sb-transport-edges".to_owned(),
        "token".to_owned(),
        endpoint.port(),
        "test".to_owned(),
        Some(endpoint),
    );
    let trace_id = TraceId::parse("trace-transport-edges")?;
    let request_id = RequestId::parse("request-transport-edges")?;
    let args = json!({});
    let mut tcp_line =
        encode_request_with_metadata("sandbox.runtime.ready", request_id.as_str(), &args, None);
    tcp_line.push(b'\n');
    let attempt = ForwardAttempt {
        record: &record,
        config: &config,
        trace_store: &store,
        trace_id: trace_id.clone(),
        request_id,
        mutates_state: false,
        tcp_line,
        op: "sandbox.runtime.ready",
        invocation_id: "transport-edge-test",
        args: &args,
    };

    let write_failure = ClientError::Write(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "closed",
    ));
    record_client_error(&attempt, endpoint, 0, Instant::now(), &write_failure);
    let connect_timeout = ClientError::Connect {
        addr: endpoint,
        source: std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"),
    };
    record_client_error(&attempt, endpoint, 1, Instant::now(), &connect_timeout);
    record_endpoint_refreshed(&attempt, endpoint, refreshed_endpoint);
    let _ = tcp_with_connect_backoff(&attempt, endpoint);

    let events = store.events_for_trace(trace_id.as_str())?;
    assert_event(&events, "host.transport", "write_failed");
    assert_event(&events, "host.transport", "connect_timeout");
    assert_event(&events, "host.transport", "endpoint_refreshed");
    assert_event(&events, "host.transport", "retry_scheduled");
    assert!(
        events.iter().any(|event| {
            if event.module != "host.transport" || event.event != "connect_timeout" {
                return false;
            }
            serde_json::from_str::<serde_json::Value>(&event.details_json)
                .ok()
                .and_then(|details| details.get("error_kind").cloned())
                == Some(json!("connect_timeout"))
        }),
        "connect_timeout details missing: {events:?}"
    );

    let _ = fs::remove_dir_all(dir);
    Ok(())
}

fn run_tcp_once_failure(
    name: &str,
    endpoint: std::net::SocketAddr,
) -> Result<(TraceStore, TraceId)> {
    let dir = temp_host_dir(name);
    let store = TraceStore::open(&dir)?;
    let config = HostConfig {
        image: "test-image".to_owned(),
        platform: None,
        docker_privileged: true,
        eosd_path: dir.join("eosd"),
        config_yaml_path: dir.join("config.yml"),
        remote_daemon_dir: PathBuf::from("/eos/runtime"),
        remote_eosd_path: PathBuf::from("/eos/eosd"),
        remote_config_path: PathBuf::from("/eos/config.yml"),
        tcp_port: endpoint.port(),
        ready_timeout: Duration::from_millis(100),
        request_timeout: Duration::from_millis(100),
        created_by: "test".to_owned(),
        state_dir: dir,
    };
    let record = SandboxRecord::new(
        format!("sb-{name}"),
        format!("sb-{name}"),
        "token".to_owned(),
        endpoint.port(),
        "test".to_owned(),
        Some(endpoint),
    );
    let trace_id = TraceId::parse(format!("trace-{name}")).expect("trace id");
    let request_id = RequestId::parse(format!("request-{name}")).expect("request id");
    let args = json!({});
    let mut tcp_line =
        encode_request_with_metadata("sandbox.runtime.ready", request_id.as_str(), &args, None);
    tcp_line.push(b'\n');
    let attempt = ForwardAttempt {
        record: &record,
        config: &config,
        trace_store: &store,
        trace_id: trace_id.clone(),
        request_id,
        mutates_state: false,
        tcp_line,
        op: "sandbox.runtime.ready",
        invocation_id: "failure-test",
        args: &args,
    };
    let _ = tcp_once(&attempt, endpoint, 0);
    Ok((store, trace_id))
}

fn assert_event(events: &[TraceEventRow], module: &str, event: &str) {
    assert!(
        events
            .iter()
            .any(|row| row.module == module && row.event == event),
        "missing {module}/{event}: {events:?}"
    );
}

fn spawn_trace_export_server(response: Value) -> Result<(SocketAddr, JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = listener.local_addr()?;
    let server = std::thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let request: Value = serde_json::from_str(line.trim_end())?;
        assert_eq!(request["op"], json!("sandbox.trace.export"));
        writeln!(stream, "{}", serde_json::to_string(&response)?)?;
        Ok(())
    });
    Ok((endpoint, server))
}

fn spawn_trace_export_ack_server(
    response: Value,
    export_id: &str,
    batch_sha256: &str,
    record_count: u64,
) -> Result<(SocketAddr, JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = listener.local_addr()?;
    let export_id = export_id.to_owned();
    let batch_sha256 = batch_sha256.to_owned();
    let server = std::thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let request: Value = serde_json::from_str(line.trim_end())?;
        assert_eq!(request["op"], json!("sandbox.trace.export"));
        writeln!(stream, "{}", serde_json::to_string(&response)?)?;

        let (mut stream, _) = listener.accept()?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        let request: Value = serde_json::from_str(line.trim_end())?;
        assert_eq!(request["op"], json!("sandbox.trace.export_ack"));
        assert_eq!(request["args"]["export_id"], json!(export_id));
        assert_eq!(request["args"]["batch_sha256"], json!(batch_sha256));
        assert_eq!(request["args"]["record_count"], json!(record_count));
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&json!({
                "status": "ok",
                "success": true,
                "export_id": export_id,
                "acked": true,
            }))?
        )?;
        Ok(())
    });
    Ok((endpoint, server))
}

fn temp_host_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("eos-host-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp host dir");
    dir
}

fn assert_ordered_events(actual: &[(&str, &str)], expected: &[(&str, &str)]) {
    let mut next = 0;
    for event in actual {
        if expected.get(next) == Some(event) {
            next += 1;
        }
    }
    assert_eq!(
        next,
        expected.len(),
        "missing ordered replay suffix starting at {:?}; actual events: {actual:?}",
        expected.get(next)
    );
}
