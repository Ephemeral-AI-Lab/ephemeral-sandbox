use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use eos_trace::{
    decode_trace_batch, encode_trace_batch, BootId, BoundedJson, DetailBudget, EventRecord,
    RequestId, ResourceStats, ResourceStatsKind, ResourceStatsMeta, SpanKind, SpanRecord, SpanUid,
    TraceBatch, TraceId, TraceRecord, TraceSpool,
};
use serde_json::{json, Value};

use crate::wire::RequestTraceContext;

pub(crate) const TRACE_SIDECAR_FIELD: &str = "_trace_events";

static CONNECTION_SEQ: AtomicU64 = AtomicU64::new(1);
static BACKGROUND_SPOOL: OnceLock<Mutex<TraceSpool>> = OnceLock::new();
static DAEMON_BOOT_ID: OnceLock<BootId> = OnceLock::new();

pub(crate) fn daemon_boot_id() -> &'static BootId {
    DAEMON_BOOT_ID.get_or_init(BootId::new)
}

#[derive(Debug, Clone)]
pub(crate) struct RequestTraceFacts {
    pub connection_id: String,
    pub accepted_at_unix_ms: u64,
    pub listener_kind: &'static str,
    pub peer_addr: Option<String>,
    pub local_addr: Option<String>,
    pub is_tcp: bool,
    pub request_bytes: usize,
    pub read_duration_us: u64,
    pub auth_required: bool,
    pub auth_ok: bool,
    pub protocol_version: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct RequestTraceEvent {
    pub(crate) span_id: SpanUid,
    pub(crate) name: String,
    pub(crate) module: String,
    pub(crate) details: Value,
}

impl RequestTraceEvent {
    pub(crate) fn operation(
        module: impl Into<String>,
        name: impl Into<String>,
        details: Value,
    ) -> Self {
        Self {
            span_id: SpanUid::new(4),
            name: name.into(),
            module: module.into(),
            details,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RequestTraceEventSink {
    events: Arc<Mutex<Vec<RequestTraceEvent>>>,
}

impl RequestTraceEventSink {
    pub(crate) fn push(&self, event: RequestTraceEvent) {
        self.events
            .lock()
            .expect("request trace event mutex poisoned")
            .push(event);
    }

    pub(crate) fn drain(&self) -> Vec<RequestTraceEvent> {
        self.events
            .lock()
            .expect("request trace event mutex poisoned")
            .drain(..)
            .collect()
    }
}

pub(crate) fn next_connection_id() -> String {
    format!(
        "daemon-conn-{}",
        CONNECTION_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn push_background_record(record: TraceRecord) {
    let _ = background_spool()
        .lock()
        .expect("trace spool mutex poisoned")
        .push(record);
}

pub(crate) fn drain_background_records(max_records: usize) -> (Vec<TraceRecord>, u64) {
    let mut spool = background_spool()
        .lock()
        .expect("trace spool mutex poisoned");
    let records = spool.drain_batch(max_records);
    let dropped = spool.dropped_traces();
    (records, dropped)
}

fn background_spool() -> &'static Mutex<TraceSpool> {
    BACKGROUND_SPOOL.get_or_init(|| Mutex::new(TraceSpool::default()))
}

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
    ];
    if !request_events
        .iter()
        .any(|event| event.module == "workspace.route" && event.name == "route_selected")
    {
        events.push(EventRecord::new(
            SpanUid::new(4),
            "route_selected",
            "workspace.route",
            json!({"kind": "none", "reason": "phase04_no_workspace_route"}),
        ));
    }
    events.extend(request_events.iter().map(|event| {
        EventRecord::new(
            event.span_id,
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
    ]);
    for event in &mut events {
        event.at_unix_ms = now;
    }

    let mut record = TraceRecord::new(trace_id, SpanUid::ROOT);
    record.request_id = Some(request_id);
    record.started_at_unix_ms = started_at;
    record.finished_at_unix_ms = now;
    record.spans = vec![root, transport, dispatch, operation];
    record.resources = request_events
        .iter()
        .filter_map(resource_stats_from_event)
        .collect();
    record.events = events;

    let mut batch = TraceBatch::single(record);
    batch.daemon_boot_id = Some(daemon_boot_id().to_string());
    let encoded = encode_trace_batch(&batch);
    object.insert(
        TRACE_SIDECAR_FIELD.to_owned(),
        Value::String(base64::engine::general_purpose::STANDARD.encode(encoded)),
    );
    response
}

pub(crate) fn push_transport_failure_from_sidecar(
    response: &Value,
    event_name: &str,
    error: &std::io::Error,
) {
    let Some(sidecar) = response.get(TRACE_SIDECAR_FIELD).and_then(Value::as_str) else {
        return;
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(sidecar) else {
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

fn op_family(op: &str) -> &str {
    op.split('.').nth(1).unwrap_or("unknown")
}

fn op_verb(op: &str) -> &str {
    op.rsplit('.').next().unwrap_or("unknown")
}

fn op_span_name(op: &str) -> String {
    format!("op.{}.{}", op_family(op), op_verb(op))
}

fn resource_stats_from_event(event: &RequestTraceEvent) -> Option<ResourceStats> {
    if event.module != "resource" || event.name != "resource_stats" {
        return None;
    }
    let details = event.details.as_object()?;
    let meta = details.get("meta")?.as_object()?;
    let stats_kind = resource_stats_kind(meta.get("stats_kind")?.as_str()?);
    let source = meta.get("source")?.as_str()?.to_owned();
    let source_available = meta
        .get("source_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let phase = meta.get("phase").and_then(Value::as_str).map(str::to_owned);
    let read_error = optional_string(meta.get("read_error"));
    let parse_error = optional_string(meta.get("parse_error"));
    let sampler_duration_us = optional_u64(meta.get("sampler_duration_us")).unwrap_or(0);
    let inflight_requests = optional_u64(meta.get("inflight_requests")).unwrap_or(0);
    let mut payload = event.details.clone();
    if let Some(payload_object) = payload.as_object_mut() {
        payload_object.remove("meta");
    }
    Some(ResourceStats {
        span_id: Some(event.span_id),
        meta: ResourceStatsMeta {
            stats_kind,
            phase,
            source,
            source_available,
            read_error,
            parse_error,
            sampler_duration_us,
            inflight_requests,
        },
        payload: BoundedJson::capture(payload, DetailBudget::EventDetails),
    })
}

fn resource_stats_kind(label: &str) -> ResourceStatsKind {
    match label {
        "tree" => ResourceStatsKind::Tree,
        "host" => ResourceStatsKind::Host,
        "mount_cost" => ResourceStatsKind::MountCost,
        _ => ResourceStatsKind::CgroupProcess,
    }
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn optional_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).or_else(|| {
        value
            .and_then(Value::as_i64)
            .and_then(|value| value.try_into().ok())
    })
}

pub(crate) fn now_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use eos_operation::control::contract::TraceExportInput;
    use eos_trace::decode_trace_batch;

    use super::*;

    #[test]
    fn trace_export_drains_background_spool_as_protobuf_batch() {
        let trace_id = TraceId::parse("trace-export-test").expect("trace id");
        let mut record = TraceRecord::new(trace_id.clone(), SpanUid::ROOT);
        record.events.push(EventRecord::new(
            SpanUid::ROOT,
            "background_finished",
            "daemon.background",
            json!({"kind": "unit"}),
        ));
        push_background_record(record);

        let response =
            crate::op_adapter::control::op_trace_export(TraceExportInput { max_records: 16 });
        assert_eq!(response["success"], true);
        assert_eq!(response["record_count"], 1);
        let encoded = response["trace_batch_base64"]
            .as_str()
            .expect("trace batch");
        let batch = decode_trace_batch(
            &base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("base64"),
        )
        .expect("trace batch decodes");
        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.records[0].trace_id, trace_id);

        let trace = RequestTraceContext {
            trace_id: "trace-write-failed".to_owned(),
            request_id: "request-write-failed".to_owned(),
            parent_span_id: None,
            link_hints: Vec::new(),
            capture_budget_version: 1,
        };
        let facts = RequestTraceFacts {
            connection_id: "daemon-conn-write-failed".to_owned(),
            accepted_at_unix_ms: now_ms(),
            listener_kind: "tcp",
            peer_addr: Some("127.0.0.1:51000".to_owned()),
            local_addr: Some("127.0.0.1:50000".to_owned()),
            is_tcp: true,
            request_bytes: 16,
            read_duration_us: 10,
            auth_required: true,
            auth_ok: true,
            protocol_version: Some(1),
        };
        let response = attach_request_sidecar(
            json!({"success": true}),
            Some(&trace),
            "sandbox.runtime.ready",
            &facts,
        );
        push_transport_failure_from_sidecar(
            &response,
            "response_write_failed",
            &std::io::Error::new(std::io::ErrorKind::BrokenPipe, "peer closed"),
        );
        let response =
            crate::op_adapter::control::op_trace_export(TraceExportInput { max_records: 16 });
        assert_eq!(response["record_count"], 1);
        let encoded = response["trace_batch_base64"]
            .as_str()
            .expect("trace batch");
        let batch = decode_trace_batch(
            &base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("base64"),
        )
        .expect("trace batch decodes");
        let record = batch.records.first().expect("failure record");
        assert_eq!(record.trace_id.as_str(), "trace-write-failed");
        assert_eq!(
            record
                .events
                .first()
                .map(|event| (event.module.as_str(), event.name.as_str())),
            Some(("daemon.transport", "response_write_failed"))
        );
    }

    #[test]
    fn request_sidecar_merges_subsystem_events() {
        let trace = RequestTraceContext {
            trace_id: "trace-checkpoint-events".to_owned(),
            request_id: "request-checkpoint-events".to_owned(),
            parent_span_id: None,
            link_hints: Vec::new(),
            capture_budget_version: 1,
        };
        let facts = RequestTraceFacts {
            connection_id: "daemon-conn-checkpoint-events".to_owned(),
            accepted_at_unix_ms: now_ms(),
            listener_kind: "unix",
            peer_addr: None,
            local_addr: None,
            is_tcp: false,
            request_bytes: 128,
            read_duration_us: 12,
            auth_required: false,
            auth_ok: true,
            protocol_version: Some(1),
        };
        let response = attach_request_sidecar_with_events(
            json!({"success": true}),
            Some(&trace),
            "sandbox.checkpoint.commit_to_git",
            &facts,
            &[
                RequestTraceEvent::operation(
                    "checkpoint",
                    "git_command_finished",
                    json!({"argv_summary": "git add -A -- <paths>", "exit_code": 0, "stderr_tail": ""}),
                ),
                RequestTraceEvent::operation(
                    "workspace.route",
                    "route_selected",
                    json!({"kind": "fast_path", "reason": "unit"}),
                ),
            ],
        );
        let encoded = response[TRACE_SIDECAR_FIELD]
            .as_str()
            .expect("trace sidecar");
        let batch = decode_trace_batch(
            &base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("base64"),
        )
        .expect("trace batch decodes");
        let record = batch.records.first().expect("request trace record");

        assert!(
            record
                .events
                .iter()
                .any(|event| event.module == "checkpoint"
                    && event.name == "git_command_finished"
                    && event.details.value["argv_summary"] == "git add -A -- <paths>"
                    && event.span_id == SpanUid::new(4)),
            "checkpoint event merged into operation span"
        );
        let route_events: Vec<_> = record
            .events
            .iter()
            .filter(|event| event.module == "workspace.route" && event.name == "route_selected")
            .collect();
        assert_eq!(route_events.len(), 1, "real route suppresses fallback");
        assert_eq!(route_events[0].details.value["kind"], "fast_path");
    }

    #[test]
    fn request_sidecar_promotes_resource_stats_events() {
        let trace = RequestTraceContext {
            trace_id: "trace-resource-events".to_owned(),
            request_id: "request-resource-events".to_owned(),
            parent_span_id: None,
            link_hints: Vec::new(),
            capture_budget_version: 1,
        };
        let facts = RequestTraceFacts {
            connection_id: "daemon-conn-resource-events".to_owned(),
            accepted_at_unix_ms: now_ms(),
            listener_kind: "unix",
            peer_addr: None,
            local_addr: None,
            is_tcp: false,
            request_bytes: 96,
            read_duration_us: 8,
            auth_required: false,
            auth_ok: true,
            protocol_version: Some(1),
        };
        let response = attach_request_sidecar_with_events(
            json!({"success": true}),
            Some(&trace),
            "sandbox.command.exec",
            &facts,
            &[
                RequestTraceEvent::operation(
                    "resource",
                    "resource_stats",
                    json!({
                        "meta": {
                            "stats_kind": "cgroup_process",
                            "phase": "after",
                            "source": "command.process.wait",
                            "source_available": true,
                            "sampler_duration_us": 17,
                            "inflight_requests": 2,
                        },
                        "cgroup": {
                            "source_available": true,
                            "cpu": {"usage_usec": 42},
                        },
                        "process": {
                            "source_available": true,
                            "gauges": {"rss_bytes": 4096},
                        },
                    }),
                ),
                RequestTraceEvent::operation(
                    "resource",
                    "resource_stats",
                    json!({
                        "meta": {
                            "stats_kind": "tree",
                            "phase": "after",
                            "source": "resource.command_exec.upperdir",
                            "source_available": true,
                            "sampler_duration_us": 0,
                            "inflight_requests": 2,
                        },
                        "tree": {
                            "bytes": 4096,
                            "file_count": 1,
                            "truncated": 1,
                        },
                    }),
                ),
                RequestTraceEvent::operation(
                    "resource",
                    "resource_stats",
                    json!({
                        "meta": {
                            "stats_kind": "host",
                            "phase": "after",
                            "source": "daemon.process",
                            "source_available": true,
                            "sampler_duration_us": 0,
                            "inflight_requests": 2,
                        },
                        "host": {
                            "process": {
                                "rss_bytes": 4096,
                                "max_rss_bytes": 8192,
                            },
                        },
                    }),
                ),
            ],
        );

        let encoded = response[TRACE_SIDECAR_FIELD]
            .as_str()
            .expect("trace sidecar");
        let batch = decode_trace_batch(
            &base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("base64"),
        )
        .expect("trace batch decodes");
        let record = batch.records.first().expect("request trace record");
        assert_eq!(record.resources.len(), 3);
        let resource = record
            .resources
            .iter()
            .find(|resource| resource.meta.stats_kind == ResourceStatsKind::CgroupProcess)
            .expect("cgroup resource stats");
        assert_eq!(resource.span_id, Some(SpanUid::new(4)));
        assert_eq!(resource.meta.stats_kind, ResourceStatsKind::CgroupProcess);
        assert_eq!(resource.meta.phase.as_deref(), Some("after"));
        assert_eq!(resource.meta.source, "command.process.wait");
        assert!(resource.meta.source_available);
        assert_eq!(resource.meta.sampler_duration_us, 17);
        assert_eq!(resource.meta.inflight_requests, 2);
        assert_eq!(resource.payload.value["cgroup"]["cpu"]["usage_usec"], 42);
        assert_eq!(
            resource.payload.value["process"]["gauges"]["rss_bytes"],
            4096
        );
        assert!(resource.payload.value.get("meta").is_none());
        let tree = record
            .resources
            .iter()
            .find(|resource| resource.meta.stats_kind == ResourceStatsKind::Tree)
            .expect("tree resource stats");
        assert_eq!(tree.span_id, Some(SpanUid::new(4)));
        assert_eq!(tree.meta.source, "resource.command_exec.upperdir");
        assert_eq!(tree.payload.value["tree"]["bytes"], 4096);
        assert_eq!(tree.payload.value["tree"]["truncated"], 1);
        let host = record
            .resources
            .iter()
            .find(|resource| resource.meta.stats_kind == ResourceStatsKind::Host)
            .expect("host resource stats");
        assert_eq!(host.meta.source, "daemon.process");
        assert_eq!(host.payload.value["host"]["process"]["rss_bytes"], 4096);
        assert_eq!(host.payload.value["host"]["process"]["max_rss_bytes"], 8192);
        assert!(
            record
                .events
                .iter()
                .any(|event| event.module == "resource" && event.name == "resource_stats"),
            "resource_stats event remains queryable as an event"
        );
    }
}
