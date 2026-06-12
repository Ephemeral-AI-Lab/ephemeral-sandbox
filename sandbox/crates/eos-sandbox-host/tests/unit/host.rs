use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use eos_trace::{
    encode_trace_batch, EventRecord, RequestId, SpanKind, SpanRecord, SpanUid, TraceBatch, TraceId,
    TraceRecord,
};
use serde_json::json;

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
        record.events.push(EventRecord::new(
            SpanUid::ROOT,
            "accepted",
            "daemon.transport",
            json!({"listener_kind": "tcp", "request_bytes": line.len()}),
        ));
        let sidecar = base64::engine::general_purpose::STANDARD
            .encode(encode_trace_batch(&TraceBatch::single(record)));
        let response = json!({
            "success": true,
            "ready": true,
            "_trace_events": sidecar,
        });
        writeln!(stream, "{}", serde_json::to_string(&response)?)?;
        Ok(())
    });

    let dir = temp_host_dir("forward-trace");
    let store = Arc::new(TraceStore::open(&dir)?);
    let config = HostConfig {
        image: "test-image".to_owned(),
        platform: None,
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
    let record = SandboxRecord::new(
        "sb-forward".to_owned(),
        "sb-forward".to_owned(),
        "token".to_owned(),
        endpoint.port(),
        "test".to_owned(),
        Some(endpoint),
    );

    let mut trace = ForwardTraceContext::new("request-forward");
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

    let response = forward_request(
        &record,
        &config,
        &store,
        &TraceExportDrainer::default(),
        trace,
        false,
        "sandbox.runtime.ready",
        "request-forward",
        &json!({"caller_id": "caller-1"}),
    )?;
    assert_eq!(response["_trace_events"], serde_json::Value::Null);
    assert_eq!(response["ready"], json!(true));

    let request = store
        .request_by_id("request-forward")?
        .expect("request row");
    assert_eq!(request.status.as_deref(), Some("ok"));
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

    server.join().expect("server thread")?;
    let _ = fs::remove_dir_all(dir);
    Ok(())
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

fn run_tcp_once_failure(
    name: &str,
    endpoint: std::net::SocketAddr,
) -> Result<(TraceStore, TraceId)> {
    let dir = temp_host_dir(name);
    let store = TraceStore::open(&dir)?;
    let config = HostConfig {
        image: "test-image".to_owned(),
        platform: None,
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

fn temp_host_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("eos-host-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp host dir");
    dir
}
