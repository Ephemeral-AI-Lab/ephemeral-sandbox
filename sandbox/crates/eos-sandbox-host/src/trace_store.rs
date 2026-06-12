use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use prost::Message;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use eos_trace::budget::{BoundedJson, DetailBudget};
use eos_trace::codec::{decode_trace_batch, proto};
use eos_trace::{BootId, RequestId, TraceId};

const STORE_SCHEMA_VERSION: u32 = 1;
const HOST_SANDBOX_ID: &str = "_host";
const AUDIT_SCHEMA: &str = "eos.trace.v1.AuditEntry";
const REQUEST_START_SCHEMA: &str = "eos.trace.v1.RequestStart";
const TRACE_BATCH_SCHEMA: &str = "eos.trace.v1.TraceBatch";

#[derive(Debug, thiserror::Error)]
pub enum TraceStoreError {
    #[error("open trace store at {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("trace store schema version {found} is newer than supported {supported}")]
    NewerSchema { found: u32, supported: u32 },
    #[error("trace store sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("trace protobuf decode error: {0}")]
    Decode(#[from] eos_trace::DecodeTraceError),
    #[error("trace protobuf decode error: {0}")]
    ProstDecode(#[from] prost::DecodeError),
    #[error("trace store request-start append intentionally failed for test")]
    InjectedRequestStartFailure,
    #[error("invalid seal key length: {0}")]
    InvalidSealKeyLength(usize),
    #[error("segment {0} has no seal")]
    MissingSegmentSeal(String),
    #[error("segment {0} signature failed verification")]
    BadSegmentSignature(String),
}

pub struct TraceStore {
    db_path: PathBuf,
    conn: Mutex<Connection>,
    host_boot_id: BootId,
    fail_next_request_start: AtomicBool,
}

impl TraceStore {
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self, TraceStoreError> {
        let state_dir = state_dir.as_ref();
        std::fs::create_dir_all(state_dir).map_err(|source| TraceStoreError::Open {
            path: state_dir.to_path_buf(),
            source: rusqlite::Error::ToSqlConversionFailure(Box::new(source)),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700));
        }

        let db_path = state_dir.join("sandbox-traces.sqlite");
        let conn = Connection::open(&db_path).map_err(|source| TraceStoreError::Open {
            path: db_path.clone(),
            source,
        })?;
        apply_pragmas(&conn)?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > STORE_SCHEMA_VERSION {
            return Err(TraceStoreError::NewerSchema {
                found: version,
                supported: STORE_SCHEMA_VERSION,
            });
        }
        conn.execute_batch(DDL)?;
        conn.pragma_update(None, "user_version", STORE_SCHEMA_VERSION)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600));
        }

        let store = Self {
            db_path,
            conn: Mutex::new(conn),
            host_boot_id: BootId::new(),
            fail_next_request_start: AtomicBool::new(false),
        };
        store.record_host_boot()?;
        store.reconcile_startup_orphans()?;
        Ok(store)
    }

    #[must_use]
    pub fn host_boot_id(&self) -> &BootId {
        &self.host_boot_id
    }

    #[must_use]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    #[cfg(test)]
    pub fn fail_next_request_start_for_tests(&self) {
        self.fail_next_request_start.store(true, Ordering::SeqCst);
    }

    pub fn prepare_forward(
        &self,
        input: RequestStartInput<'_>,
    ) -> Result<ForwardTraceDecision, TraceStoreError> {
        let trace_id = input.trace_id.clone();
        let request_id = input.request_id.clone();
        let mutates_state = input.mutates_state;
        match self.append_request_start(input) {
            Ok(()) => Ok(ForwardTraceDecision {
                trace_id,
                request_id,
                degraded: false,
            }),
            Err(err) if !mutates_state && err.allows_read_only_degraded() => {
                self.append_trace_degraded(HOST_SANDBOX_ID, &trace_id, Some(&request_id), &err)?;
                Ok(ForwardTraceDecision {
                    trace_id,
                    request_id,
                    degraded: true,
                })
            }
            Err(err) => Err(err),
        }
    }

    pub fn append_request_start(
        &self,
        input: RequestStartInput<'_>,
    ) -> Result<(), TraceStoreError> {
        if self.fail_next_request_start.swap(false, Ordering::SeqCst) {
            return Err(TraceStoreError::InjectedRequestStartFailure);
        }

        let args_summary =
            BoundedJson::capture(input.args.clone(), DetailBudget::RequestArgsSummary);
        let payload = proto::RequestStart {
            trace_id: input.trace_id.to_string(),
            request_id: input.request_id.to_string(),
            sandbox_id: input.sandbox_id.to_owned(),
            op: input.op.to_owned(),
            mutates_state: input.mutates_state,
            args_summary_json: args_summary.encoded_value(),
            args_summary_truncated: args_summary.truncated,
            args_summary_sha256: args_summary.sha256.clone().unwrap_or_default(),
            args_summary_original_len: usize_to_u64(args_summary.original_len),
            started_at_unix_ms: now_ms(),
            caller_id: input.caller_id.unwrap_or_default().to_owned(),
            host_boot_id: self.host_boot_id.to_string(),
            args_len: usize_to_u64(input.forwarded_bytes.len()),
            args_digest: sha256_hex(input.forwarded_bytes),
        }
        .encode_to_vec();

        let mut conn = self.lock();
        let tx = conn.transaction()?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: input.sandbox_id,
                trace_id: input.trace_id.as_str(),
                request_id: Some(input.request_id.as_str()),
                entry_kind: "request_start",
                schema_name: REQUEST_START_SCHEMA,
                schema_version: 1,
                received_at_ms: now_ms(),
                payload: &payload,
                segment_id: None,
                key_id: None,
                signature: None,
            },
        )?;
        project_request_start_tx(
            &tx,
            ProjectRequestStart {
                sandbox_id: input.sandbox_id,
                trace_id: input.trace_id.as_str(),
                request_id: input.request_id.as_str(),
                op: input.op,
                family: input.family,
                caller_id: input.caller_id,
                args_summary: &args_summary.encoded_value(),
                args_digest: &sha256_hex(input.forwarded_bytes),
                sent_at_ms: now_ms(),
                host_boot_id: self.host_boot_id.as_str(),
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn append_trace_degraded(
        &self,
        sandbox_id: &str,
        trace_id: &TraceId,
        request_id: Option<&RequestId>,
        error: &TraceStoreError,
    ) -> Result<(), TraceStoreError> {
        let payload = proto::AuditEntry {
            entry_id: request_id.map_or_else(|| trace_id.to_string(), ToString::to_string),
            trace_id: trace_id.to_string(),
            seq: 0,
            payload: error.to_string().into_bytes(),
            previous_hash: Vec::new(),
            entry_hash: Vec::new(),
            schema_version: "1".to_owned(),
            written_at_unix_ms: now_ms(),
        }
        .encode_to_vec();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id,
                trace_id: trace_id.as_str(),
                request_id: request_id.map(RequestId::as_str),
                entry_kind: "trace_degraded",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms: now_ms(),
                payload: &payload,
                segment_id: None,
                key_id: None,
                signature: None,
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn ingest_trace_batch(
        &self,
        sandbox_id: &str,
        batch_bytes: &[u8],
    ) -> Result<(), TraceStoreError> {
        let batch = decode_trace_batch(batch_bytes)?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let trace_id = batch.records.first().map_or_else(
            || "trace_batch_empty".to_owned(),
            |record| record.trace_id.to_string(),
        );
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id,
                trace_id: &trace_id,
                request_id: batch
                    .records
                    .first()
                    .and_then(|record| record.request_id.as_ref())
                    .map(RequestId::as_str),
                entry_kind: "trace_batch",
                schema_name: TRACE_BATCH_SCHEMA,
                schema_version: 1,
                received_at_ms: now_ms(),
                payload: batch_bytes,
                segment_id: None,
                key_id: None,
                signature: None,
            },
        )?;
        project_trace_batch_tx(&tx, &batch)?;
        tx.commit()?;
        Ok(())
    }

    pub fn append_trace_event(&self, input: TraceEventInput<'_>) -> Result<(), TraceStoreError> {
        let payload = HostTraceEventPayload {
            trace_id: input.trace_id.to_string(),
            request_id: input.request_id.map(ToString::to_string),
            span_id: input.span_id,
            module: input.module.to_owned(),
            event: input.event.to_owned(),
            details_json: BoundedJson::capture(input.details, DetailBudget::EventDetails)
                .encoded_value(),
            ts_us: now_ms().saturating_mul(1000),
        };
        let payload_bytes = encode_audit_payload(&payload);
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: input.sandbox_id,
                trace_id: input.trace_id.as_str(),
                request_id: input.request_id.map(RequestId::as_str),
                entry_kind: "trace_event",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms: now_ms(),
                payload: &payload_bytes,
                segment_id: None,
                key_id: None,
                signature: None,
            },
        )?;
        project_host_trace_event_tx(&tx, &payload)?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_response_persisted(
        &self,
        input: ResponsePersistedInput<'_>,
    ) -> Result<(), TraceStoreError> {
        let status = response_status(input.response);
        let error_kind = crate::protocol::error_kind(input.response).map(ToOwned::to_owned);
        let summary = BoundedJson::capture(input.response.clone(), DetailBudget::ResponseSummary);
        let payload = ResponsePersistedPayload {
            trace_id: input.trace_id.to_string(),
            request_id: input.request_id.to_string(),
            status: status.clone(),
            error_kind: error_kind.clone(),
            received_at_ms: now_ms(),
            host_rtt_ms: input.host_rtt_ms,
            response_digest: sha256_hex(input.raw_response_bytes),
            response_len: usize_to_u64(input.raw_response_bytes.len()),
            response_summary: summary.encoded_value(),
        };
        let payload_bytes = encode_audit_payload(&payload);
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: input.sandbox_id,
                trace_id: input.trace_id.as_str(),
                request_id: Some(input.request_id.as_str()),
                entry_kind: "response_persisted",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms: payload.received_at_ms,
                payload: &payload_bytes,
                segment_id: None,
                key_id: None,
                signature: None,
            },
        )?;
        project_response_persisted_tx(&tx, &payload)?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_response_missing(
        &self,
        input: ResponseMissingInput<'_>,
    ) -> Result<(), TraceStoreError> {
        let payload = ResponseMissingPayload {
            trace_id: input.trace_id.to_string(),
            request_id: input.request_id.to_string(),
            status: input.status.to_owned(),
            error_kind: input.error_kind.to_owned(),
            message: input.message.to_owned(),
            received_at_ms: now_ms(),
        };
        let payload_bytes = encode_audit_payload(&payload);
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: input.sandbox_id,
                trace_id: input.trace_id.as_str(),
                request_id: Some(input.request_id.as_str()),
                entry_kind: "loss",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms: payload.received_at_ms,
                payload: &payload_bytes,
                segment_id: None,
                key_id: None,
                signature: None,
            },
        )?;
        tx.execute(
            "UPDATE trace_requests
             SET status=?2, error_kind=?3, received_at_ms=?4, response_summary=?5
             WHERE request_id=?1",
            params![
                payload.request_id,
                payload.status,
                payload.error_kind,
                payload.received_at_ms,
                payload.message
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn rebuild_projections(&self) -> Result<(), TraceStoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        clear_projections_tx(&tx)?;
        let rows = audit_rows_for_rebuild(&tx)?;
        for row in rows {
            match row.entry_kind.as_str() {
                "request_start" => {
                    let payload = proto::RequestStart::decode(row.payload.as_slice())?;
                    project_request_start_proto_tx(&tx, &payload)?;
                }
                "trace_batch" => {
                    let batch = decode_trace_batch(&row.payload)?;
                    project_trace_batch_tx(&tx, &batch)?;
                }
                "trace_event" => {
                    let payload = decode_audit_payload::<HostTraceEventPayload>(&row.payload)?;
                    project_host_trace_event_tx(&tx, &payload)?;
                }
                "response_persisted" => {
                    let payload = decode_audit_payload::<ResponsePersistedPayload>(&row.payload)?;
                    project_response_persisted_tx(&tx, &payload)?;
                }
                "loss" => {
                    if let Ok(payload) =
                        decode_audit_payload::<ResponseMissingPayload>(&row.payload)
                    {
                        tx.execute(
                            "UPDATE trace_requests
                             SET status=?2, error_kind=?3, received_at_ms=?4, response_summary=?5
                             WHERE request_id=?1",
                            params![
                                payload.request_id,
                                payload.status,
                                payload.error_kind,
                                payload.received_at_ms,
                                payload.message
                            ],
                        )?;
                    } else {
                        let payload = proto::AuditEntry::decode(row.payload.as_slice())?;
                        tx.execute(
                            "UPDATE trace_requests SET status='uncertain' WHERE request_id=?1",
                            params![payload.entry_id],
                        )?;
                    }
                }
                _ => {}
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn reconcile_startup_orphans(&self) -> Result<usize, TraceStoreError> {
        let rows = {
            let conn = self.lock();
            let mut stmt = conn.prepare(
                "SELECT request_id, trace_id, sandbox_id FROM trace_requests
                 WHERE status IS NULL AND host_boot_id <> ?1",
            )?;
            let rows = stmt
                .query_map(params![self.host_boot_id.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for (request_id, trace_id, sandbox_id) in &rows {
            append_loss_tx(
                &tx,
                sandbox_id,
                trace_id,
                request_id,
                "uncertain_outcome",
                "host restarted with in-flight request from prior boot",
            )?;
            tx.execute(
                "UPDATE trace_requests SET status='uncertain' WHERE request_id=?1",
                params![request_id],
            )?;
        }
        tx.commit()?;
        Ok(rows.len())
    }

    pub fn seal_all_unsealed(
        &self,
        key_id: &str,
        signing_key_bytes: &[u8],
    ) -> Result<Option<SegmentSeal>, TraceStoreError> {
        let signing_key = signing_key(signing_key_bytes)?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let after_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(last_audit_seq), 0) FROM audit_segment_seals",
            [],
            |row| row.get(0),
        )?;
        let rows = audit_hash_rows_after(&tx, after_seq)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let first = rows.first().expect("non-empty rows").audit_seq;
        let last = rows.last().expect("non-empty rows").audit_seq;
        let mut root_input = Vec::new();
        for row in &rows {
            root_input.extend_from_slice(row.entry_sha256.as_bytes());
        }
        let root_sha256 = sha256_hex(&root_input);
        let segment_id = format!("seg-{first}-{last}-{}", &root_sha256[..12]);
        let signature = signing_key.sign(root_sha256.as_bytes()).to_bytes().to_vec();
        tx.execute(
            "INSERT INTO audit_segment_seals
             (segment_id, first_audit_seq, last_audit_seq, root_sha256, key_id, signature, sealed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                segment_id,
                first,
                last,
                root_sha256,
                key_id,
                signature,
                now_ms_i64()
            ],
        )?;
        let payload = proto::AuditEntry {
            entry_id: segment_id.clone(),
            trace_id: segment_id.clone(),
            seq: u64::try_from(last).unwrap_or(u64::MAX),
            payload: root_sha256.as_bytes().to_vec(),
            previous_hash: Vec::new(),
            entry_hash: Vec::new(),
            schema_version: "1".to_owned(),
            written_at_unix_ms: now_ms(),
        }
        .encode_to_vec();
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: HOST_SANDBOX_ID,
                trace_id: &segment_id,
                request_id: None,
                entry_kind: "seal",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms: now_ms(),
                payload: &payload,
                segment_id: Some(&segment_id),
                key_id: Some(key_id),
                signature: Some(&signature),
            },
        )?;
        tx.commit()?;
        Ok(Some(SegmentSeal {
            segment_id,
            first_audit_seq: first,
            last_audit_seq: last,
            root_sha256,
        }))
    }

    pub fn verify_segment_signature(
        &self,
        segment_id: &str,
        verifying_key_bytes: &[u8],
    ) -> Result<(), TraceStoreError> {
        let verifying_key = verifying_key(verifying_key_bytes)?;
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT root_sha256, signature FROM audit_segment_seals WHERE segment_id=?1",
                params![segment_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?
            .ok_or_else(|| TraceStoreError::MissingSegmentSeal(segment_id.to_owned()))?;
        let signature_bytes: [u8; 64] = row
            .1
            .try_into()
            .map_err(|bytes: Vec<u8>| TraceStoreError::InvalidSealKeyLength(bytes.len()))?;
        let signature = Signature::from_bytes(&signature_bytes);
        verifying_key
            .verify(row.0.as_bytes(), &signature)
            .map_err(|_| TraceStoreError::BadSegmentSignature(segment_id.to_owned()))
    }

    pub fn prune_sealed_through(
        &self,
        max_audit_seq: i64,
    ) -> Result<Vec<PrunedRange>, TraceStoreError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let seals = {
            let mut stmt = tx.prepare(
                "SELECT segment_id, first_audit_seq, last_audit_seq, root_sha256
                 FROM audit_segment_seals WHERE last_audit_seq <= ?1 ORDER BY first_audit_seq",
            )?;
            let rows = stmt
                .query_map(params![max_audit_seq], |row| {
                    Ok(PrunedRange {
                        segment_id: row.get(0)?,
                        first_audit_seq: row.get(1)?,
                        last_audit_seq: row.get(2)?,
                        root_sha256: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for seal in &seals {
            let payload = proto::AuditEntry {
                entry_id: seal.segment_id.clone(),
                trace_id: seal.segment_id.clone(),
                seq: u64::try_from(seal.last_audit_seq).unwrap_or(u64::MAX),
                payload: serde_json::to_vec(seal).expect("prune range serializes"),
                previous_hash: Vec::new(),
                entry_hash: Vec::new(),
                schema_version: "1".to_owned(),
                written_at_unix_ms: now_ms(),
            }
            .encode_to_vec();
            append_audit_entry_tx(
                &tx,
                AuditAppend {
                    sandbox_id: HOST_SANDBOX_ID,
                    trace_id: &seal.segment_id,
                    request_id: None,
                    entry_kind: "prune",
                    schema_name: AUDIT_SCHEMA,
                    schema_version: 1,
                    received_at_ms: now_ms(),
                    payload: &payload,
                    segment_id: Some(&seal.segment_id),
                    key_id: None,
                    signature: None,
                },
            )?;
            tx.execute(
                "DELETE FROM audit_entries WHERE audit_seq BETWEEN ?1 AND ?2",
                params![seal.first_audit_seq, seal.last_audit_seq],
            )?;
        }
        tx.commit()?;
        Ok(seals)
    }

    pub fn verify_chain(&self) -> Result<VerificationReport, TraceStoreError> {
        let conn = self.lock();
        let pruned_ranges = load_pruned_ranges(&conn)?;
        let rows = audit_rows_for_verify(&conn)?;
        let mut previous_hash: Option<String> = None;
        let mut errors = Vec::new();
        for row in &rows {
            let payload_sha256 = sha256_hex(&row.payload);
            if payload_sha256 != row.payload_sha256 {
                errors.push(format!("audit_seq {} payload hash mismatch", row.audit_seq));
            }
            let expected = compute_entry_hash(EntryHashInput {
                sandbox_id: &row.sandbox_id,
                trace_id: &row.trace_id,
                request_id: row.request_id.as_deref(),
                entry_kind: &row.entry_kind,
                schema_name: &row.schema_name,
                schema_version: row.schema_version,
                received_at_ms: row.received_at_ms,
                payload_sha256: &row.payload_sha256,
                prev_global_sha256: row.prev_global_sha256.as_deref(),
                prev_sandbox_sha256: row.prev_sandbox_sha256.as_deref(),
            });
            if expected != row.entry_sha256 {
                errors.push(format!("audit_seq {} entry hash mismatch", row.audit_seq));
            }
            if let Some(previous_hash) = previous_hash.as_deref() {
                if row.prev_global_sha256.as_deref() != Some(previous_hash) {
                    errors.push(format!("audit_seq {} global chain break", row.audit_seq));
                }
            }
            previous_hash = Some(row.entry_sha256.clone());
        }
        Ok(VerificationReport {
            entries_checked: rows.len(),
            pruned_ranges,
            errors,
        })
    }

    pub fn events_for_trace(&self, trace_id: &str) -> Result<Vec<TraceEventRow>, TraceStoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT seq, module, event, details_json FROM trace_events
             WHERE trace_id=?1 ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![trace_id], |row| {
                Ok(TraceEventRow {
                    seq: row.get(0)?,
                    module: row.get(1)?,
                    event: row.get(2)?,
                    details_json: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn request_by_id(
        &self,
        request_id: &str,
    ) -> Result<Option<TraceRequestRow>, TraceStoreError> {
        let conn = self.lock();
        conn.query_row(
            "SELECT request_id, trace_id, sandbox_id, op, family, status
             FROM trace_requests WHERE request_id=?1",
            params![request_id],
            |row| {
                Ok(TraceRequestRow {
                    request_id: row.get(0)?,
                    trace_id: row.get(1)?,
                    sandbox_id: row.get(2)?,
                    op: row.get(3)?,
                    family: row.get(4)?,
                    status: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(TraceStoreError::from)
    }

    pub fn trace_ids_for_link(
        &self,
        link_kind: &str,
        link_id: &str,
    ) -> Result<Vec<String>, TraceStoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT trace_id FROM trace_links WHERE link_kind=?1 AND link_id=?2 ORDER BY trace_id",
        )?;
        let rows = stmt
            .query_map(params![link_kind, link_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn query_plan_for(&self, sql: &str) -> Result<Vec<String>, TraceStoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(3))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    #[cfg(test)]
    pub fn resource_span_ids_for_request(
        &self,
        request_id: &str,
    ) -> Result<Vec<Option<i64>>, TraceStoreError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT span_id FROM trace_resources WHERE request_id=?1 ORDER BY kind, span_id, ts_us",
        )?;
        let rows = stmt
            .query_map(params![request_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn sqlite_posture(&self) -> Result<SqlitePosture, TraceStoreError> {
        let conn = self.lock();
        let journal_mode: String =
            conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        let synchronous: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
        Ok(SqlitePosture {
            journal_mode,
            synchronous,
        })
    }

    fn record_host_boot(&self) -> Result<(), TraceStoreError> {
        let payload = proto::AuditEntry {
            entry_id: self.host_boot_id.to_string(),
            trace_id: self.host_boot_id.to_string(),
            seq: 0,
            payload: Vec::new(),
            previous_hash: Vec::new(),
            entry_hash: Vec::new(),
            schema_version: "1".to_owned(),
            written_at_unix_ms: now_ms(),
        }
        .encode_to_vec();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: HOST_SANDBOX_ID,
                trace_id: self.host_boot_id.as_str(),
                request_id: None,
                entry_kind: "host_boot",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms: now_ms(),
                payload: &payload,
                segment_id: None,
                key_id: None,
                signature: None,
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl TraceStoreError {
    const fn allows_read_only_degraded(&self) -> bool {
        matches!(self, Self::InjectedRequestStartFailure)
    }
}

pub struct RequestStartInput<'a> {
    pub sandbox_id: &'a str,
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub op: &'a str,
    pub family: &'a str,
    pub caller_id: Option<&'a str>,
    pub mutates_state: bool,
    pub args: Value,
    pub forwarded_bytes: &'a [u8],
}

pub struct TraceEventInput<'a> {
    pub sandbox_id: &'a str,
    pub trace_id: &'a TraceId,
    pub request_id: Option<&'a RequestId>,
    pub span_id: Option<i64>,
    pub module: &'a str,
    pub event: &'a str,
    pub details: Value,
}

pub struct ResponsePersistedInput<'a> {
    pub sandbox_id: &'a str,
    pub trace_id: &'a TraceId,
    pub request_id: &'a RequestId,
    pub response: &'a Value,
    pub raw_response_bytes: &'a [u8],
    pub host_rtt_ms: u64,
}

pub struct ResponseMissingInput<'a> {
    pub sandbox_id: &'a str,
    pub trace_id: &'a TraceId,
    pub request_id: &'a RequestId,
    pub status: &'a str,
    pub error_kind: &'a str,
    pub message: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardTraceDecision {
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub degraded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlitePosture {
    pub journal_mode: String,
    pub synchronous: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrunedRange {
    pub segment_id: String,
    pub first_audit_seq: i64,
    pub last_audit_seq: i64,
    pub root_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentSeal {
    pub segment_id: String,
    pub first_audit_seq: i64,
    pub last_audit_seq: i64,
    pub root_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationReport {
    pub entries_checked: usize,
    pub pruned_ranges: Vec<PrunedRange>,
    pub errors: Vec<String>,
}

impl VerificationReport {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEventRow {
    pub seq: i64,
    pub module: String,
    pub event: String,
    pub details_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceRequestRow {
    pub request_id: String,
    pub trace_id: String,
    pub sandbox_id: String,
    pub op: String,
    pub family: String,
    pub status: Option<String>,
}

struct AuditAppend<'a> {
    sandbox_id: &'a str,
    trace_id: &'a str,
    request_id: Option<&'a str>,
    entry_kind: &'a str,
    schema_name: &'a str,
    schema_version: i64,
    received_at_ms: u64,
    payload: &'a [u8],
    segment_id: Option<&'a str>,
    key_id: Option<&'a str>,
    signature: Option<&'a [u8]>,
}

struct ProjectRequestStart<'a> {
    sandbox_id: &'a str,
    trace_id: &'a str,
    request_id: &'a str,
    op: &'a str,
    family: &'a str,
    caller_id: Option<&'a str>,
    args_summary: &'a str,
    args_digest: &'a str,
    sent_at_ms: u64,
    host_boot_id: &'a str,
}

#[derive(Debug)]
struct RebuildAuditRow {
    entry_kind: String,
    payload: Vec<u8>,
}

#[derive(Debug)]
struct AuditVerifyRow {
    audit_seq: i64,
    sandbox_id: String,
    trace_id: String,
    request_id: Option<String>,
    entry_kind: String,
    schema_name: String,
    schema_version: i64,
    received_at_ms: u64,
    payload: Vec<u8>,
    payload_sha256: String,
    prev_global_sha256: Option<String>,
    prev_sandbox_sha256: Option<String>,
    entry_sha256: String,
}

#[derive(Debug)]
struct AuditHashRow {
    audit_seq: i64,
    entry_sha256: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct HostTraceEventPayload {
    trace_id: String,
    request_id: Option<String>,
    span_id: Option<i64>,
    module: String,
    event: String,
    details_json: String,
    ts_us: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ResponsePersistedPayload {
    trace_id: String,
    request_id: String,
    status: String,
    error_kind: Option<String>,
    received_at_ms: u64,
    host_rtt_ms: u64,
    response_digest: String,
    response_len: u64,
    response_summary: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ResponseMissingPayload {
    trace_id: String,
    request_id: String,
    status: String,
    error_kind: String,
    message: String,
    received_at_ms: u64,
}

fn apply_pragmas(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn append_loss_tx(
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
            segment_id: None,
            key_id: None,
            signature: None,
        },
    )?;
    Ok(())
}

fn append_audit_entry_tx(
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
          prev_sandbox_sha256, entry_sha256, segment_id, key_id, signature)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
            append.segment_id,
            append.key_id,
            append.signature,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

fn project_request_start_tx(
    tx: &Transaction<'_>,
    row: ProjectRequestStart<'_>,
) -> Result<(), rusqlite::Error> {
    tx.execute(
        "INSERT OR REPLACE INTO trace_requests
         (request_id, trace_id, sandbox_id, op, family, caller_id, args_summary,
          args_digest, sent_at_ms, host_boot_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            row.request_id,
            row.trace_id,
            row.sandbox_id,
            row.op,
            row.family,
            row.caller_id,
            row.args_summary,
            row.args_digest,
            row.sent_at_ms,
            row.host_boot_id,
        ],
    )?;
    Ok(())
}

fn project_request_start_proto_tx(
    tx: &Transaction<'_>,
    payload: &proto::RequestStart,
) -> Result<(), rusqlite::Error> {
    let family = family_from_op(&payload.op);
    project_request_start_tx(
        tx,
        ProjectRequestStart {
            sandbox_id: &payload.sandbox_id,
            trace_id: &payload.trace_id,
            request_id: &payload.request_id,
            op: &payload.op,
            family: &family,
            caller_id: (!payload.caller_id.is_empty()).then_some(payload.caller_id.as_str()),
            args_summary: &payload.args_summary_json,
            args_digest: &payload.args_digest,
            sent_at_ms: payload.started_at_unix_ms,
            host_boot_id: &payload.host_boot_id,
        },
    )
}

fn project_trace_batch_tx(
    tx: &Transaction<'_>,
    batch: &eos_trace::TraceBatch,
) -> Result<(), rusqlite::Error> {
    let daemon_boot_id = batch
        .daemon_boot_id
        .as_deref()
        .filter(|boot_id| !boot_id.is_empty());
    for record in &batch.records {
        let trace_id = record.trace_id.to_string();
        let request_id = record.request_id.as_ref().map(ToString::to_string);
        for span in &record.spans {
            tx.execute(
                "INSERT OR REPLACE INTO trace_spans
                 (trace_id, request_id, span_id, parent_span_id, kind, subsystem, status,
                  started_us, duration_us, fields_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    trace_id,
                    request_id,
                    u64_to_i64(span.span_id.get()),
                    span.parent_span_id.map(|id| u64_to_i64(id.get())),
                    serde_label(span.kind),
                    serde_label(span.subsystem),
                    span.status.map_or_else(|| "ok".to_owned(), serde_label),
                    u64_to_i64(span.started_at_unix_ms.saturating_mul(1000)),
                    u64_to_i64(span.duration_us),
                    span.fields.encoded_value(),
                ],
            )?;
        }
        for event in &record.events {
            let seq = next_trace_seq(tx, &trace_id)?;
            tx.execute(
                "INSERT INTO trace_events
                 (trace_id, seq, request_id, span_id, module, event, level, ts_us, details_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'info', ?7, ?8)",
                params![
                    trace_id,
                    seq,
                    request_id,
                    u64_to_i64(event.span_id.get()),
                    event.module,
                    event.name,
                    u64_to_i64(event.at_unix_ms.saturating_mul(1000)),
                    event.details.encoded_value(),
                ],
            )?;
        }
        for resource in &record.resources {
            let values = json!({
                "phase": resource.meta.phase,
                "source": resource.meta.source,
                "source_available": resource.meta.source_available,
                "read_error": resource.meta.read_error,
                "parse_error": resource.meta.parse_error,
                "sampler_duration_us": resource.meta.sampler_duration_us,
                "inflight_requests": resource.meta.inflight_requests,
                "payload": resource.payload.value,
            });
            tx.execute(
                "INSERT INTO trace_resources
                 (trace_id, request_id, span_id, ts_us, kind, values_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    trace_id,
                    request_id,
                    resource.span_id.map(|span_id| u64_to_i64(span_id.get())),
                    u64_to_i64(record.finished_at_unix_ms.saturating_mul(1000)),
                    resource.meta.stats_kind.as_str(),
                    values.to_string(),
                ],
            )?;
        }
        for link in &record.links {
            tx.execute(
                "INSERT OR IGNORE INTO trace_links
                 (trace_id, link_kind, link_id, request_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![trace_id, serde_label(link.kind), link.value, request_id],
            )?;
        }
        if let Some(request_id) = &request_id {
            project_request_rollup_tx(tx, request_id, record, daemon_boot_id)?;
        }
    }
    Ok(())
}

/// Denormalized `trace_requests` rollups derived from the daemon batch: root
/// span duration, recorded workspace route, daemon boot id, and the ordered
/// distinct span subsystems.
fn project_request_rollup_tx(
    tx: &Transaction<'_>,
    request_id: &str,
    record: &eos_trace::TraceRecord,
    daemon_boot_id: Option<&str>,
) -> Result<(), rusqlite::Error> {
    let duration_us = record
        .spans
        .iter()
        .find(|span| span.span_id == record.root_span_id)
        .map(|span| u64_to_i64(span.duration_us));
    let workspace_route = record
        .events
        .iter()
        .filter(|event| event.module == "workspace.route" && event.name == "route_selected")
        .find_map(|event| event.details.value.get("kind").and_then(Value::as_str))
        .filter(|kind| {
            matches!(
                *kind,
                "ephemeral_workspace" | "isolated_workspace" | "fast_path" | "none"
            )
        });
    let mut modules: Vec<String> = Vec::new();
    for span in &record.spans {
        let subsystem = serde_label(span.subsystem);
        if !modules.contains(&subsystem) {
            modules.push(subsystem);
        }
    }
    let modules_touched =
        (!modules.is_empty()).then(|| serde_json::to_string(&modules).unwrap_or_default());
    tx.execute(
        "UPDATE trace_requests
         SET workspace_route=COALESCE(?2, workspace_route),
             duration_us=COALESCE(?3, duration_us),
             daemon_boot_id=COALESCE(?4, daemon_boot_id),
             modules_touched=COALESCE(?5, modules_touched)
         WHERE request_id=?1",
        params![
            request_id,
            workspace_route,
            duration_us,
            daemon_boot_id,
            modules_touched
        ],
    )?;
    Ok(())
}

fn project_host_trace_event_tx(
    tx: &Transaction<'_>,
    payload: &HostTraceEventPayload,
) -> Result<(), rusqlite::Error> {
    let seq = next_trace_seq(tx, &payload.trace_id)?;
    tx.execute(
        "INSERT INTO trace_events
         (trace_id, seq, request_id, span_id, module, event, level, ts_us, details_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'info', ?7, ?8)",
        params![
            payload.trace_id,
            seq,
            payload.request_id,
            payload.span_id,
            payload.module,
            payload.event,
            u64_to_i64(payload.ts_us),
            payload.details_json,
        ],
    )?;
    Ok(())
}

fn project_response_persisted_tx(
    tx: &Transaction<'_>,
    payload: &ResponsePersistedPayload,
) -> Result<(), rusqlite::Error> {
    tx.execute(
        "UPDATE trace_requests
         SET status=?2, error_kind=?3, received_at_ms=?4, host_rtt_ms=?5,
             response_digest=?6, response_len=?7, response_summary=?8
         WHERE request_id=?1",
        params![
            payload.request_id,
            payload.status,
            payload.error_kind,
            payload.received_at_ms,
            payload.host_rtt_ms,
            payload.response_digest,
            payload.response_len,
            payload.response_summary,
        ],
    )?;
    Ok(())
}

fn clear_projections_tx(tx: &Transaction<'_>) -> Result<(), rusqlite::Error> {
    for table in [
        "trace_requests",
        "trace_spans",
        "trace_events",
        "trace_resources",
        "trace_links",
        "sandbox_heartbeats",
    ] {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    Ok(())
}

fn audit_rows_for_rebuild(tx: &Transaction<'_>) -> Result<Vec<RebuildAuditRow>, rusqlite::Error> {
    let mut stmt = tx.prepare(
        "SELECT entry_kind, payload FROM audit_entries
         WHERE entry_kind IN ('request_start', 'trace_batch', 'trace_event', 'response_persisted', 'loss')
         ORDER BY audit_seq",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RebuildAuditRow {
                entry_kind: row.get(0)?,
                payload: row.get(1)?,
            })
        })?
        .collect();
    rows
}

fn audit_hash_rows_after(
    tx: &Transaction<'_>,
    after_seq: i64,
) -> Result<Vec<AuditHashRow>, rusqlite::Error> {
    let mut stmt = tx.prepare(
        "SELECT audit_seq, entry_sha256 FROM audit_entries
         WHERE audit_seq > ?1 AND entry_kind <> 'seal'
         ORDER BY audit_seq",
    )?;
    let rows = stmt
        .query_map(params![after_seq], |row| {
            Ok(AuditHashRow {
                audit_seq: row.get(0)?,
                entry_sha256: row.get(1)?,
            })
        })?
        .collect();
    rows
}

fn audit_rows_for_verify(conn: &Connection) -> Result<Vec<AuditVerifyRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT audit_seq, sandbox_id, trace_id, request_id, entry_kind, schema_name,
                schema_version, received_at_ms, payload, payload_sha256, prev_global_sha256,
                prev_sandbox_sha256, entry_sha256
         FROM audit_entries ORDER BY audit_seq",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(AuditVerifyRow {
                audit_seq: row.get(0)?,
                sandbox_id: row.get(1)?,
                trace_id: row.get(2)?,
                request_id: row.get(3)?,
                entry_kind: row.get(4)?,
                schema_name: row.get(5)?,
                schema_version: row.get(6)?,
                received_at_ms: row.get(7)?,
                payload: row.get(8)?,
                payload_sha256: row.get(9)?,
                prev_global_sha256: row.get(10)?,
                prev_sandbox_sha256: row.get(11)?,
                entry_sha256: row.get(12)?,
            })
        })?
        .collect();
    rows
}

fn load_pruned_ranges(conn: &Connection) -> Result<Vec<PrunedRange>, rusqlite::Error> {
    let mut stmt = conn
        .prepare("SELECT payload FROM audit_entries WHERE entry_kind='prune' ORDER BY audit_seq")?;
    let rows = stmt
        .query_map([], |row| {
            let payload: Vec<u8> = row.get(0)?;
            let entry = proto::AuditEntry::decode(payload.as_slice()).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    payload.len(),
                    rusqlite::types::Type::Blob,
                    Box::new(err),
                )
            })?;
            serde_json::from_slice(&entry.payload).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    entry.payload.len(),
                    rusqlite::types::Type::Blob,
                    Box::new(err),
                )
            })
        })?
        .collect();
    rows
}

fn next_trace_seq(tx: &Transaction<'_>, trace_id: &str) -> Result<i64, rusqlite::Error> {
    tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM trace_events WHERE trace_id=?1",
        params![trace_id],
        |row| row.get(0),
    )
}

fn encode_audit_payload<T: serde::Serialize>(payload: &T) -> Vec<u8> {
    proto::AuditEntry {
        entry_id: uuid::Uuid::new_v4().simple().to_string(),
        trace_id: String::new(),
        seq: 0,
        payload: serde_json::to_vec(payload).expect("audit payload serializes"),
        previous_hash: Vec::new(),
        entry_hash: Vec::new(),
        schema_version: "1".to_owned(),
        written_at_unix_ms: now_ms(),
    }
    .encode_to_vec()
}

fn decode_audit_payload<T: serde::de::DeserializeOwned>(
    payload: &[u8],
) -> Result<T, prost::DecodeError> {
    let entry = proto::AuditEntry::decode(payload)?;
    serde_json::from_slice(&entry.payload)
        .map_err(|err| prost::DecodeError::new(format!("decode audit payload json: {err}")))
}

fn response_status(response: &Value) -> String {
    crate::protocol::response_status(response).to_owned()
}

struct EntryHashInput<'a> {
    sandbox_id: &'a str,
    trace_id: &'a str,
    request_id: Option<&'a str>,
    entry_kind: &'a str,
    schema_name: &'a str,
    schema_version: i64,
    received_at_ms: u64,
    payload_sha256: &'a str,
    prev_global_sha256: Option<&'a str>,
    prev_sandbox_sha256: Option<&'a str>,
}

fn compute_entry_hash(input: EntryHashInput<'_>) -> String {
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

fn signing_key(bytes: &[u8]) -> Result<SigningKey, TraceStoreError> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| TraceStoreError::InvalidSealKeyLength(bytes.len()))?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn verifying_key(bytes: &[u8]) -> Result<VerifyingKey, TraceStoreError> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| TraceStoreError::InvalidSealKeyLength(bytes.len()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| TraceStoreError::InvalidSealKeyLength(bytes.len()))
}

fn serde_label<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn family_from_op(op: &str) -> String {
    op.split('.').nth(1).unwrap_or("unknown").to_owned()
}

fn now_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn now_ms_i64() -> i64 {
    u64_to_i64(now_ms())
}

fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS audit_entries (
  audit_seq             INTEGER PRIMARY KEY AUTOINCREMENT,
  sandbox_id            TEXT NOT NULL,
  trace_id              TEXT NOT NULL,
  request_id            TEXT,
  entry_kind            TEXT NOT NULL,
  schema_name           TEXT NOT NULL,
  schema_version        INTEGER NOT NULL,
  received_at_ms        INTEGER NOT NULL,
  payload               BLOB NOT NULL,
  payload_sha256        TEXT NOT NULL,
  prev_global_sha256    TEXT,
  prev_sandbox_sha256   TEXT,
  entry_sha256          TEXT NOT NULL UNIQUE,
  segment_id            TEXT,
  key_id                TEXT,
  signature             BLOB
);
CREATE TABLE IF NOT EXISTS audit_segment_seals (
  segment_id       TEXT PRIMARY KEY,
  first_audit_seq  INTEGER NOT NULL,
  last_audit_seq   INTEGER NOT NULL,
  root_sha256      TEXT NOT NULL,
  key_id           TEXT NOT NULL,
  signature        BLOB NOT NULL,
  sealed_at_ms     INTEGER NOT NULL,
  export_ref       TEXT
);
CREATE TABLE IF NOT EXISTS trace_requests (
  request_id       TEXT PRIMARY KEY,
  trace_id         TEXT NOT NULL,
  sandbox_id       TEXT NOT NULL,
  op               TEXT NOT NULL,
  family           TEXT NOT NULL,
  caller_id        TEXT,
  args_summary     TEXT,
  args_digest      TEXT,
  workspace_route  TEXT CHECK (workspace_route IN
    ('ephemeral_workspace','isolated_workspace','fast_path','none') OR workspace_route IS NULL),
  status           TEXT,
  error_kind       TEXT,
  sent_at_ms       INTEGER NOT NULL,
  received_at_ms   INTEGER,
  host_rtt_ms      INTEGER,
  duration_us      INTEGER,
  daemon_boot_id   TEXT,
  host_boot_id     TEXT NOT NULL,
  modules_touched  TEXT,
  response_digest  TEXT,
  response_len     INTEGER,
  response_summary TEXT
);
CREATE TABLE IF NOT EXISTS trace_spans (
  trace_id        TEXT NOT NULL,
  request_id      TEXT,
  span_id         INTEGER NOT NULL,
  parent_span_id  INTEGER,
  kind            TEXT NOT NULL,
  subsystem       TEXT NOT NULL,
  status          TEXT NOT NULL DEFAULT 'ok',
  started_us      INTEGER NOT NULL,
  duration_us     INTEGER NOT NULL,
  fields_json     TEXT,
  PRIMARY KEY (trace_id, span_id)
);
CREATE TABLE IF NOT EXISTS trace_events (
  trace_id    TEXT NOT NULL,
  seq         INTEGER NOT NULL,
  request_id  TEXT,
  span_id     INTEGER,
  module      TEXT NOT NULL,
  event       TEXT NOT NULL,
  level       TEXT NOT NULL DEFAULT 'info',
  ts_us       INTEGER NOT NULL,
  details_json TEXT,
  PRIMARY KEY (trace_id, seq)
);
CREATE TABLE IF NOT EXISTS trace_resources (
  trace_id TEXT NOT NULL, request_id TEXT, span_id INTEGER,
  ts_us INTEGER NOT NULL, kind TEXT NOT NULL, values_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS trace_links (
  trace_id  TEXT NOT NULL,
  link_kind TEXT NOT NULL,
  link_id   TEXT NOT NULL,
  request_id TEXT,
  PRIMARY KEY (trace_id, link_kind, link_id, request_id)
);
CREATE TABLE IF NOT EXISTS sandbox_heartbeats (
  sandbox_id        TEXT NOT NULL,
  ts_ms             INTEGER NOT NULL,
  daemon_boot_id    TEXT,
  reachable         INTEGER NOT NULL,
  uptime_s          REAL,
  manifest_version  INTEGER, manifest_depth INTEGER,
  active_leases     INTEGER, storage_bytes INTEGER, layer_dirs INTEGER, staging_dirs INTEGER,
  open_isolated     INTEGER, overlay_mounts INTEGER,
  active_commands   INTEGER, running_commands INTEGER, completed_unclaimed_commands INTEGER,
  plugin_services_ok INTEGER, plugin_services_failed INTEGER,
  cpu_usage_usec    INTEGER, cpu_throttled_usec INTEGER, cpu_nr_throttled INTEGER,
  memory_current_bytes INTEGER, memory_peak_bytes INTEGER,
  memory_oom_events INTEGER, memory_oom_kill_events INTEGER,
  io_rbytes         INTEGER, io_wbytes INTEGER, process_rss_bytes INTEGER,
  inflight_requests INTEGER, spool_pending INTEGER, spool_dropped_total INTEGER,
  details_json      TEXT,
  PRIMARY KEY (sandbox_id, ts_ms)
);
CREATE INDEX IF NOT EXISTS idx_hb_time         ON sandbox_heartbeats(ts_ms);
CREATE INDEX IF NOT EXISTS idx_audit_trace     ON audit_entries(trace_id, audit_seq);
CREATE INDEX IF NOT EXISTS idx_audit_sandbox   ON audit_entries(sandbox_id, audit_seq);
CREATE INDEX IF NOT EXISTS idx_audit_request   ON audit_entries(request_id);
CREATE INDEX IF NOT EXISTS idx_requests_trace  ON trace_requests(trace_id);
CREATE INDEX IF NOT EXISTS idx_requests_sent   ON trace_requests(sent_at_ms);
CREATE INDEX IF NOT EXISTS idx_requests_status ON trace_requests(status);
CREATE INDEX IF NOT EXISTS idx_spans_kind      ON trace_spans(kind);
CREATE INDEX IF NOT EXISTS idx_spans_request   ON trace_spans(request_id);
CREATE INDEX IF NOT EXISTS idx_events_request  ON trace_events(request_id);
CREATE INDEX IF NOT EXISTS idx_resources_trace ON trace_resources(trace_id, ts_us);
CREATE INDEX IF NOT EXISTS idx_resources_request_span_kind ON trace_resources(request_id, span_id, kind);
CREATE INDEX IF NOT EXISTS idx_links_id       ON trace_links(link_kind, link_id);
CREATE INDEX IF NOT EXISTS idx_events_span    ON trace_events(trace_id, span_id);
CREATE INDEX IF NOT EXISTS idx_events_event   ON trace_events(event);
"#;

#[cfg(test)]
#[path = "../tests/unit/trace_store.rs"]
mod tests;
