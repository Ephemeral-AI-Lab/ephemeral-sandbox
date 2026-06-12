use ed25519_dalek::SigningKey;
use eos_trace::{
    encode_trace_batch, EventRecord, RequestId, ResourceStats, ResourceStatsKind, SpanKind,
    SpanRecord, SpanUid, TraceBatch, TraceId, TraceLink, TraceLinkKind, TraceRecord,
};
use serde_json::json;

use super::*;

#[test]
fn sqlite_posture_and_schema_are_set_on_open() -> Result<(), TraceStoreError> {
    let store = temp_store("posture")?;
    let posture = store.sqlite_posture()?;

    assert_eq!(posture.journal_mode, "wal");
    assert_eq!(posture.synchronous, 2);
    assert!(store.db_path().is_file());
    Ok(())
}

#[test]
fn request_start_failures_fail_closed_for_mutations_and_degrade_reads(
) -> Result<(), TraceStoreError> {
    let store = temp_store("fail-closed")?;

    store.fail_next_request_start_for_tests();
    let mutating =
        store.prepare_forward(request_input("sb-1", "sandbox.file.write", true, "write-1"));
    assert!(matches!(
        mutating,
        Err(TraceStoreError::InjectedRequestStartFailure)
    ));

    store.fail_next_request_start_for_tests();
    let read =
        store.prepare_forward(request_input("sb-1", "sandbox.file.read", false, "read-1"))?;
    assert!(read.degraded);

    let degraded_count: i64 = store.lock().query_row(
        "SELECT COUNT(*) FROM audit_entries WHERE entry_kind='trace_degraded'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(degraded_count, 1);
    Ok(())
}

#[test]
fn sidecar_ingest_rebuilds_lookup_projections_from_audit_entries() -> Result<(), TraceStoreError> {
    let store = temp_store("ingest")?;
    let request = request_input("sb-1", "sandbox.command.exec", true, "request-ingest");
    let trace_id = request.trace_id.clone();
    let request_id = request.request_id.clone();
    store.append_request_start(request)?;

    let mut record = TraceRecord::new(trace_id.clone(), SpanUid::ROOT);
    record.request_id = Some(request_id.clone());
    record.spans.push(SpanRecord::new(
        SpanUid::ROOT,
        None,
        "op_request",
        SpanKind::OpRequest,
        json!({"op": "sandbox.command.exec"}),
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
    record.resources.push(ResourceStats::available(
        ResourceStatsKind::CgroupProcess,
        Some("before".to_owned()),
        "/sys/fs/cgroup/cpu.stat",
        12,
        1,
        json!({"usage_usec": 10}),
    ));
    let batch = encode_trace_batch(&TraceBatch::single(record));
    store.ingest_trace_batch("sb-1", &batch)?;

    assert_eq!(store.events_for_trace(trace_id.as_str())?.len(), 1);
    assert_eq!(
        store.trace_ids_for_link("command", "cmd-1")?,
        vec![trace_id.to_string()]
    );
    assert_eq!(
        store
            .request_by_id(request_id.as_str())?
            .expect("request row")
            .op,
        "sandbox.command.exec"
    );

    store.rebuild_projections()?;
    assert_eq!(store.events_for_trace(trace_id.as_str())?.len(), 1);
    assert_eq!(
        store.trace_ids_for_link("command", "cmd-1")?,
        vec![trace_id.to_string()]
    );
    Ok(())
}

#[test]
fn response_finalization_and_host_events_rebuild_from_audit_entries() -> Result<(), TraceStoreError>
{
    let store = temp_store("response-finalization")?;
    let request = request_input("sb-1", "sandbox.file.read", false, "request-finalized");
    let trace_id = request.trace_id.clone();
    let request_id = request.request_id.clone();
    store.append_request_start(request)?;

    store.append_trace_event(TraceEventInput {
        sandbox_id: "sb-1",
        trace_id: &trace_id,
        request_id: Some(&request_id),
        span_id: None,
        module: "host.transport",
        event: "connect_failed",
        details: json!({"endpoint": "127.0.0.1:9", "error_kind": "connect_failed"}),
    })?;
    let response = json!({"success": true, "content": "ok"});
    let raw = br#"{"success":true,"content":"ok","_trace_events":"encoded"}"#;
    store.record_response_persisted(ResponsePersistedInput {
        sandbox_id: "sb-1",
        trace_id: &trace_id,
        request_id: &request_id,
        response: &response,
        raw_response_bytes: raw,
        host_rtt_ms: 17,
    })?;

    let row = store
        .request_by_id(request_id.as_str())?
        .expect("request row");
    assert_eq!(row.status.as_deref(), Some("ok"));
    assert_eq!(store.events_for_trace(trace_id.as_str())?.len(), 1);

    store.rebuild_projections()?;
    let rebuilt = store
        .request_by_id(request_id.as_str())?
        .expect("rebuilt request row");
    assert_eq!(rebuilt.status.as_deref(), Some("ok"));
    assert_eq!(
        store.events_for_trace(trace_id.as_str())?[0].event,
        "connect_failed"
    );

    store.record_response_missing(ResponseMissingInput {
        sandbox_id: "sb-1",
        trace_id: &trace_id,
        request_id: &request_id,
        status: "uncertain",
        error_kind: "read_timeout",
        message: "daemon response timed out",
    })?;
    let missing = store
        .request_by_id(request_id.as_str())?
        .expect("missing response row");
    assert_eq!(missing.status.as_deref(), Some("uncertain"));
    Ok(())
}

#[test]
fn startup_reconciles_prior_boot_incomplete_requests_to_uncertain() -> Result<(), TraceStoreError> {
    let dir = temp_dir("reconcile");
    {
        let store = TraceStore::open(&dir)?;
        store.append_request_start(request_input("sb-1", "sandbox.file.write", true, "orphan"))?;
    }

    let reopened = TraceStore::open(&dir)?;
    let request = reopened
        .request_by_id("orphan")?
        .expect("orphan request exists");
    assert_eq!(request.status.as_deref(), Some("uncertain"));
    Ok(())
}

#[test]
fn newer_schema_versions_are_refused() -> Result<(), TraceStoreError> {
    let dir = temp_dir("newer-version");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let db = dir.join("sandbox-traces.sqlite");
    let conn = rusqlite::Connection::open(db)?;
    conn.pragma_update(None, "user_version", 999_u32)?;
    drop(conn);

    assert!(matches!(
        TraceStore::open(&dir),
        Err(TraceStoreError::NewerSchema { found: 999, .. })
    ));
    Ok(())
}

#[test]
fn tamper_detection_reports_payload_hash_mismatch() -> Result<(), TraceStoreError> {
    let store = temp_store("tamper")?;
    store.append_request_start(request_input("sb-1", "sandbox.file.read", false, "tamper"))?;
    store.lock().execute(
        "UPDATE audit_entries SET payload=x'00' WHERE entry_kind='request_start'",
        [],
    )?;

    let report = store.verify_chain()?;
    assert!(!report.is_valid());
    assert!(report
        .errors
        .iter()
        .any(|error| error.contains("payload hash mismatch")));
    Ok(())
}

#[test]
fn seal_prune_and_verify_retained_chain() -> Result<(), TraceStoreError> {
    let store = temp_store("seal-prune")?;
    store.append_request_start(request_input("sb-1", "sandbox.file.read", false, "seal-1"))?;
    store.append_request_start(request_input("sb-1", "sandbox.file.write", true, "seal-2"))?;
    let key = [7_u8; 32];
    let seal = store
        .seal_all_unsealed("test-key", &key)?
        .expect("segment sealed");
    let verify_key = SigningKey::from_bytes(&key).verifying_key();
    store.verify_segment_signature(&seal.segment_id, verify_key.as_bytes())?;

    let pruned = store.prune_sealed_through(seal.last_audit_seq)?;
    assert_eq!(pruned.len(), 1);
    let report = store.verify_chain()?;
    assert!(report.is_valid(), "{:?}", report.errors);
    assert_eq!(report.pruned_ranges.len(), 1);
    Ok(())
}

#[test]
fn acceptance_queries_use_indexes() -> Result<(), TraceStoreError> {
    let store = temp_store("query-plan")?;
    let request = request_input("sb-1", "sandbox.command.exec", true, "request-plan");
    let trace_id = request.trace_id.clone();
    store.append_request_start(request)?;

    let mut record = TraceRecord::new(trace_id.clone(), SpanUid::ROOT);
    record.request_id = Some(RequestId::parse("request-plan").expect("request id"));
    record.spans.push(SpanRecord::new(
        SpanUid::ROOT,
        None,
        "mount",
        SpanKind::Overlay,
        json!({"layer_count": 1}),
    ));
    record.events.push(EventRecord::new(
        SpanUid::ROOT,
        "mount_finished",
        "overlay",
        json!({"layer_count": 1, "fsconfig_calls": 3, "duration_us": 10}),
    ));
    record.links.push(TraceLink {
        kind: TraceLinkKind::Command,
        value: "cmd-plan".to_owned(),
    });
    record.resources.push(ResourceStats::available(
        ResourceStatsKind::CgroupProcess,
        Some("before".to_owned()),
        "cpu.stat",
        1,
        1,
        json!({"phase": "before"}),
    ));
    store.ingest_trace_batch("sb-1", &encode_trace_batch(&TraceBatch::single(record)))?;

    for sql in acceptance_queries(trace_id.as_str()) {
        let plan = store.query_plan_for(&sql)?;
        assert!(
            plan.iter().any(|line| line.contains("SEARCH")),
            "expected indexed SEARCH for {sql}; plan={plan:?}"
        );
    }
    Ok(())
}

fn request_input<'a>(
    sandbox_id: &'a str,
    op: &'a str,
    mutates_state: bool,
    request_id: &'a str,
) -> RequestStartInput<'a> {
    RequestStartInput {
        sandbox_id,
        trace_id: TraceId::parse(format!("trace-{request_id}")).expect("trace id"),
        request_id: RequestId::parse(request_id).expect("request id"),
        op,
        family: "Files",
        caller_id: Some("caller-1"),
        mutates_state,
        args: json!({"caller_id": "caller-1", "path": "README.md"}),
        forwarded_bytes: br#"{"op":"sandbox.file.read"}"#,
    }
}

fn temp_store(name: &str) -> Result<TraceStore, TraceStoreError> {
    TraceStore::open(temp_dir(name))
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "eos-host-trace-store-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn acceptance_queries(trace_id: &str) -> Vec<String> {
    vec![
        format!(
            "SELECT seq, module, event, details_json FROM trace_events WHERE trace_id='{trace_id}' ORDER BY seq"
        ),
        "SELECT s.kind, s.subsystem, s.duration_us/1e3 ms, s.fields_json FROM trace_spans s WHERE s.request_id='request-plan' ORDER BY s.started_us".to_owned(),
        "SELECT o.request_id, o.op, o.status, o.sent_at_ms FROM trace_requests o JOIN trace_links l ON l.trace_id=o.trace_id WHERE l.link_kind='command' AND l.link_id='cmd-plan' ORDER BY o.sent_at_ms".to_owned(),
        "SELECT * FROM trace_requests WHERE family='Plugins' AND status IN ('error','rejected') AND workspace_route='isolated_workspace' AND sent_at_ms > 0".to_owned(),
        format!(
            "SELECT audit_seq, entry_kind, payload_sha256, prev_global_sha256, prev_sandbox_sha256, entry_sha256 FROM audit_entries WHERE trace_id='{trace_id}' ORDER BY audit_seq"
        ),
        "SELECT b.values_json AS before_values, a.values_json AS after_values FROM trace_resources b JOIN trace_resources a ON a.trace_id=b.trace_id AND a.request_id=b.request_id AND a.span_id=b.span_id AND a.kind=b.kind WHERE b.request_id='request-plan' AND json_extract(b.values_json,'$.phase')='before' AND json_extract(a.values_json,'$.phase')='after'".to_owned(),
        "SELECT json_extract(details_json,'$.layer_count') AS layer_count, json_extract(details_json,'$.fsconfig_calls') AS fsconfig_calls, json_extract(details_json,'$.duration_us') AS duration_us FROM trace_events WHERE event='mount_finished' ORDER BY layer_count, duration_us".to_owned(),
    ]
}
