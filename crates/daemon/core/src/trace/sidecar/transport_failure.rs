use base64::Engine as _;
use serde_json::{json, Value};
use trace::{
    decode_trace_batch, EventRecord, SpanKind, SpanRecord, SpanUid, TraceRecord,
    TRACE_SIDECAR_ENCODING, TRACE_SIDECAR_FIELD, TRACE_SIDECAR_SCHEMA,
};

use super::{now_ms, push_background_record};

pub(crate) fn push_transport_failure_from_sidecar(
    response: &Value,
    event_name: &str,
    error: &std::io::Error,
) {
    let Some(bytes) = trace_sidecar_bytes(response) else {
        return;
    };
    let Ok(batch) = decode_trace_batch(&bytes) else {
        return;
    };
    let Some(source) = batch.records.first() else {
        return;
    };
    let now = now_ms();
    let mut span = SpanRecord::new(
        SpanUid::ROOT,
        None,
        "daemon.transport.failure",
        SpanKind::DaemonTransport,
        json!({"source": "response_sidecar"}),
    );
    span.started_at_unix_ms = now;
    span.finished_at_unix_ms = now;
    let mut event = EventRecord::new(
        SpanUid::ROOT,
        event_name,
        "daemon.transport",
        json!({
            "error_kind": format!("{:?}", error.kind()),
            "error": error.to_string(),
        }),
    );
    event.at_unix_ms = now;

    let mut record = TraceRecord::new(source.trace_id.clone(), SpanUid::ROOT);
    record.request_id = source.request_id.clone();
    record.started_at_unix_ms = now;
    record.finished_at_unix_ms = now;
    record.spans.push(span);
    record.events.push(event);
    push_background_record(record);
}

pub(super) fn trace_sidecar_bytes(response: &Value) -> Option<Vec<u8>> {
    let sidecar = response.get(TRACE_SIDECAR_FIELD)?;
    let encoded = match sidecar {
        Value::Object(object) => {
            if object.get("schema").and_then(Value::as_str) != Some(TRACE_SIDECAR_SCHEMA) {
                return None;
            }
            if object.get("encoding").and_then(Value::as_str) != Some(TRACE_SIDECAR_ENCODING) {
                return None;
            }
            object.get("data").and_then(Value::as_str)?
        }
        _ => return None,
    };
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}
