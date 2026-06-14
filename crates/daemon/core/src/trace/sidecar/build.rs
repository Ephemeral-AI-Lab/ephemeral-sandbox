use base64::Engine as _;
use serde_json::{json, Value};
use trace::{
    encode_trace_batch, EventRecord, RequestId, SpanKind, SpanRecord, SpanUid, TraceBatch, TraceId,
    TraceRecord, TRACE_SIDECAR_ENCODING, TRACE_SIDECAR_FIELD, TRACE_SIDECAR_SCHEMA,
};

use super::budget::enforce_sidecar_record_budget;
use super::events::{
    child_spans_from_request_events, op_family, op_span_name, op_verb, request_event_span_id,
};
use super::resources::resource_stats_from_event;
use super::{
    daemon_boot_id, now_ms, stamp_pending_envelope_meta, RequestTraceContext, RequestTraceEvent,
    RequestTraceFacts,
};

pub(crate) fn attach_request_sidecar(
    response: Value,
    trace: Option<&RequestTraceContext>,
    op: &str,
    facts: &RequestTraceFacts,
) -> Value {
    attach_request_sidecar_with_events(response, trace, op, facts, &[])
}

pub(crate) fn attach_request_sidecar_with_events(
    mut response: Value,
    trace: Option<&RequestTraceContext>,
    op: &str,
    facts: &RequestTraceFacts,
    request_events: &[RequestTraceEvent],
) -> Value {
    let response_bytes = serde_json::to_vec(&response).map_or(0, |bytes| bytes.len());
    let Some(object) = response.as_object_mut() else {
        return response;
    };
    let trace_id = trace
        .and_then(|trace| TraceId::parse(trace.trace_id.clone()).ok())
        .unwrap_or_default();
    let request_id = trace
        .and_then(|trace| RequestId::parse(trace.request_id.clone()).ok())
        .unwrap_or_default();
    let capture_budget_version = trace.map_or(1, |trace| trace.capture_budget_version);
    let now = now_ms();
    let started_at = facts.accepted_at_unix_ms.min(now);
    let duration_us = now.saturating_sub(started_at).saturating_mul(1000);

    let mut root = SpanRecord::new(
        SpanUid::ROOT,
        None,
        "op_request",
        SpanKind::OpRequest,
        json!({
            "op": op,
            "capture_budget_version": capture_budget_version,
            "connection_id": facts.connection_id,
            "listener_kind": facts.listener_kind,
            "peer_addr": facts.peer_addr,
            "local_addr": facts.local_addr,
            "request_bytes": facts.request_bytes,
        }),
    );
    root.started_at_unix_ms = started_at;
    root.finished_at_unix_ms = now;
    root.duration_us = duration_us;
    let mut transport = SpanRecord::new(
        SpanUid::new(2),
        Some(SpanUid::ROOT),
        "daemon.transport",
        SpanKind::DaemonTransport,
        json!({
            "connection_id": facts.connection_id,
            "listener_kind": facts.listener_kind,
            "peer_addr": facts.peer_addr,
            "local_addr": facts.local_addr,
        }),
    );
    transport.started_at_unix_ms = started_at;
    transport.finished_at_unix_ms = now;
    transport.duration_us = duration_us;
    let mut dispatch = SpanRecord::new(
        SpanUid::new(3),
        Some(SpanUid::ROOT),
        "dispatch",
        SpanKind::Dispatch,
        json!({"op": op}),
    );
    dispatch.started_at_unix_ms = now;
    dispatch.finished_at_unix_ms = now;
    let mut operation = SpanRecord::new(
        SpanUid::new(4),
        Some(SpanUid::new(3)),
        op_span_name(op),
        SpanKind::Operation,
        json!({"op": op, "family": op_family(op), "verb": op_verb(op)}),
    );
    operation.started_at_unix_ms = now;
    operation.finished_at_unix_ms = now;
    let child_spans = child_spans_from_request_events(request_events, now);

    let mut events = vec![
        EventRecord::new(
            SpanUid::new(2),
            "accepted",
            "daemon.transport",
            json!({
                "connection_id": facts.connection_id,
                "listener_kind": facts.listener_kind,
                "peer_addr": facts.peer_addr,
                "local_addr": facts.local_addr,
                "daemon_boot_id": daemon_boot_id().to_string(),
            }),
        ),
        EventRecord::new(
            SpanUid::new(2),
            "read_started",
            "daemon.transport",
            json!({
                "connection_id": facts.connection_id,
                "is_tcp": facts.is_tcp,
            }),
        ),
        EventRecord::new(
            SpanUid::new(2),
            "read_finished",
            "daemon.transport",
            json!({
                "connection_id": facts.connection_id,
                "is_tcp": facts.is_tcp,
                "request_bytes": facts.request_bytes,
                "read_duration_us": facts.read_duration_us,
            }),
        ),
        EventRecord::new(
            SpanUid::new(2),
            "auth_checked",
            "daemon.transport",
            json!({
                "connection_id": facts.connection_id,
                "auth_required": facts.auth_required,
                "auth_ok": facts.auth_ok,
            }),
        ),
        EventRecord::new(
            SpanUid::new(2),
            "decoded",
            "daemon.transport",
            json!({
                "connection_id": facts.connection_id,
                "protocol_version": facts.protocol_version,
            }),
        ),
        EventRecord::new(
            SpanUid::new(3),
            "dispatch_started",
            "daemon.dispatch",
            json!({"op": op}),
        ),
        EventRecord::new(
            SpanUid::new(3),
            "op_resolved",
            "daemon.dispatch",
            json!({"op": op, "family": op_family(op), "verb": op_verb(op)}),
        ),
        EventRecord::new(
            SpanUid::new(3),
            "parse_finished",
            "daemon.dispatch",
            json!({
                "op": op,
                "success": true,
                "protocol_version": facts.protocol_version,
            }),
        ),
    ];
    if !request_events
        .iter()
        .any(|event| event.module == "workspace.route" && event.name == "route_selected")
    {
        events.push(EventRecord::new(
            SpanUid::new(4),
            "route_selected",
            "workspace.route",
            json!({"kind": "none", "reason": "no_route_recorded"}),
        ));
    }
    events.extend(request_events.iter().map(|event| {
        EventRecord::new(
            request_event_span_id(event),
            event.name.clone(),
            event.module.clone(),
            event.details.clone(),
        )
    }));
    events.extend([
        EventRecord::new(
            SpanUid::new(3),
            "dispatch_finished",
            "daemon.dispatch",
            json!({"op": op}),
        ),
        EventRecord::new(
            SpanUid::new(2),
            "response_write_started",
            "daemon.transport",
            json!({"connection_id": facts.connection_id, "response_bytes": response_bytes}),
        ),
        EventRecord::new(
            SpanUid::new(2),
            "response_write_finished",
            "daemon.transport",
            json!({"connection_id": facts.connection_id, "response_bytes": response_bytes}),
        ),
        EventRecord::new(
            SpanUid::new(2),
            "shutdown_finished",
            "daemon.transport",
            json!({"connection_id": facts.connection_id}),
        ),
    ]);
    for event in &mut events {
        event.at_unix_ms = now;
    }

    let mut record = TraceRecord::new(trace_id, SpanUid::ROOT);
    record.request_id = Some(request_id);
    record.started_at_unix_ms = started_at;
    record.finished_at_unix_ms = now;
    record.spans = vec![root, transport, dispatch, operation];
    record.spans.extend(child_spans);
    record.resources = request_events
        .iter()
        .filter_map(resource_stats_from_event)
        .collect();
    record.events = events;
    enforce_sidecar_record_budget(&mut record);
    stamp_pending_envelope_meta(object, &record, op, duration_us);

    let mut batch = TraceBatch::single(record);
    batch.daemon_boot_id = Some(daemon_boot_id().to_string());
    let encoded = encode_trace_batch(&batch);
    object.insert(
        TRACE_SIDECAR_FIELD.to_owned(),
        json!({
            "schema": TRACE_SIDECAR_SCHEMA,
            "encoding": TRACE_SIDECAR_ENCODING,
            "spool_pending": false,
            "data": base64::engine::general_purpose::STANDARD.encode(encoded),
        }),
    );
    response
}
