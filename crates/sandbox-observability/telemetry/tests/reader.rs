//! Reader: historical trace/event views fold one sorted `scan()` over primary +
//! rotated; sample views use bounded streaming passes; `trace` resolves
//! out-of-order records; `samples` Δs only emitter-tagged counters.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{SystemTime, UNIX_EPOCH};

use sandbox_observability_telemetry::{
    Attrs, RawFilter, Reader, Record, Sample, Sink, MAX_LINE_BYTES,
};
use serde_json::{json, Value};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sandbox-obs-reader-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_lines(path: &Path, lines: &[Value]) {
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).expect("write log");
}

fn write_records(path: &Path, records: &[Record]) {
    let mut body = Vec::new();
    for record in records {
        serde_json::to_writer(&mut body, record).expect("serialize record");
        body.push(b'\n');
    }
    fs::write(path, body).expect("write records");
}

fn sample_record(ts: i64, scope: &str, metrics: Value) -> Record {
    Record::Sample(Sample {
        ts,
        scope: scope.to_owned(),
        metrics: metrics.as_object().cloned().unwrap_or_else(Attrs::new),
    })
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

#[test]
fn scan_spans_primary_and_rotated_sorted_by_ts() {
    let dir = temp_dir("rotation");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    // Rotated (older) and primary (newer) each hold out-of-order lines.
    write_lines(
        &rotated,
        &[
            json!({ "kind": "sample", "ts": 300, "scope": "sandbox" }),
            json!({ "kind": "sample", "ts": 100, "scope": "sandbox" }),
        ],
    );
    write_lines(
        &primary,
        &[
            json!({ "kind": "sample", "ts": 400, "scope": "sandbox" }),
            json!({ "kind": "sample", "ts": 200, "scope": "sandbox" }),
        ],
    );

    let reader = Reader::new(primary, rotated);
    let raw = reader.raw(RawFilter::default());
    let order: Vec<i64> = raw
        .iter()
        .map(|line| {
            serde_json::from_str::<Value>(line).expect("parse")["ts"]
                .as_i64()
                .expect("ts")
        })
        .collect();
    assert_eq!(
        order,
        vec![100, 200, 300, 400],
        "sorted by ts across both files"
    );
}

#[test]
fn scan_skips_malformed_lines() {
    let dir = temp_dir("malformed");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    fs::write(
        &primary,
        "{\"kind\":\"sample\",\"ts\":10,\"scope\":\"sandbox\"}\n{\"kind\":\"sample\",\"ts\":20,\"sco",
    )
    .expect("write");

    let reader = Reader::new(primary, rotated);
    assert_eq!(
        reader.raw(RawFilter::default()).len(),
        1,
        "half-written tail skipped"
    );
}

#[test]
fn scan_skips_invalid_utf8_oversized_and_partial_lines() {
    let dir = temp_dir("invalid-lines");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    let valid = serde_json::to_vec(&sample_record(7, "sandbox", json!({ "ok": true })))
        .expect("valid record");
    let mut body = Vec::new();
    body.extend_from_slice(b"{malformed}\n");
    body.extend_from_slice(&[0xff, 0xfe, b'\n']);
    body.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES));
    body.push(b'\n');
    body.extend_from_slice(&valid);
    body.push(b'\n');
    body.extend_from_slice(b"{\"kind\":\"sample\"");
    fs::write(&primary, body).expect("write mixed input");

    let raw = Reader::new(primary, rotated).raw(RawFilter::default());
    assert_eq!(raw.len(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(&raw[0]).expect("parse")["ts"],
        7
    );
}

#[test]
fn maximum_escaped_record_split_across_internal_buffers_is_read() {
    let dir = temp_dir("split-buffer");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    let record = sample_record(9, "sandbox", json!({ "escaped": "\\\"\n🦀".repeat(1_300) }));
    let encoded = serde_json::to_vec(&record).expect("serialize");
    assert!(encoded.len() > 8 * 1024, "crosses the internal read buffer");
    assert!(encoded.len() < MAX_LINE_BYTES, "within line cap");
    write_records(&primary, &[record]);

    let raw = Reader::new(primary, rotated).raw(RawFilter::default());
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].as_bytes(), encoded);
}

#[test]
fn segment_order_does_not_change_timestamp_order() {
    for reversed in [false, true] {
        let dir = temp_dir(if reversed {
            "order-reversed"
        } else {
            "order-normal"
        });
        let primary = dir.join("observability.ndjson");
        let rotated = dir.join("observability.ndjson.1");
        let older = json!({ "kind": "sample", "ts": 10, "scope": "sandbox" });
        let newer = json!({ "kind": "sample", "ts": 20, "scope": "sandbox" });
        if reversed {
            write_lines(&primary, &[older]);
            write_lines(&rotated, &[newer]);
        } else {
            write_lines(&rotated, &[older]);
            write_lines(&primary, &[newer]);
        }
        let order: Vec<i64> = Reader::new(primary, rotated)
            .raw(RawFilter::default())
            .into_iter()
            .map(|line| {
                serde_json::from_str::<Value>(&line).expect("parse")["ts"]
                    .as_i64()
                    .expect("ts")
            })
            .collect();
        assert_eq!(order, vec![10, 20]);
    }
}

#[test]
fn samples_delta_only_emitter_tagged_counters() {
    let dir = temp_dir("samples");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    let base = now_ms() - 2_000;
    write_lines(
        &primary,
        &[
            json!({ "kind": "sample", "ts": base, "scope": "sandbox", "cpu_usec": 100, "mem_cur": 10, "_counters": ["cpu_usec"] }),
            json!({ "kind": "sample", "ts": base + 1_000, "scope": "sandbox", "cpu_usec": 250, "mem_cur": 8, "_counters": ["cpu_usec"] }),
        ],
    );

    let reader = Reader::new(primary, rotated);
    let series = reader.samples("sandbox", 600_000);
    assert_eq!(series.len(), 2);
    assert!(
        series[0].deltas.is_empty(),
        "first in-window sample has no delta"
    );
    assert_eq!(series[1].deltas["cpu_usec"], 150, "counter Δ");
    assert!(!series[1].deltas.contains_key("mem_cur"), "gauge gets no Δ");
    assert_eq!(series[1].sample_delta_ms, Some(1_000));
    assert!(
        !series[1].metrics.contains_key("_counters"),
        "reserved meta key stripped from presented metrics"
    );
    assert_eq!(series[1].metrics["mem_cur"], 8);
}

#[test]
fn resource_samples_stream_rotated_then_active_without_mutating_either_segment() {
    let dir = temp_dir("resource-pure");
    let primary = dir.join("resources.ndjson");
    let rotated = dir.join("resources.ndjson.1");
    let base = now_ms() - 2_000;
    let mut rotated_body = format!(
        "{{\"kind\":\"sample\",\"ts\":{base},\"scope\":\"sandbox\",\"cpu_usec\":10,\"fixture_marker\":\"older\",\"_counters\":[\"cpu_usec\"]}}\n{{malformed}}\n"
    )
    .into_bytes();
    rotated_body.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES));
    rotated_body.push(b'\n');
    fs::write(&rotated, rotated_body).expect("write rotated resource segment");
    fs::write(
        &primary,
        format!(
            "{{\"kind\":\"sample\",\"ts\":{},\"scope\":\"sandbox\",\"cpu_usec\":25,\"fixture_marker\":\"newer\",\"_counters\":[\"cpu_usec\"]}}\n{{\"kind\":\"sample\"",
            base + 1_000
        ),
    )
    .expect("write active resource segment");
    let before = [
        fs::read(&rotated).expect("rotated bytes"),
        fs::read(&primary).expect("active bytes"),
    ];

    let read = Reader::new(primary.clone(), rotated.clone()).resource_samples("sandbox", 600_000);

    assert_eq!(read.series.len(), 2);
    assert_eq!(read.series[0].metrics["fixture_marker"], "older");
    assert_eq!(read.series[1].metrics["fixture_marker"], "newer");
    assert_eq!(read.series[1].deltas["cpu_usec"], 15);
    assert!(read.errors.iter().any(|error| error.contains("malformed")));
    assert!(read.errors.iter().any(|error| error.contains("oversized")));
    assert!(read
        .errors
        .iter()
        .any(|error| error.contains("partial tail")));
    assert_eq!(fs::read(&rotated).expect("rotated after"), before[0]);
    assert_eq!(fs::read(&primary).expect("active after"), before[1]);
}

#[test]
fn resource_samples_remain_bounded_and_parseable_during_concurrent_rotation() {
    let dir = temp_dir("resource-concurrent");
    let primary = dir.join("resources.ndjson");
    let rotated = dir.join("resources.ndjson.1");
    let budget = 64 * 1024;
    let sink = Arc::new(Sink::with_budget(primary.clone(), MAX_LINE_BYTES, budget));
    sink.append_strict(&sample_record(
        now_ms(),
        "sandbox",
        json!({ "fixture_index": 0, "blob": "x".repeat(1_000) }),
    ))
    .expect("seed resource store");
    let barrier = Arc::new(Barrier::new(2));
    let writer_sink = Arc::clone(&sink);
    let writer_barrier = Arc::clone(&barrier);
    let base = now_ms();
    let writer = std::thread::spawn(move || {
        writer_barrier.wait();
        for index in 1..=300_i64 {
            writer_sink
                .append_strict(&sample_record(
                    base.saturating_add(index),
                    "sandbox",
                    json!({ "fixture_index": index, "blob": "x".repeat(1_000) }),
                ))
                .expect("concurrent resource append");
            if index % 8 == 0 {
                std::thread::yield_now();
            }
        }
    });
    let reader = Reader::new(primary.clone(), rotated.clone());
    barrier.wait();
    for _ in 0..300 {
        let read = reader.resource_samples("sandbox", 600_000);
        assert!(read.series.len() <= 500);
        assert!(
            serde_json::to_vec(&read.series)
                .expect("serialize concurrent read")
                .len()
                <= 256 * 1024
        );
        assert!(read
            .series
            .iter()
            .all(|sample| sample.metrics["fixture_index"].is_i64()));
        std::thread::yield_now();
    }
    writer.join().expect("resource writer joins");

    let final_read = reader.resource_samples("sandbox", 600_000);
    assert!(!final_read.series.is_empty());
    assert!(fs::metadata(&primary).map_or(0, |metadata| metadata.len()) <= budget / 2);
    assert!(fs::metadata(&rotated).map_or(0, |metadata| metadata.len()) <= budget / 2);
}

#[test]
fn samples_filter_by_window() {
    let dir = temp_dir("window");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    let now = now_ms();
    write_lines(
        &primary,
        &[
            json!({ "kind": "sample", "ts": now - 500_000, "scope": "sandbox", "cpu_usec": 1 }),
            json!({ "kind": "sample", "ts": now - 1_000, "scope": "sandbox", "cpu_usec": 2 }),
        ],
    );
    let reader = Reader::new(primary, rotated);
    let recent = reader.samples("sandbox", 60_000);
    assert_eq!(recent.len(), 1, "only the in-window sample");
    assert_eq!(recent[0].metrics["cpu_usec"], 2);
}

#[test]
fn samples_orders_only_matching_windowed_records_across_both_logs() {
    let dir = temp_dir("streamed-window");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    let now = now_ms();
    write_lines(
        &rotated,
        &[
            json!({ "kind": "span", "ts": now - 500, "trace": "ignored", "span": "d-1", "name": "ignored", "dur_ms": 1.0, "status": "completed", "attrs": { "payload": "unrelated history" } }),
            json!({ "kind": "sample", "ts": now - 100, "scope": "sandbox", "cpu_usec": 20, "_counters": ["cpu_usec"] }),
            json!({ "kind": "sample", "ts": now - 300, "scope": "other", "cpu_usec": 9_999 }),
        ],
    );
    write_lines(
        &primary,
        &[
            json!({ "kind": "sample", "ts": now - 200, "scope": "sandbox", "cpu_usec": 10, "_counters": ["cpu_usec"] }),
            json!({ "kind": "sample", "ts": now - 120_000, "scope": "sandbox", "cpu_usec": 1 }),
        ],
    );

    let reader = Reader::new(primary, rotated);
    let series = reader.samples("sandbox", 60_000);

    assert_eq!(series.len(), 2);
    assert_eq!(series[0].metrics["cpu_usec"], 10);
    assert_eq!(series[1].metrics["cpu_usec"], 20);
    assert_eq!(series[1].deltas["cpu_usec"], 10);
    assert_eq!(series[1].sample_delta_ms, Some(100));
}

#[test]
fn latest_samples_keeps_only_the_newest_pair_per_requested_scope() {
    let dir = temp_dir("latest-samples");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    write_lines(
        &rotated,
        &[
            json!({ "kind": "sample", "ts": 100, "scope": "sandbox", "cpu_usec": 100, "_counters": ["cpu_usec"] }),
            json!({ "kind": "sample", "ts": 150, "scope": "workspace-1", "disk_bytes": 1 }),
            json!({ "kind": "sample", "ts": 500, "scope": "ignored", "cpu_usec": 9_999 }),
        ],
    );
    write_lines(
        &primary,
        &[
            json!({ "kind": "sample", "ts": 300, "scope": "sandbox", "cpu_usec": 250, "_counters": ["cpu_usec"] }),
            json!({ "kind": "sample", "ts": 200, "scope": "sandbox", "cpu_usec": 150, "_counters": ["cpu_usec"] }),
            json!({ "kind": "sample", "ts": 250, "scope": "workspace-1", "disk_bytes": 3 }),
        ],
    );

    let reader = Reader::new(primary, rotated);
    let latest = reader.latest_samples(&["sandbox", "workspace-1"]);

    assert_eq!(latest.len(), 2, "unrequested scopes are not retained");
    let sandbox = &latest["sandbox"];
    assert_eq!(sandbox.ts, 300);
    assert_eq!(sandbox.metrics["cpu_usec"], 250);
    assert_eq!(sandbox.deltas["cpu_usec"], 100);
    assert_eq!(sandbox.sample_delta_ms, Some(100));
    let workspace = &latest["workspace-1"];
    assert_eq!(workspace.ts, 250);
    assert_eq!(workspace.metrics["disk_bytes"], 3);
    assert!(workspace.deltas.is_empty());
    assert_eq!(workspace.sample_delta_ms, Some(100));
}

#[test]
fn trace_builds_tree_with_offsets_resolving_out_of_order() {
    let dir = temp_dir("trace");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    // Child + event appended BEFORE the parent span — resolution is by id.
    write_lines(
        &primary,
        &[
            json!({ "kind": "span", "ts": 60, "trace": "t", "span": "d-1", "parent": "d-0", "name": "command.exec", "dur_ms": 20.0, "status": "completed", "attrs": {} }),
            json!({ "kind": "event", "ts": 50, "trace": "t", "parent": "d-1", "name": "lease.acquired", "attrs": {} }),
            json!({ "kind": "span", "ts": 120, "trace": "t", "span": "d-0", "name": "daemon.dispatch", "dur_ms": 120.0, "status": "completed", "attrs": {} }),
        ],
    );

    let reader = Reader::new(primary, rotated);
    let forest = reader.trace("t");
    assert_eq!(forest.len(), 1, "one root");
    let root = &forest[0];
    assert_eq!(root.span.span, "d-0");
    assert_eq!(root.offset_ms, 0.0, "root starts at trace_start");
    assert_eq!(root.children.len(), 1);
    let child = &root.children[0];
    assert_eq!(child.span.span, "d-1");
    assert_eq!(
        child.offset_ms, 40.0,
        "child start offset = (ts - dur) - trace_start"
    );
    assert_eq!(
        child.events.len(),
        1,
        "event resolves under its parent span"
    );
    assert_eq!(child.events[0].event.name, "lease.acquired");
    assert_eq!(child.events[0].offset_ms, 50.0);
}

#[test]
fn trace_records_straddling_rotation_build_one_tree() {
    let dir = temp_dir("trace-rotation");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    write_lines(
        &rotated,
        &[
            json!({ "kind": "span", "ts": 100, "trace": "cross", "span": "d-0", "name": "daemon.dispatch", "dur_ms": 100.0, "status": "completed", "attrs": {} }),
        ],
    );
    write_lines(
        &primary,
        &[
            json!({ "kind": "span", "ts": 80, "trace": "cross", "span": "d-1", "parent": "d-0", "name": "command.exec", "dur_ms": 20.0, "status": "completed", "attrs": {} }),
            json!({ "kind": "event", "ts": 70, "trace": "cross", "parent": "d-1", "name": "lease.acquired", "attrs": {} }),
        ],
    );

    let forest = Reader::new(primary, rotated).trace("cross");
    assert_eq!(forest.len(), 1);
    assert_eq!(forest[0].children.len(), 1);
    assert_eq!(forest[0].children[0].events.len(), 1);
}

#[test]
fn every_view_enforces_record_and_encoded_byte_limits() {
    let dir = temp_dir("response-limits");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    let now = now_ms();
    let mut body = String::new();
    for index in 0..20 {
        body.push_str(&format!(
            "{{\"kind\":\"event\",\"ts\":{},\"trace\":\"events\",\"parent\":\"d-0\",\"name\":\"event.{index}\",\"attrs\":{{\"blob\":\"{}\"}}}}\n",
            now + index,
            "x".repeat(80)
        ));
        body.push_str(&format!(
            "{{\"kind\":\"span\",\"ts\":{},\"trace\":\"trace\",\"span\":\"d-{index}\",\"name\":\"command.exec\",\"dur_ms\":1.0,\"status\":\"completed\",\"attrs\":{{\"blob\":\"{}\"}}}}\n",
            now + 100 + index,
            "y".repeat(80)
        ));
        body.push_str(&format!(
            "{{\"kind\":\"sample\",\"ts\":{},\"scope\":\"scope-{index}\",\"value\":{index},\"blob\":\"{}\"}}\n",
            now + 200 + index,
            "z".repeat(80)
        ));
    }
    fs::write(&primary, body).expect("write bounded-view fixture");
    let max_records = 3;
    let max_bytes = 420;
    let reader = Reader::with_limits(primary, rotated, MAX_LINE_BYTES, max_records, max_bytes);

    let raw = reader.raw(RawFilter::default());
    assert!(raw.len() <= max_records);
    assert!(serde_json::to_vec(&raw).expect("raw response").len() <= max_bytes);

    let events = reader.events(RawFilter::default());
    assert!(events.len() <= max_records);
    assert!(serde_json::to_vec(&events).expect("events response").len() <= max_bytes);

    let trace = reader.trace("trace");
    assert!(trace.len() <= max_records);
    assert!(serde_json::to_vec(&trace).expect("trace response").len() <= max_bytes);

    let samples = reader.samples("scope-19", 600_000);
    assert!(samples.len() <= max_records);
    assert!(
        serde_json::to_vec(&samples)
            .expect("samples response")
            .len()
            <= max_bytes
    );

    let scopes: Vec<String> = (0..20).map(|index| format!("scope-{index}")).collect();
    let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let latest = reader.latest_samples(&scope_refs);
    assert!(latest.len() <= max_records);
    assert!(serde_json::to_vec(&latest).expect("latest response").len() <= max_bytes);
}

#[test]
fn raw_and_events_filter_by_kind_name_trace_since() {
    let dir = temp_dir("filters");
    let primary = dir.join("observability.ndjson");
    let rotated = dir.join("observability.ndjson.1");
    write_lines(
        &primary,
        &[
            json!({ "kind": "span", "ts": 10, "trace": "t1", "span": "d-0", "name": "command.exec", "dur_ms": 1.0, "status": "completed", "attrs": {} }),
            json!({ "kind": "event", "ts": 20, "trace": "t1", "parent": "d-0", "name": "lease.acquired", "attrs": { "layer_id": "l0" } }),
            json!({ "kind": "event", "ts": 30, "trace": "t2", "parent": "x-0", "name": "lease.released", "attrs": {} }),
        ],
    );

    let reader = Reader::new(primary, rotated);
    assert_eq!(
        reader
            .raw(RawFilter {
                kind: Some("event".to_owned()),
                ..Default::default()
            })
            .len(),
        2,
        "kind filter"
    );
    assert_eq!(
        reader
            .raw(RawFilter {
                trace: Some("t1".to_owned()),
                ..Default::default()
            })
            .len(),
        2,
        "trace filter spans kinds"
    );
    assert_eq!(
        reader
            .raw(RawFilter {
                since_ms: 25,
                ..Default::default()
            })
            .len(),
        1,
        "since filter"
    );

    let events = reader.events(RawFilter {
        name: Some("lease.acquired".to_owned()),
        ..Default::default()
    });
    assert_eq!(events.len(), 1, "events fold reuses parsed Event records");
    assert_eq!(events[0].attrs["layer_id"], "l0");

    let raw_events = reader.raw_json_events(RawFilter {
        trace: Some("t1".to_owned()),
        ..Default::default()
    });
    let mut encoded = String::new();
    raw_events.write_json_array(&mut encoded, Some(1), 256 * 1024);
    let encoded: serde_json::Value = serde_json::from_str(&encoded).expect("raw events parse");
    assert_eq!(encoded.as_array().map(Vec::len), Some(1));
    assert_eq!(encoded[0]["name"], "lease.acquired");
}
