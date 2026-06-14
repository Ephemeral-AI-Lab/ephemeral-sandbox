use base64::Engine as _;
use operation::control::contract::{TraceExportAckInput, TraceExportInput};
use serde_json::json;
use trace::{
    decode_trace_batch, DetailBudget, EventRecord, ResourceStatsKind, SpanKind, SpanUid, TraceId,
    TraceRecord, TRACE_SIDECAR_ENCODING, TRACE_SIDECAR_FIELD, TRACE_SIDECAR_SCHEMA,
};

use super::build::{attach_request_sidecar, attach_request_sidecar_with_events};
use super::events::{COMMAND_PROCESS_SPAWN_SPAN_ID, COMMAND_PROCESS_WAIT_SPAN_ID};
use super::transport_failure::{push_transport_failure_from_sidecar, trace_sidecar_bytes};
use crate::trace::{now_ms, push_background_record, RequestTraceEvent, RequestTraceFacts};
use crate::wire::RequestTraceContext;

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
    assert_eq!(response["spool_pending_after"], 1);
    let export_id = response["export_id"]
        .as_str()
        .expect("export id")
        .to_owned();
    let batch_sha256 = response["batch_sha256"]
        .as_str()
        .expect("batch sha")
        .to_owned();
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

    let replay = crate::op_adapter::control::op_trace_export(TraceExportInput { max_records: 16 });
    assert_eq!(replay["export_id"], export_id);
    assert_eq!(replay["batch_sha256"], batch_sha256);

    let ack = crate::op_adapter::control::op_trace_export_ack(TraceExportAckInput {
        export_id,
        batch_sha256,
        record_count: 1,
    });
    assert_eq!(ack["acked"], true);

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
    let ack = crate::op_adapter::control::op_trace_export_ack(TraceExportAckInput {
        export_id: response["export_id"]
            .as_str()
            .expect("export id")
            .to_owned(),
        batch_sha256: response["batch_sha256"]
            .as_str()
            .expect("batch sha")
            .to_owned(),
        record_count: 1,
    });
    assert_eq!(ack["acked"], true);
}

#[test]
fn request_sidecar_drops_children_when_over_budget() {
    let trace = RequestTraceContext {
        trace_id: "trace-budget".to_owned(),
        request_id: "request-budget".to_owned(),
        parent_span_id: None,
        link_hints: Vec::new(),
        capture_budget_version: 1,
    };
    let facts = RequestTraceFacts {
        connection_id: "daemon-conn-budget".to_owned(),
        accepted_at_unix_ms: now_ms(),
        listener_kind: "unix",
        peer_addr: None,
        local_addr: None,
        is_tcp: false,
        request_bytes: 64,
        read_duration_us: 5,
        auth_required: false,
        auth_ok: true,
        protocol_version: Some(1),
    };
    let oversize: Vec<RequestTraceEvent> = (0..200)
        .map(|index| {
            RequestTraceEvent::operation(
                "command",
                "stdin_written",
                json!({"index": index, "padding": "x".repeat(500)}),
            )
        })
        .collect();
    let response = attach_request_sidecar_with_events(
        json!({"success": true}),
        Some(&trace),
        "sandbox.command.exec",
        &facts,
        &oversize,
    );
    let batch = decode_trace_batch(&trace_sidecar_bytes(&response).expect("trace sidecar bytes"))
        .expect("trace batch decodes");
    let record = batch.records.first().expect("request trace record");
    assert!(record.truncated, "oversize record is marked truncated");
    assert!(
        record.dropped_children > 0,
        "dropped children are counted, not silent"
    );
    assert!(
        trace::codec::encoded_trace_record_len(record) <= DetailBudget::SidecarRecord.bytes(),
        "record fits the 64 KiB sidecar budget after enforcement"
    );
    assert!(
        record
            .events
            .iter()
            .any(|event| event.module == "daemon.transport"),
        "transport frame events are never dropped"
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
    let batch = decode_trace_batch(&trace_sidecar_bytes(&response).expect("trace sidecar bytes"))
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
fn request_sidecar_stamps_envelope_meta_from_trace_record() {
    let trace = RequestTraceContext {
        trace_id: "trace-envelope-meta".to_owned(),
        request_id: "request-envelope-meta".to_owned(),
        parent_span_id: None,
        link_hints: Vec::new(),
        capture_budget_version: 1,
    };
    let facts = RequestTraceFacts {
        connection_id: "daemon-conn-envelope-meta".to_owned(),
        accepted_at_unix_ms: now_ms(),
        listener_kind: "tcp",
        peer_addr: Some("127.0.0.1:51000".to_owned()),
        local_addr: Some("127.0.0.1:50000".to_owned()),
        is_tcp: true,
        request_bytes: 128,
        read_duration_us: 9,
        auth_required: true,
        auth_ok: true,
        protocol_version: Some(1),
    };
    let response = attach_request_sidecar_with_events(
        json!({"status": "ok", "result": {"published": true}, "meta": {}}),
        Some(&trace),
        "sandbox.file.write",
        &facts,
        &[RequestTraceEvent::operation(
            "workspace.route",
            "route_selected",
            json!({"kind": "fast_path", "reason": "unit"}),
        )],
    );

    assert_eq!(response["status"], "ok");
    assert_eq!(response["meta"]["op"], "sandbox.file.write");
    assert_eq!(response["meta"]["request_id"], "request-envelope-meta");
    assert_eq!(response["meta"]["trace"]["trace_id"], "trace-envelope-meta");
    assert_eq!(
        response["meta"]["trace"]["request_id"],
        "request-envelope-meta"
    );
    assert_eq!(response["meta"]["trace"]["store"], "pending_host_ingest");
    assert!(
        response["meta"]["trace"]["event_count"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "{response}"
    );
    assert_eq!(response["meta"]["workspace_route"]["kind"], "fast_path");
    assert_eq!(response["meta"]["workspace_route"]["reason"], "unit");
    assert_eq!(
        response[TRACE_SIDECAR_FIELD]["schema"],
        TRACE_SIDECAR_SCHEMA
    );
    assert_eq!(
        response[TRACE_SIDECAR_FIELD]["encoding"],
        TRACE_SIDECAR_ENCODING
    );
    assert_eq!(response[TRACE_SIDECAR_FIELD]["spool_pending"], false);
    assert!(response[TRACE_SIDECAR_FIELD]["data"]
        .as_str()
        .is_some_and(|data| !data.is_empty()));
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
                "command",
                "spawned",
                json!({
                    "command_id": "cmd-span",
                    "success": true,
                    "duration_ms": 3,
                }),
            ),
            RequestTraceEvent::operation(
                "command",
                "wait_finished",
                json!({
                    "command_id": "cmd-span",
                    "status": "ok",
                    "completed": true,
                    "yield_time_ms": 100,
                    "duration_ms": 7,
                }),
            ),
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

    let batch = decode_trace_batch(&trace_sidecar_bytes(&response).expect("trace sidecar bytes"))
        .expect("trace batch decodes");
    let record = batch.records.first().expect("request trace record");
    let spawn_span = record
        .spans
        .iter()
        .find(|span| span.kind == SpanKind::CommandProcessSpawn)
        .expect("command process spawn span");
    assert_eq!(spawn_span.span_id, COMMAND_PROCESS_SPAWN_SPAN_ID);
    assert_eq!(spawn_span.duration_us, 3_000);
    let wait_span = record
        .spans
        .iter()
        .find(|span| span.kind == SpanKind::CommandProcessWait)
        .expect("command process wait span");
    assert_eq!(wait_span.span_id, COMMAND_PROCESS_WAIT_SPAN_ID);
    assert_eq!(wait_span.duration_us, 7_000);
    assert_eq!(record.resources.len(), 3);
    let resource = record
        .resources
        .iter()
        .find(|resource| resource.meta.stats_kind == ResourceStatsKind::CgroupProcess)
        .expect("cgroup resource stats");
    assert_eq!(resource.span_id, Some(COMMAND_PROCESS_WAIT_SPAN_ID));
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
        record.events.iter().any(|event| event.module == "resource"
            && event.name == "resource_stats"
            && event.span_id == COMMAND_PROCESS_WAIT_SPAN_ID),
        "resource_stats event remains queryable as an event"
    );
}
