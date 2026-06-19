use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use trace::{BootId, SpanUid, TraceRecord, TraceSpool};
#[cfg(test)]
use trace::{ExportId, TraceExportBatch};

static CONNECTION_SEQ: AtomicU64 = AtomicU64::new(1);
static BACKGROUND_SPOOL: OnceLock<Mutex<TraceSpool>> = OnceLock::new();
static DAEMON_BOOT_ID: OnceLock<BootId> = OnceLock::new();

pub(crate) fn daemon_boot_id() -> &'static BootId {
    DAEMON_BOOT_ID.get_or_init(BootId::new)
}

#[derive(Debug, Clone)]
pub(crate) struct RequestTraceFacts {
    pub connection_id: String,
    pub accepted_at_unix_ms: u64,
    pub listener_kind: &'static str,
    pub peer_addr: Option<String>,
    pub local_addr: Option<String>,
    pub is_tcp: bool,
    pub request_bytes: usize,
    pub read_duration_us: u64,
    pub auth_required: bool,
    pub auth_ok: bool,
    pub protocol_version: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct RequestTraceEvent {
    pub(crate) span_id: SpanUid,
    pub(crate) name: String,
    pub(crate) module: String,
    pub(crate) details: Value,
}

impl RequestTraceEvent {
    #[cfg(test)]
    pub(crate) fn operation(
        module: impl Into<String>,
        name: impl Into<String>,
        details: Value,
    ) -> Self {
        Self {
            span_id: SpanUid::new(4),
            name: name.into(),
            module: module.into(),
            details,
        }
    }
}

pub(crate) fn next_connection_id() -> String {
    format!(
        "daemon-conn-{}",
        CONNECTION_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn push_background_record(record: TraceRecord) {
    let Ok(mut spool) = background_spool().lock() else {
        return;
    };
    let _ = spool.push(record);
}

#[cfg(test)]
pub(crate) fn lease_background_records(max_records: usize) -> TraceExportBatch {
    let Ok(mut spool) = background_spool().lock() else {
        return empty_trace_export_batch();
    };
    spool.lease_batch(max_records, Some(daemon_boot_id().to_string()))
}

#[cfg(test)]
pub(crate) fn ack_background_export(
    export_id: &ExportId,
    batch_sha256: &str,
    record_count: usize,
) -> bool {
    background_spool()
        .lock()
        .is_ok_and(|mut spool| spool.ack_batch(export_id, batch_sha256, record_count))
}

#[cfg(test)]
fn empty_trace_export_batch() -> TraceExportBatch {
    TraceExportBatch {
        export_id: None,
        record_count: 0,
        spool_pending_after: 0,
        dropped_traces: 0,
        batch_sha256: None,
        trace_batch_bytes: None,
    }
}

fn background_spool() -> &'static Mutex<TraceSpool> {
    BACKGROUND_SPOOL.get_or_init(|| Mutex::new(TraceSpool::default()))
}

pub(crate) fn now_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}
