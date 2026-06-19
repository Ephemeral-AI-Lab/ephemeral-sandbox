#![cfg_attr(any(test, feature = "e2e-support"), allow(dead_code))]

use std::path::Path;
#[cfg(any(test, feature = "e2e-support"))]
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
#[cfg(any(test, feature = "e2e-support"))]
use std::sync::atomic::Ordering;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, Transaction, TransactionBehavior};
use trace::BootId;

pub(crate) mod audit;
mod error;
mod events;
mod ingest;
mod payload;
mod projection;
mod query;
mod read;
mod request;
mod response;
mod schema;
mod sidecar;
mod types;

pub use error::TraceStoreError;
#[cfg(any(test, feature = "e2e-support"))]
pub use query::SqlitePosture;
pub use query::{
    TraceAuditEntryRow, TraceEventRow, TraceLinkRow, TraceRequestRow, TraceResourceRow,
    TraceSpanRow,
};
#[cfg(feature = "e2e-support")]
pub use types::TraceVerifyFailure;
pub use types::{
    ForwardTraceDecision, PendingSidecarInput, RequestStartInput, ResponseMissingInput,
    ResponsePersistedInput, TraceEventInput, TraceIngestFailedInput, TraceVerifyReport,
};

const HOST_SANDBOX_ID: &str = "_host";

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

    #[must_use]
    #[cfg(any(test, feature = "e2e-support"))]
    pub fn startup_pending_sidecar_recovery_limit_for_tests() -> usize {
        sidecar::MAX_STARTUP_PENDING_SIDECAR_RECOVERY
    }

    #[must_use]
    #[cfg(any(test, feature = "e2e-support"))]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn fail_next_request_start_for_tests(&self) {
        self.fail_next_request_start.store(true, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn fail_next_response_persisted_for_tests(&self) {
        self.fail_next_response_persisted
            .store(true, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn fail_next_trace_batch_ingest_for_tests(&self) {
        self.fail_next_trace_batch_ingest
            .store(true, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn fail_next_trace_event_for_tests(&self) {
        self.fail_next_trace_event.store(true, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn pending_sidecar_count_for_tests(&self) -> Result<usize, TraceStoreError> {
        let count: i64 =
            self.lock()
                .query_row("SELECT COUNT(*) FROM pending_trace_sidecars", [], |row| {
                    row.get(0)
                })?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
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
