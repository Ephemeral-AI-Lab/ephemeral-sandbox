use prost::Message;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::json;
use trace::codec::proto;
use trace::sha256_hex;

use super::now_ms;

pub(super) const AUDIT_SCHEMA: &str = "eos.trace.v1.AuditEntry";
pub(super) const REQUEST_START_SCHEMA: &str = "eos.trace.v1.RequestStart";
pub(super) const RESPONSE_PERSISTED_SCHEMA: &str = "eos.trace.v1.ResponsePersisted";
pub(super) const TRACE_BATCH_SCHEMA: &str = "eos.trace.v1.TraceBatch";

const SPOOL_OVERFLOW_TRACE_ID: &str = "_spool_overflow";

pub(super) struct AuditAppend<'a> {
    pub(super) sandbox_id: &'a str,
    pub(super) trace_id: &'a str,
    pub(super) request_id: Option<&'a str>,
    pub(super) entry_kind: &'a str,
    pub(super) schema_name: &'a str,
    pub(super) schema_version: i64,
    pub(super) received_at_ms: u64,
    pub(super) payload: &'a [u8],
}

pub(super) struct EntryHashInput<'a> {
    pub(super) sandbox_id: &'a str,
    pub(super) trace_id: &'a str,
    pub(super) request_id: Option<&'a str>,
    pub(super) entry_kind: &'a str,
    pub(super) schema_name: &'a str,
    pub(super) schema_version: i64,
    pub(super) received_at_ms: u64,
    pub(super) payload_sha256: &'a str,
    pub(super) prev_global_sha256: Option<&'a str>,
    pub(super) prev_sandbox_sha256: Option<&'a str>,
}

pub(super) fn append_loss_tx(
    tx: &Transaction<'_>,
    sandbox_id: &str,
    trace_id: &str,
    request_id: &str,
    reason: &str,
    message: &str,
) -> Result<(), rusqlite::Error> {
    let payload = proto::AuditEntry {
        entry_id: request_id.to_owned(),
        trace_id: trace_id.to_owned(),
        seq: 0,
        payload: json!({"reason": reason, "message": message})
            .to_string()
            .into_bytes(),
        previous_hash: Vec::new(),
        entry_hash: Vec::new(),
        schema_version: "1".to_owned(),
        written_at_unix_ms: now_ms(),
    }
    .encode_to_vec();
    append_audit_entry_tx(
        tx,
        AuditAppend {
            sandbox_id,
            trace_id,
            request_id: Some(request_id),
            entry_kind: "loss",
            schema_name: AUDIT_SCHEMA,
            schema_version: 1,
            received_at_ms: now_ms(),
            payload: &payload,
        },
    )?;
    Ok(())
}

/// Durable loss entry for daemon spool overflow. The daemon reports a cumulative
/// counter, so callers must pass the newly observed delta and the total that
/// produced it.
pub(super) fn append_dropped_traces_loss_tx(
    tx: &Transaction<'_>,
    sandbox_id: &str,
    dropped_traces_delta: u64,
    dropped_traces_total: u64,
    daemon_boot_id: Option<&str>,
) -> Result<(), rusqlite::Error> {
    let payload = proto::AuditEntry {
        entry_id: uuid::Uuid::new_v4().simple().to_string(),
        trace_id: SPOOL_OVERFLOW_TRACE_ID.to_owned(),
        seq: 0,
        payload: json!({
            "reason": "spool_overflow",
            "dropped_traces": dropped_traces_delta,
            "dropped_traces_delta": dropped_traces_delta,
            "dropped_traces_total": dropped_traces_total,
            "daemon_boot_id": daemon_boot_id,
        })
        .to_string()
        .into_bytes(),
        previous_hash: Vec::new(),
        entry_hash: Vec::new(),
        schema_version: "1".to_owned(),
        written_at_unix_ms: now_ms(),
    }
    .encode_to_vec();
    append_audit_entry_tx(
        tx,
        AuditAppend {
            sandbox_id,
            trace_id: SPOOL_OVERFLOW_TRACE_ID,
            request_id: None,
            entry_kind: "loss",
            schema_name: AUDIT_SCHEMA,
            schema_version: 1,
            received_at_ms: now_ms(),
            payload: &payload,
        },
    )?;
    Ok(())
}

pub(super) fn append_audit_entry_tx(
    tx: &Transaction<'_>,
    append: AuditAppend<'_>,
) -> Result<i64, rusqlite::Error> {
    let payload_sha256 = sha256_hex(append.payload);
    let prev_global_sha256 = tx
        .query_row(
            "SELECT entry_sha256 FROM audit_entries ORDER BY audit_seq DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let prev_sandbox_sha256 = tx
        .query_row(
            "SELECT entry_sha256 FROM audit_entries
             WHERE sandbox_id=?1 ORDER BY audit_seq DESC LIMIT 1",
            params![append.sandbox_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let entry_sha256 = compute_entry_hash(EntryHashInput {
        sandbox_id: append.sandbox_id,
        trace_id: append.trace_id,
        request_id: append.request_id,
        entry_kind: append.entry_kind,
        schema_name: append.schema_name,
        schema_version: append.schema_version,
        received_at_ms: append.received_at_ms,
        payload_sha256: &payload_sha256,
        prev_global_sha256: prev_global_sha256.as_deref(),
        prev_sandbox_sha256: prev_sandbox_sha256.as_deref(),
    });
    tx.execute(
        "INSERT INTO audit_entries
         (sandbox_id, trace_id, request_id, entry_kind, schema_name, schema_version,
          received_at_ms, payload, payload_sha256, prev_global_sha256,
          prev_sandbox_sha256, entry_sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            append.sandbox_id,
            append.trace_id,
            append.request_id,
            append.entry_kind,
            append.schema_name,
            append.schema_version,
            append.received_at_ms,
            append.payload,
            payload_sha256,
            prev_global_sha256,
            prev_sandbox_sha256,
            entry_sha256,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

pub(super) fn compute_entry_hash(input: EntryHashInput<'_>) -> String {
    let canonical = format!(
        "v1|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        input.sandbox_id,
        input.trace_id,
        input.request_id.unwrap_or_default(),
        input.entry_kind,
        input.schema_name,
        input.schema_version,
        input.received_at_ms,
        input.payload_sha256,
        input.prev_global_sha256.unwrap_or_default(),
        input.prev_sandbox_sha256.unwrap_or_default()
    );
    sha256_hex(canonical.as_bytes())
}
