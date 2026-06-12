use eos_trace::{
    decode_trace_batch, encode_trace_batch, EventRecord, RequestId, SpanKind, SpanRecord, SpanUid,
    TraceBatch, TraceId, TraceLink, TraceLinkKind, TraceRecord,
};
use serde_json::json;

#[test]
fn round_trips_trace_batch_through_protobuf() {
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

    let encoded = encode_trace_batch(&TraceBatch::single(record.clone()));
    let decoded = decode_trace_batch(&encoded).expect("decode encoded trace batch");

    assert_eq!(decoded.records, vec![record]);
}

#[test]
fn decodes_committed_v1_fixture() {
    let hex = include_str!("fixtures/trace_batch_v1.hex").trim();
    let bytes = hex_to_bytes(hex);
    let decoded = decode_trace_batch(&bytes).expect("decode v1 fixture");

    assert_eq!(decoded.dropped_traces, 7);
    assert!(decoded.records.is_empty());
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
