use serde_json::json;
use sha2::{Digest, Sha256};
use trace::{
    encode_trace_batch, EventRecord, RequestId, ResourceStats, ResourceStatsKind, SpanKind,
    SpanRecord, SpanUid, TraceBatch, TraceId, TraceLink, TraceLinkKind, TraceRecord,
};

use super::audit::RESPONSE_PERSISTED_SCHEMA;
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
fn request_start_args_digest_excludes_the_daemon_auth_token() -> Result<(), TraceStoreError> {
    // Security rule: the auth token is never recorded, hashed, or
    // length-recorded. args_digest must describe the request args only, not the
    // forwarded TCP frame (which carries _eos_daemon_auth_token).
    let store = temp_store("args-digest-token-free")?;
    let args = json!({"caller_id": "caller-1", "path": "README.md"});
    store.append_request_start(request_input(
        "sb-1",
        "sandbox.file.read",
        false,
        "digest-token-free",
    ))?;

    let recorded = trace_request_args_digest(&store, "digest-token-free")?;
    let args_bytes = serde_json::to_vec(&args).expect("args serialize");
    assert_eq!(
        recorded,
        hex_sha256(&args_bytes),
        "args_digest must hash the args bytes only"
    );

    // A token-bearing frame must never produce the recorded digest.
    let token_frame = serde_json::to_vec(&json!({
        "op": "sandbox.file.read",
        "args": args,
        "_eos_daemon_auth_token": "super-secret-token",
    }))
    .expect("frame serialize");
    assert_ne!(
        recorded,
        hex_sha256(&token_frame),
        "args_digest must not be computed over the token-bearing frame"
    );
    Ok(())
}

#[test]
fn audit_payload_summaries_are_redacted_before_persistence() -> Result<(), TraceStoreError> {
    let store = temp_store("redacted-summaries")?;
    let trace_id = TraceId::parse("trace-redacted").expect("trace id");
    let request_id = RequestId::parse("request-redacted").expect("request id");
    store.append_request_start(RequestStartInput {
        sandbox_id: "sb-1",
        trace_id: trace_id.clone(),
        request_id: request_id.clone(),
        op: "sandbox.command.exec",
        family: "Commands",
        caller_id: Some("caller-1"),
        mutates_state: true,
        args: json!({
            "caller_id": "caller-1",
            "cmd": "printenv",
            "api_key": "sk-live",
            "nested": {"password": "pw"},
        }),
    })?;
    store.append_trace_event(TraceEventInput {
        sandbox_id: "sb-1",
        trace_id: &trace_id,
        request_id: Some(&request_id),
        span_id: None,
        module: "host.transport",
        event: "request_written",
        details: json!({"Authorization": "Bearer token", "bytes": 12}),
    })?;
    store.record_response_persisted(ResponsePersistedInput {
        sandbox_id: "sb-1",
        trace_id: &trace_id,
        request_id: &request_id,
        response: &json!({
            "status": "ok",
            "result": {"token": "result-token", "safe": "visible"},
            "meta": {},
        }),
        raw_response_bytes:
            br#"{"status":"ok","result":{"token":"result-token","safe":"visible"},"meta":{}}"#,
        host_rtt_ms: 1,
    })?;

    let (args_summary, response_summary): (String, String) = store.lock().query_row(
        "SELECT args_summary, response_summary FROM trace_requests WHERE request_id=?1",
        [request_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert!(args_summary.contains("[redacted]"), "{args_summary}");
    assert!(!args_summary.contains("sk-live"), "{args_summary}");
    assert!(!args_summary.contains("\"pw\""), "{args_summary}");
    assert!(
        response_summary.contains("[redacted]"),
        "{response_summary}"
    );
    assert!(response_summary.contains("visible"), "{response_summary}");
    assert!(
        !response_summary.contains("result-token"),
        "{response_summary}"
    );

    let details_json: String = store.lock().query_row(
        "SELECT details_json FROM trace_events WHERE request_id=?1 AND event='request_written'",
        [request_id.as_str()],
        |row| row.get(0),
    )?;
    assert!(details_json.contains("[redacted]"), "{details_json}");
    assert!(!details_json.contains("Bearer token"), "{details_json}");
    Ok(())
}

#[test]
fn trace_event_append_failures_record_a_durable_loss_entry() -> Result<(), TraceStoreError> {
    let store = temp_store("trace-event-loss")?;
    let trace_id = TraceId::parse("trace-event-loss").expect("trace id");
    let request_id = RequestId::parse("request-event-loss").expect("request id");
    store.fail_next_trace_event_for_tests();

    let error = store
        .append_trace_event_or_loss(TraceEventInput {
            sandbox_id: "sb-1",
            trace_id: &trace_id,
            request_id: Some(&request_id),
            span_id: None,
            module: "host.protocol",
            event: "request_written",
            details: json!({"bytes": 12}),
        })
        .expect_err("injected event append failure should reach caller");
    assert!(
        matches!(error, TraceStoreError::InjectedTraceEventFailure),
        "{error}"
    );

    let payload = trace_loss_payload(&store, trace_id.as_str())?;
    assert_eq!(payload["reason"], "trace_event_append_failed");
    assert_eq!(payload["trace_id"], trace_id.as_str());
    assert_eq!(payload["request_id"], request_id.as_str());
    assert_eq!(payload["module"], "host.protocol");
    assert_eq!(payload["event"], "request_written");
    assert!(
        payload["message"]
            .as_str()
            .is_some_and(|message| message.contains("trace event append intentionally failed")),
        "{payload}"
    );
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
    let degraded_row = store
        .request_by_id("read-1")?
        .expect("degraded read request row");
    assert_eq!(degraded_row.sandbox_id, "sb-1");
    assert_eq!(degraded_row.status.as_deref(), Some("trace_degraded"));
    Ok(())
}

#[test]
fn heartbeat_samples_are_audit_backed_and_projected() -> Result<(), TraceStoreError> {
    let store = temp_store("heartbeat-audit-backed")?;
    store.record_heartbeat(HeartbeatInput {
        sandbox_id: "sb-1",
        daemon_boot_id: Some("boot-1"),
        reachable: true,
        spool_pending: Some(3),
        spool_dropped_total: Some(5),
    })?;

    let (reachable, spool_pending, spool_dropped_total): (i64, i64, i64) = store.lock().query_row(
        "SELECT reachable, spool_pending, spool_dropped_total
         FROM sandbox_heartbeats WHERE sandbox_id='sb-1'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!((reachable, spool_pending, spool_dropped_total), (1, 3, 5));

    let (entry_kind, trace_id, payload): (String, String, Vec<u8>) = store.lock().query_row(
        "SELECT entry_kind, trace_id, payload
         FROM audit_entries WHERE sandbox_id='sb-1' AND entry_kind='heartbeat'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(entry_kind, "heartbeat");
    assert_eq!(trace_id, "_heartbeat");
    let entry = proto::AuditEntry::decode(payload.as_slice())?;
    let body: serde_json::Value =
        serde_json::from_slice(&entry.payload).expect("heartbeat payload is json");
    assert_eq!(body["daemon_boot_id"], json!("boot-1"));
    assert_eq!(body["spool_pending"], json!(3));
    assert!(store.verify_audit(None)?.ok);
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
    let spans = store.spans_for_trace(trace_id.as_str())?;
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].kind, "op_request");
    assert_eq!(spans[0].request_id.as_deref(), Some(request_id.as_str()));
    let resources = store.resources_for_trace(trace_id.as_str())?;
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].kind, "cgroup_process");
    assert!(resources[0].values_json.contains("\"phase\":\"before\""));
    let links = store.links_for_trace(trace_id.as_str())?;
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].link_kind, "command");
    assert_eq!(links[0].link_id, "cmd-1");
    assert_eq!(links[0].request_id, request_id.as_str());
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

    Ok(())
}

#[test]
fn dropped_traces_in_a_batch_persist_a_durable_loss_entry() -> Result<(), TraceStoreError> {
    // Spool overflow is irrecoverable: the daemon reports the count it could not
    // buffer in TraceBatch.dropped_traces, and the host must record a durable,
    // queryable loss entry rather than silently discarding it.
    let store = temp_store("dropped-traces-loss")?;
    let trace_id = TraceId::parse("trace-dropped").expect("trace id");
    let record = TraceRecord::new(trace_id, SpanUid::ROOT);
    let batch = TraceBatch {
        records: vec![record],
        dropped_traces: 7,
        daemon_boot_id: Some("boot-xyz".to_owned()),
    };
    store.ingest_trace_batch("sb-1", &encode_trace_batch(&batch))?;

    let (sandbox_id, payload): (String, Vec<u8>) = store.lock().query_row(
        "SELECT sandbox_id, payload FROM audit_entries
         WHERE entry_kind='loss' AND trace_id=?1",
        ["_spool_overflow"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(sandbox_id, "sb-1");
    let entry = proto::AuditEntry::decode(payload.as_slice())?;
    let body: serde_json::Value =
        serde_json::from_slice(&entry.payload).expect("loss payload is json");
    assert_eq!(body["reason"], json!("spool_overflow"));
    assert_eq!(body["dropped_traces"], json!(7));
    assert_eq!(body["daemon_boot_id"], json!("boot-xyz"));
    Ok(())
}

#[test]
fn empty_batches_with_dropped_traces_persist_a_durable_loss_entry() -> Result<(), TraceStoreError> {
    let store = temp_store("empty-dropped-traces-loss")?;
    let batch = TraceBatch {
        records: Vec::new(),
        dropped_traces: 3,
        daemon_boot_id: Some("boot-empty".to_owned()),
    };
    store.ingest_trace_batch("sb-1", &encode_trace_batch(&batch))?;

    let payloads = loss_payloads(&store)?;
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["reason"], json!("spool_overflow"));
    assert_eq!(payloads[0]["dropped_traces"], json!(3));
    assert_eq!(payloads[0]["daemon_boot_id"], json!("boot-empty"));
    Ok(())
}

#[test]
fn dropped_trace_loss_entries_use_counter_deltas() -> Result<(), TraceStoreError> {
    let store = temp_store("dropped-traces-delta")?;
    let trace_id = TraceId::parse("trace-dropped-delta").expect("trace id");
    let record = TraceRecord::new(trace_id, SpanUid::ROOT);
    let batch = TraceBatch {
        records: vec![record],
        dropped_traces: 7,
        daemon_boot_id: Some("boot-delta".to_owned()),
    };
    let encoded = encode_trace_batch(&batch);

    store.ingest_trace_batch("sb-1", &encoded)?;
    store.ingest_trace_batch("sb-1", &encoded)?;

    let loss_count: i64 = store.lock().query_row(
        "SELECT COUNT(*) FROM audit_entries WHERE entry_kind='loss' AND trace_id=?1",
        ["_spool_overflow"],
        |row| row.get(0),
    )?;
    assert_eq!(
        loss_count, 1,
        "re-exporting the same cumulative daemon counter must not create a fresh loss"
    );

    store.ingest_trace_batch(
        "sb-1",
        &encode_trace_batch(&TraceBatch {
            records: vec![TraceRecord::new(
                TraceId::parse("trace-dropped-delta-next").expect("trace id"),
                SpanUid::ROOT,
            )],
            dropped_traces: 9,
            daemon_boot_id: Some("boot-delta".to_owned()),
        }),
    )?;

    let payloads = loss_payloads(&store)?;
    assert_eq!(payloads.len(), 2);
    assert_eq!(payloads[0]["dropped_traces"], json!(7));
    assert_eq!(payloads[0]["dropped_traces_total"], json!(7));
    assert_eq!(payloads[1]["dropped_traces"], json!(2));
    assert_eq!(payloads[1]["dropped_traces_delta"], json!(2));
    assert_eq!(payloads[1]["dropped_traces_total"], json!(9));
    Ok(())
}

#[test]
fn trace_export_batch_replay_is_idempotent_by_export_id() -> Result<(), TraceStoreError> {
    let store = temp_store("trace-export-idempotent")?;
    let trace_id = TraceId::parse("trace-export-idempotent").expect("trace id");
    let mut record = TraceRecord::new(trace_id.clone(), SpanUid::ROOT);
    record.events.push(EventRecord::new(
        SpanUid::ROOT,
        "background_finished",
        "daemon.background",
        json!({"kind": "unit"}),
    ));
    let encoded = encode_trace_batch(&TraceBatch {
        records: vec![record],
        dropped_traces: 0,
        daemon_boot_id: Some("boot-export-idempotent".to_owned()),
    });
    let digest = trace::sha256_hex(&encoded);

    store.ingest_trace_export_batch_once("sb-1", "export-1", &digest, 1, &encoded)?;
    store.record_trace_export_ack_failure("export-1", "temporary ack failure")?;
    store.ingest_trace_export_batch_once("sb-1", "export-1", &digest, 1, &encoded)?;
    store.record_trace_export_ack_success("export-1")?;

    assert_eq!(store.events_for_trace(trace_id.as_str())?.len(), 1);
    let audit_count: i64 = store.lock().query_row(
        "SELECT COUNT(*) FROM audit_entries WHERE entry_kind='trace_batch'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(audit_count, 1);
    let (acked_at_ms, retry_count, last_error): (Option<i64>, i64, Option<String>) =
        store.lock().query_row(
            "SELECT acked_at_ms, retry_count, last_ack_error
             FROM trace_export_batches WHERE export_id='export-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    assert!(acked_at_ms.is_some());
    assert_eq!(retry_count, 1);
    assert!(last_error.is_none());
    Ok(())
}

#[test]
fn batches_without_dropped_traces_write_no_loss_entry() -> Result<(), TraceStoreError> {
    let store = temp_store("no-dropped-traces")?;
    let trace_id = TraceId::parse("trace-clean").expect("trace id");
    store.ingest_trace_batch(
        "sb-1",
        &encode_trace_batch(&TraceBatch::single(TraceRecord::new(
            trace_id,
            SpanUid::ROOT,
        ))),
    )?;

    let loss_count: i64 = store.lock().query_row(
        "SELECT COUNT(*) FROM audit_entries WHERE entry_kind='loss'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(loss_count, 0);
    Ok(())
}

#[test]
fn background_links_without_request_id_are_deduplicated() -> Result<(), TraceStoreError> {
    let store = temp_store("background-link-dedupe")?;
    let trace_id = TraceId::parse("trace-background-link").expect("trace id");
    let mut record = TraceRecord::new(trace_id.clone(), SpanUid::ROOT);
    record.links.push(TraceLink {
        kind: TraceLinkKind::PluginService,
        value: "svc-1".to_owned(),
    });
    record.links.push(TraceLink {
        kind: TraceLinkKind::PluginService,
        value: "svc-1".to_owned(),
    });

    store.ingest_trace_batch("sb-1", &encode_trace_batch(&TraceBatch::single(record)))?;
    assert_eq!(
        store.trace_ids_for_link("plugin_service", "svc-1")?,
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
    let response = json!({"status": "ok", "result": {"content": "ok"}, "meta": {}});
    let raw = br#"{"status":"ok","result":{"content":"ok"},"meta":{},"_trace_events":"encoded"}"#;
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
    let (schema_name, payload): (String, Vec<u8>) = store.lock().query_row(
        "SELECT schema_name, payload FROM audit_entries WHERE entry_kind='response_persisted'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(schema_name, RESPONSE_PERSISTED_SCHEMA);
    let decoded = proto::ResponsePersisted::decode(payload.as_slice())?;
    assert_eq!(decoded.trace_id, trace_id.as_str());
    assert_eq!(decoded.request_id, request_id.as_str());
    assert_eq!(decoded.status, "ok");
    assert!(decoded.error_kind.is_empty());
    assert_eq!(decoded.host_rtt_ms, 17);
    assert_eq!(decoded.response_len, raw.len() as u64);
    assert!(decoded.response_summary_json.contains("\"content\":\"ok\""));

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
fn response_projection_classifies_new_envelope_errors() -> Result<(), TraceStoreError> {
    let store = temp_store("response-envelope-error")?;
    let request = request_input("sb-1", "sandbox.file.read", false, "request-envelope-error");
    let trace_id = request.trace_id.clone();
    let request_id = request.request_id.clone();
    store.append_request_start(request)?;

    let response = json!({
        "status": "error",
        "error": {
            "kind": "internal_error",
            "message": "failed",
            "details": {}
        },
        "meta": {}
    });
    let raw = br#"{"status":"error","error":{"kind":"internal_error","message":"failed","details":{}},"meta":{}}"#;
    store.record_response_persisted(ResponsePersistedInput {
        sandbox_id: "sb-1",
        trace_id: &trace_id,
        request_id: &request_id,
        response: &response,
        raw_response_bytes: raw,
        host_rtt_ms: 3,
    })?;

    let row = store
        .request_by_id(request_id.as_str())?
        .expect("request row");
    assert_eq!(row.status.as_deref(), Some("error"));
    assert_eq!(
        trace_request_error_kind(&store, request_id.as_str())?.as_deref(),
        Some("internal_error")
    );

    Ok(())
}

#[test]
fn audit_verifier_accepts_intact_store_and_reports_tampering() -> Result<(), TraceStoreError> {
    let store = temp_store("audit-verify")?;
    let request = request_input("sb-1", "sandbox.file.read", false, "verify-request");
    let trace_id = request.trace_id.clone();
    let request_id = request.request_id.clone();
    store.append_request_start(request)?;

    store.record_response_persisted(ResponsePersistedInput {
        sandbox_id: "sb-1",
        trace_id: &trace_id,
        request_id: &request_id,
        response: &json!({"status": "ok", "result": {}, "meta": {}}),
        raw_response_bytes: br#"{"status":"ok","result":{},"meta":{}}"#,
        host_rtt_ms: 1,
    })?;

    let report = store.verify_audit(Some(trace_id.as_str()))?;
    assert!(report.ok, "intact audit store verifies: {report:?}");
    assert_eq!(report.first_error, None);
    assert_eq!(report.scope, "global_chain_with_trace_projection");
    assert_eq!(
        report.checked_entries, 3,
        "trace-scoped verification still checks the global hash chain"
    );

    store.lock().execute(
        "UPDATE audit_entries SET payload=x'00' WHERE request_id=?1 AND entry_kind='response_persisted'",
        [request_id.as_str()],
    )?;
    let tampered = store.verify_audit(Some(trace_id.as_str()))?;
    assert_eq!(
        verify_error_kind(&tampered).as_deref(),
        Some("payload_hash_mismatch")
    );
    Ok(())
}

#[test]
fn audit_verifier_reports_chain_and_projection_failures() -> Result<(), TraceStoreError> {
    let store = temp_store("audit-verify-failures")?;
    let request = request_input("sb-1", "sandbox.file.read", false, "verify-chain");
    let trace_id = request.trace_id.clone();
    store.append_request_start(request)?;

    store.lock().execute(
        "UPDATE audit_entries SET prev_global_sha256='wrong' WHERE trace_id=?1 AND entry_kind='request_start'",
        [trace_id.as_str()],
    )?;
    let broken_chain = store.verify_audit(Some(trace_id.as_str()))?;
    assert_eq!(
        verify_error_kind(&broken_chain).as_deref(),
        Some("global_chain_mismatch")
    );

    let store = temp_store("audit-verify-projection")?;
    let request = request_input("sb-1", "sandbox.file.read", false, "verify-projection");
    let trace_id = request.trace_id.clone();
    store.append_request_start(request)?;
    store.lock().execute(
        "DELETE FROM trace_requests WHERE request_id='verify-projection'",
        [],
    )?;
    let projection_gap = store.verify_audit(Some(trace_id.as_str()))?;
    assert_eq!(
        verify_error_kind(&projection_gap).as_deref(),
        Some("projection_missing_request")
    );
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
fn migrations_repair_partial_trace_export_tables_before_bumping_version(
) -> Result<(), TraceStoreError> {
    let dir = temp_dir("partial-trace-export-migration");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let db = dir.join("sandbox-traces.sqlite");
    let conn = rusqlite::Connection::open(&db)?;
    conn.execute(
        "CREATE TABLE trace_export_batches (export_id TEXT PRIMARY KEY)",
        [],
    )?;
    conn.pragma_update(None, "user_version", 2_u32)?;
    drop(conn);

    let store = TraceStore::open(&dir)?;
    let conn = store.lock();
    let columns = table_columns(&conn, "trace_export_batches")?;
    assert!(columns.iter().any(|column| column == "sandbox_id"));
    assert!(columns.iter().any(|column| column == "ingested_at_ms"));
    assert!(columns.iter().any(|column| column == "retry_count"));
    assert!(columns.iter().any(|column| column == "last_ack_error"));
    let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    assert_eq!(version, 3);
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
    record.spans.push(SpanRecord::new(
        SpanUid::new(2),
        Some(SpanUid::ROOT),
        "plugin_overlay_run",
        SpanKind::Plugin,
        json!({"op": "plugin.generic.query"}),
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
    record.resources.push(
        ResourceStats::available(
            ResourceStatsKind::CgroupProcess,
            Some("before".to_owned()),
            "command.process.wait",
            1,
            1,
            json!({"cpu": {"usage_usec": 10}}),
        )
        .with_span_id(SpanUid::ROOT),
    );
    record.resources.push(
        ResourceStats::available(
            ResourceStatsKind::CgroupProcess,
            Some("after".to_owned()),
            "command.process.wait",
            1,
            1,
            json!({"cpu": {"usage_usec": 15}}),
        )
        .with_span_id(SpanUid::ROOT),
    );
    record.resources.push(
        ResourceStats::available(
            ResourceStatsKind::CgroupProcess,
            Some("before".to_owned()),
            "plugin.overlay.run",
            2,
            1,
            json!({"cpu": {"usage_usec": 20}}),
        )
        .with_span_id(SpanUid::new(2)),
    );
    record.resources.push(
        ResourceStats::available(
            ResourceStatsKind::CgroupProcess,
            Some("after".to_owned()),
            "plugin.overlay.run",
            2,
            1,
            json!({"cpu": {"usage_usec": 25}}),
        )
        .with_span_id(SpanUid::new(2)),
    );
    record.resources.push(
        ResourceStats::available(
            ResourceStatsKind::MountCost,
            Some("after".to_owned()),
            "plugin.overlay.mount",
            0,
            1,
            json!({
                "mount": {
                    "layer_count": 1,
                    "fsconfig_calls": 3,
                    "duration_us": 10,
                },
            }),
        )
        .with_span_id(SpanUid::ROOT),
    );
    record.resources.push(
        ResourceStats::available(
            ResourceStatsKind::Tree,
            None,
            "resource.command_exec.upperdir",
            7,
            1,
            json!({
                "tree": {
                    "bytes": 4096,
                    "entry_count": 2,
                    "truncated": 1,
                },
            }),
        )
        .with_span_id(SpanUid::ROOT),
    );
    store.ingest_trace_batch("sb-1", &encode_trace_batch(&TraceBatch::single(record)))?;

    assert_eq!(
        store.resource_span_ids_for_request("request-plan")?,
        vec![Some(1), Some(1), Some(2), Some(2), Some(1), Some(1)]
    );
    let before_after_pair_count: i64 = store.lock().query_row(
        "SELECT COUNT(*) FROM trace_resources b
         JOIN trace_resources a
           ON a.trace_id=b.trace_id
          AND a.request_id=b.request_id
          AND a.span_id=b.span_id
          AND a.kind=b.kind
         WHERE b.request_id='request-plan'
           AND json_extract(b.values_json,'$.phase')='before'
           AND json_extract(a.values_json,'$.phase')='after'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(before_after_pair_count, 2);
    let paired_sources: String = store.lock().query_row(
        "SELECT group_concat(DISTINCT json_extract(b.values_json,'$.source'))
         FROM trace_resources b
         JOIN trace_resources a
           ON a.trace_id=b.trace_id
          AND a.request_id=b.request_id
          AND a.span_id=b.span_id
          AND a.kind=b.kind
         WHERE b.request_id='request-plan'
           AND json_extract(b.values_json,'$.phase')='before'
           AND json_extract(a.values_json,'$.phase')='after'",
        [],
        |row| row.get(0),
    )?;
    assert!(paired_sources.contains("command.process.wait"));
    assert!(paired_sources.contains("plugin.overlay.run"));
    let mount_cost: (i64, i64, i64) = store.lock().query_row(
        "SELECT json_extract(values_json,'$.payload.mount.layer_count'),
                json_extract(values_json,'$.payload.mount.fsconfig_calls'),
                json_extract(values_json,'$.payload.mount.duration_us')
         FROM trace_resources
         WHERE request_id='request-plan'
           AND kind='mount_cost'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(mount_cost, (1, 3, 10));
    let truncated_tree_count: i64 = store.lock().query_row(
        "SELECT COUNT(*) FROM trace_resources
         WHERE request_id='request-plan'
           AND kind='tree'
           AND json_extract(values_json,'$.payload.tree.truncated')=1",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(truncated_tree_count, 1);

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
    }
}

fn temp_store(name: &str) -> Result<TraceStore, TraceStoreError> {
    TraceStore::open(temp_dir(name))
}

fn trace_request_args_digest(
    store: &TraceStore,
    request_id: &str,
) -> Result<String, TraceStoreError> {
    Ok(store.lock().query_row(
        "SELECT args_digest FROM trace_requests WHERE request_id=?1",
        [request_id],
        |row| row.get(0),
    )?)
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn trace_request_error_kind(
    store: &TraceStore,
    request_id: &str,
) -> Result<Option<String>, TraceStoreError> {
    Ok(store.lock().query_row(
        "SELECT error_kind FROM trace_requests WHERE request_id=?1",
        [request_id],
        |row| row.get(0),
    )?)
}

fn verify_error_kind(report: &TraceVerifyReport) -> Option<String> {
    report.first_error.as_ref().map(|error| error.kind.clone())
}

fn loss_payloads(store: &TraceStore) -> Result<Vec<serde_json::Value>, TraceStoreError> {
    let conn = store.lock();
    let mut stmt = conn.prepare(
        "SELECT payload FROM audit_entries
         WHERE entry_kind='loss' AND trace_id='_spool_overflow'
         ORDER BY audit_seq",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    let mut payloads = Vec::new();
    for row in rows {
        let payload = row?;
        let entry = proto::AuditEntry::decode(payload.as_slice())?;
        payloads.push(serde_json::from_slice(&entry.payload).expect("loss payload is json"));
    }
    Ok(payloads)
}

fn trace_loss_payload(
    store: &TraceStore,
    trace_id: &str,
) -> Result<serde_json::Value, TraceStoreError> {
    let payload: Vec<u8> = store.lock().query_row(
        "SELECT payload FROM audit_entries
         WHERE entry_kind='loss' AND trace_id=?1
         ORDER BY audit_seq DESC LIMIT 1",
        [trace_id],
        |row| row.get(0),
    )?;
    let entry = proto::AuditEntry::decode(payload.as_slice())?;
    Ok(serde_json::from_slice(&entry.payload).expect("loss payload is json"))
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "eos-host-trace-store-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn table_columns(conn: &rusqlite::Connection, table: &str) -> Result<Vec<String>, TraceStoreError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(columns)
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
        "SELECT json_extract(values_json,'$.payload.mount.layer_count') AS layer_count, json_extract(values_json,'$.payload.mount.fsconfig_calls') AS fsconfig_calls, json_extract(values_json,'$.payload.mount.duration_us') AS duration_us FROM trace_resources WHERE request_id='request-plan' AND kind='mount_cost' ORDER BY layer_count, duration_us".to_owned(),
        "SELECT json_extract(values_json,'$.payload.tree.entry_count') AS entry_count, json_extract(values_json,'$.payload.tree.truncated') AS truncated FROM trace_resources WHERE request_id='request-plan' AND kind='tree' AND json_extract(values_json,'$.payload.tree.truncated')=1 ORDER BY ts_us".to_owned(),
    ]
}
