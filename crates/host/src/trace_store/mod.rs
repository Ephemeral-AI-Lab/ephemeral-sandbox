use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use trace::budget::{BoundedJson, DetailBudget};
use trace::codec::proto;
use trace::{sha256_hex, BootId};

mod audit;
mod events;
mod ingest;
mod payload;
mod projection;
mod query;
mod read;
mod response;
mod schema;
mod sidecar;
mod types;

use audit::{append_audit_entry_tx, AuditAppend, AUDIT_SCHEMA, REQUEST_START_SCHEMA};
use payload::{encode_audit_payload, TraceDegradedPayload};
use projection::{project_request_start_tx, project_trace_degraded_tx, ProjectRequestStart};

#[cfg(any(test, feature = "e2e-support"))]
pub use query::SqlitePosture;
pub use query::{
    TraceAuditEntryRow, TraceEventRow, TraceLinkRow, TraceRequestRow, TraceResourceRow,
    TraceSpanRow,
};
use types::DegradedRequestInput;
#[cfg(feature = "e2e-support")]
pub use types::TraceVerifyFailure;
pub use types::{
    ForwardTraceDecision, HeartbeatInput, PendingSidecarInput, RequestStartInput,
    ResponseMissingInput, ResponsePersistedInput, TraceEventInput, TraceIngestFailedInput,
    TraceVerifyReport,
};

const HOST_SANDBOX_ID: &str = "_host";

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
    Decode(#[from] trace::DecodeTraceError),
    #[error("trace protobuf decode error: {0}")]
    ProstDecode(#[from] prost::DecodeError),
    #[error("trace store request-start append intentionally failed for test")]
    InjectedRequestStartFailure,
    #[error("trace store response-persisted append intentionally failed for test")]
    InjectedResponsePersistedFailure,
    #[error("trace batch ingest intentionally failed for test")]
    InjectedTraceBatchIngestFailure,
    #[error("trace event append intentionally failed for test")]
    InjectedTraceEventFailure,
    #[error("trace export {export_id} replay digest mismatch")]
    TraceExportReplayMismatch { export_id: String },
}

pub struct TraceStore {
    #[cfg(any(test, feature = "e2e-support"))]
    db_path: PathBuf,
    conn: Mutex<Connection>,
    host_boot_id: BootId,
    fail_next_request_start: AtomicBool,
    fail_next_response_persisted: AtomicBool,
    fail_next_trace_batch_ingest: AtomicBool,
    fail_next_trace_event: AtomicBool,
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
        schema::initialize(&conn)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600));
        }

        let store = Self {
            #[cfg(any(test, feature = "e2e-support"))]
            db_path,
            conn: Mutex::new(conn),
            host_boot_id: BootId::new(),
            fail_next_request_start: AtomicBool::new(false),
            fail_next_response_persisted: AtomicBool::new(false),
            fail_next_trace_batch_ingest: AtomicBool::new(false),
            fail_next_trace_event: AtomicBool::new(false),
        };
        store.record_host_boot()?;
        store.reconcile_startup_orphans()?;
        store.recover_startup_pending_sidecars()?;
        Ok(store)
    }

    #[cfg(test)]
    #[must_use]
    pub fn startup_pending_sidecar_recovery_limit_for_tests() -> usize {
        sidecar::MAX_STARTUP_PENDING_SIDECAR_RECOVERY
    }

    #[cfg(any(test, feature = "e2e-support"))]
    #[must_use]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    #[cfg(test)]
    pub fn fail_next_request_start_for_tests(&self) {
        self.fail_next_request_start.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn fail_next_response_persisted_for_tests(&self) {
        self.fail_next_response_persisted
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn fail_next_trace_batch_ingest_for_tests(&self) {
        self.fail_next_trace_batch_ingest
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn fail_next_trace_event_for_tests(&self) {
        self.fail_next_trace_event.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn pending_sidecar_count_for_tests(&self) -> Result<usize, TraceStoreError> {
        let count: i64 =
            self.lock()
                .query_row("SELECT COUNT(*) FROM pending_trace_sidecars", [], |row| {
                    row.get(0)
                })?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    pub fn prepare_forward(
        &self,
        input: RequestStartInput<'_>,
    ) -> Result<ForwardTraceDecision, TraceStoreError> {
        let degraded_input = DegradedRequestInput {
            sandbox_id: input.sandbox_id,
            trace_id: input.trace_id.clone(),
            request_id: input.request_id.clone(),
            op: input.op,
            family: input.family,
            caller_id: input.caller_id,
            args: input.args.clone(),
        };
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
                // Best-effort marker: a read-only op proceeds degraded even when the
                // store is too unavailable to record the marker. An untraceable read
                // is acceptable; an untraceable mutation is not, and fails closed in
                // the catch-all arm below.
                let _ = self.append_trace_degraded(&degraded_input, &err);
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
        let redacted_args = trace::budget::redact_for_audit(input.args.clone());
        // Digest/length describe the request args only. They are never computed
        // over the forwarded TCP frame, which carries `_eos_daemon_auth_token`;
        // the security rule forbids recording, hashing, or length-recording the
        // auth token (SPEC: Transport connection lifecycle -> Security rules).
        let args_bytes = serde_json::to_vec(&redacted_args).unwrap_or_default();
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
            args_len: usize_to_u64(args_bytes.len()),
            args_digest: sha256_hex(&args_bytes),
        }
        .encode_to_vec();

        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
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
                args_digest: &sha256_hex(&args_bytes),
                sent_at_ms: now_ms(),
                host_boot_id: self.host_boot_id.as_str(),
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    fn append_trace_degraded(
        &self,
        input: &DegradedRequestInput<'_>,
        error: &TraceStoreError,
    ) -> Result<(), TraceStoreError> {
        let args_summary =
            BoundedJson::capture(input.args.clone(), DetailBudget::RequestArgsSummary);
        let redacted_args = trace::budget::redact_for_audit(input.args.clone());
        // Token-free args digest only — never the forwarded auth-bearing frame.
        let args_bytes = serde_json::to_vec(&redacted_args).unwrap_or_default();
        let payload = TraceDegradedPayload {
            trace_id: input.trace_id.to_string(),
            request_id: input.request_id.to_string(),
            sandbox_id: input.sandbox_id.to_owned(),
            op: input.op.to_owned(),
            family: input.family.to_owned(),
            caller_id: input.caller_id.map(ToOwned::to_owned),
            args_summary: args_summary.encoded_value(),
            args_digest: sha256_hex(&args_bytes),
            sent_at_ms: now_ms(),
            host_boot_id: self.host_boot_id.to_string(),
            error_kind: "trace_degraded".to_owned(),
            message: error.to_string(),
        };
        let payload_bytes = encode_audit_payload(&payload);
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: input.sandbox_id,
                trace_id: input.trace_id.as_str(),
                request_id: Some(input.request_id.as_str()),
                entry_kind: "trace_degraded",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms: now_ms(),
                payload: &payload_bytes,
            },
        )?;
        project_trace_degraded_tx(&tx, &payload)?;
        tx.commit()?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl TraceStoreError {
    /// Read-only ops proceed with a `trace_degraded` marker when the
    /// request-start append fails because the store itself is unavailable: the
    /// test injection or a real sqlite error (disk-full, lock contention, I/O).
    /// Schema/decode errors are not request-start append failures and never
    /// reach this path; mutating ops always fail closed regardless.
    const fn allows_read_only_degraded(&self) -> bool {
        matches!(self, Self::InjectedRequestStartFailure | Self::Sqlite(_))
    }
}

pub(super) fn write_transaction(conn: &mut Connection) -> Result<Transaction<'_>, rusqlite::Error> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

pub(super) fn now_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

pub(super) fn u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(super) fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "../../tests/unit/trace_store.rs"]
mod tests;
