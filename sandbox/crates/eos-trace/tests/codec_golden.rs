use eos_trace::{
    decode_trace_batch, encode_trace_batch, proto, EventRecord, RequestId, ResourceStats,
    ResourceStatsKind, SpanKind, SpanRecord, SpanSubsystem, SpanUid, TraceBatch, TraceId,
    TraceLink, TraceLinkKind, TraceRecord,
};
use prost::Message;
use serde_json::json;

fn canonical_batch() -> TraceBatch {
    let trace_id = TraceId::parse("trace-codec").expect("trace id");
    let mut record = TraceRecord::new(trace_id, SpanUid::ROOT);
    record.request_id = Some(RequestId::parse("request-codec").expect("request id"));
    record.spans.push(SpanRecord::new(
        SpanUid::ROOT,
        None,
        "op_request",
        SpanKind::OpRequest,
        json!({"op":"sandbox.ready"}),
    ));
    record.events.push(EventRecord::new(
        SpanUid::ROOT,
        "dispatch_started",
        "daemon.dispatch",
        json!({"op_resolved": true}),
    ));
    record.links.push(TraceLink {
        kind: TraceLinkKind::Command,
        value: "cmd-1".to_owned(),
    });
    record.resources.push(
        ResourceStats::available(
            ResourceStatsKind::CgroupProcess,
            Some("after".to_owned()),
            "command.process.wait",
            7,
            1,
            json!({"cpu": {"usage_usec": 42}}),
        )
        .with_span_id(SpanUid::ROOT),
    );
    let mut batch = TraceBatch::single(record);
    batch.daemon_boot_id = Some("boot-codec".to_owned());
    batch
}

#[test]
fn round_trips_trace_batch_through_protobuf() {
    let batch = canonical_batch();
    let encoded = encode_trace_batch(&batch);
    let decoded = decode_trace_batch(&encoded).expect("decode encoded trace batch");

    assert_eq!(decoded.records, batch.records);
    assert_eq!(decoded.daemon_boot_id.as_deref(), Some("boot-codec"));
}

#[test]
fn decodes_operation_span_kind_without_treating_it_as_unknown() {
    let batch = proto::TraceBatch {
        records: vec![proto::TraceRecord {
            trace_id: "trace-operation-span-kind".to_owned(),
            kind: 1,
            root_span_id: 1,
            spans: vec![proto::TraceSpan {
                span_id: 1,
                name: "op.file.write".to_owned(),
                kind: 8,
                subsystem: 3,
                fields_json: "{}".to_owned(),
                ..proto::TraceSpan::default()
            }],
            ..proto::TraceRecord::default()
        }],
        ..proto::TraceBatch::default()
    };

    let decoded = decode_trace_batch(&batch.encode_to_vec()).expect("operation span kind decodes");

    assert_eq!(decoded.records[0].spans[0].kind, SpanKind::Operation);
}

#[test]
fn derives_span_subsystem_from_kind_ignoring_a_contradicting_wire_byte() {
    // A wire span whose subsystem byte contradicts its kind (kind=Operation,
    // which maps to subsystem Op, but subsystem byte claims Wire=1) must decode
    // to the kind-derived subsystem, so the closed SpanKind::subsystem mapping
    // cannot be violated through the wire.
    let batch = proto::TraceBatch {
        records: vec![proto::TraceRecord {
            trace_id: "trace-subsystem-mismatch".to_owned(),
            kind: 1,
            root_span_id: 1,
            spans: vec![proto::TraceSpan {
                span_id: 1,
                name: "op.file.write".to_owned(),
                kind: 8,
                subsystem: 1,
                fields_json: "{}".to_owned(),
                ..proto::TraceSpan::default()
            }],
            ..proto::TraceRecord::default()
        }],
        ..proto::TraceBatch::default()
    };

    let decoded =
        decode_trace_batch(&batch.encode_to_vec()).expect("contradicting subsystem still decodes");

    let span = &decoded.records[0].spans[0];
    assert_eq!(span.kind, SpanKind::Operation);
    assert_eq!(
        span.subsystem,
        SpanSubsystem::Op,
        "subsystem must be derived from kind, not the contradicting wire byte"
    );
}

/// Schema-evolution gate: the committed populated fixture must keep decoding
/// to the same DTOs after any proto regeneration. Read at runtime so the
/// fixture can be regenerated without a compile dependency on its presence.
#[test]
fn decodes_committed_populated_fixture() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/trace_batch_v1_populated.hex"
    );
    let hex = std::fs::read_to_string(path).expect("read committed populated fixture");
    let bytes = hex_to_bytes(hex.trim());
    let decoded = decode_trace_batch(&bytes).expect("decode populated fixture");
    let expected = canonical_batch();
    assert_eq!(decoded.records, expected.records);
    assert_eq!(decoded.daemon_boot_id, expected.daemon_boot_id);
    assert_eq!(decoded.dropped_traces, expected.dropped_traces);
}

#[test]
fn decodes_committed_v1_fixture() {
    let hex = include_str!("fixtures/trace_batch_v1.hex").trim();
    let bytes = hex_to_bytes(hex);
    let decoded = decode_trace_batch(&bytes).expect("decode v1 fixture");

    assert_eq!(decoded.dropped_traces, 7);
    assert!(decoded.records.is_empty());
}

#[test]
fn rejects_unknown_link_kind_instead_of_defaulting_to_command() {
    let batch = proto::TraceBatch {
        records: vec![proto::TraceRecord {
            trace_id: "trace-bad-link-kind".to_owned(),
            kind: 1,
            root_span_id: 1,
            links: vec![proto::TraceLink {
                kind: 99,
                value: "cmd-1".to_owned(),
            }],
            ..proto::TraceRecord::default()
        }],
        ..proto::TraceBatch::default()
    };

    let err = decode_trace_batch(&batch.encode_to_vec()).expect_err("unknown link kind must fail");

    assert!(
        err.to_string()
            .contains("links[0].kind has unknown code 99"),
        "decode error should name the unknown enum code: {err}"
    );
}

#[test]
fn rejects_malformed_child_json_instead_of_dropping_event() {
    let batch = proto::TraceBatch {
        records: vec![proto::TraceRecord {
            trace_id: "trace-bad-event-json".to_owned(),
            kind: 1,
            root_span_id: 1,
            events: vec![proto::TraceEvent {
                span_id: 1,
                name: "bad".to_owned(),
                module: "daemon.dispatch".to_owned(),
                details_json: "not-json".to_owned(),
                ..proto::TraceEvent::default()
            }],
            ..proto::TraceRecord::default()
        }],
        ..proto::TraceBatch::default()
    };

    let err = decode_trace_batch(&batch.encode_to_vec()).expect_err("bad event JSON must fail");

    assert!(
        err.to_string().contains("events[0].details_json"),
        "decode error should name the malformed child field: {err}"
    );
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|chunk| {
            let text = std::str::from_utf8(chunk).expect("hex utf8");
            u8::from_str_radix(text, 16).expect("hex byte")
        })
        .collect()
}
