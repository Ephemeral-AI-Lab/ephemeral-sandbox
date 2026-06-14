use rusqlite::{params, OptionalExtension, Transaction};
use trace::codec::decode_trace_batch;
use trace::{RequestId, TraceBatch};

use super::audit::{
    append_audit_entry_tx, append_dropped_traces_loss_tx, AuditAppend, TRACE_BATCH_SCHEMA,
};
use super::projection::project_trace_batch_tx;
use super::{now_ms, u64_to_i64, write_transaction, TraceStore, TraceStoreError};

impl TraceStore {
    pub fn ingest_trace_batch(
        &self,
        sandbox_id: &str,
        batch_bytes: &[u8],
    ) -> Result<(), TraceStoreError> {
        let batch = self.prepare_trace_batch_ingest(batch_bytes)?;
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        ingest_trace_batch_tx(&tx, sandbox_id, &batch, batch_bytes)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn prepare_trace_batch_ingest(
        &self,
        batch_bytes: &[u8],
    ) -> Result<TraceBatch, TraceStoreError> {
        let batch = decode_trace_batch(batch_bytes)?;
        if self
            .fail_next_trace_batch_ingest
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(TraceStoreError::InjectedTraceBatchIngestFailure);
        }
        Ok(batch)
    }

    pub fn ingest_trace_export_batch_once(
        &self,
        sandbox_id: &str,
        export_id: &str,
        batch_sha256: &str,
        record_count: u64,
        batch_bytes: &[u8],
    ) -> Result<(), TraceStoreError> {
        let batch = decode_trace_batch(batch_bytes)?;
        if self
            .fail_next_trace_batch_ingest
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(TraceStoreError::InjectedTraceBatchIngestFailure);
        }
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        if let Some((existing_sha, existing_count)) = trace_export_batch_tx(&tx, export_id)? {
            if existing_sha == batch_sha256 && existing_count == record_count {
                tx.commit()?;
                return Ok(());
            }
            return Err(TraceStoreError::TraceExportReplayMismatch {
                export_id: export_id.to_owned(),
            });
        }

        ingest_trace_batch_tx(&tx, sandbox_id, &batch, batch_bytes)?;
        tx.execute(
            "INSERT INTO trace_export_batches
             (export_id, sandbox_id, daemon_boot_id, batch_sha256, record_count, ingested_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                export_id,
                sandbox_id,
                batch.daemon_boot_id.as_deref(),
                batch_sha256,
                u64_to_i64(record_count),
                u64_to_i64(now_ms()),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_trace_export_ack_success(&self, export_id: &str) -> Result<(), TraceStoreError> {
        let now = u64_to_i64(now_ms());
        self.lock().execute(
            "UPDATE trace_export_batches
             SET acked_at_ms=?2, last_ack_error=NULL
             WHERE export_id=?1",
            params![export_id, now],
        )?;
        Ok(())
    }

    pub fn record_trace_export_ack_failure(
        &self,
        export_id: &str,
        error: &str,
    ) -> Result<(), TraceStoreError> {
        self.lock().execute(
            "UPDATE trace_export_batches
             SET retry_count=retry_count+1, last_ack_error=?2
             WHERE export_id=?1",
            params![export_id, error],
        )?;
        Ok(())
    }
}

fn trace_export_batch_tx(
    tx: &Transaction<'_>,
    export_id: &str,
) -> Result<Option<(String, u64)>, TraceStoreError> {
    let existing = tx
        .query_row(
            "SELECT batch_sha256, record_count FROM trace_export_batches WHERE export_id=?1",
            params![export_id],
            |row| {
                let sha: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((sha, count))
            },
        )
        .optional()?;
    Ok(existing.map(|(sha, count)| (sha, u64::try_from(count).unwrap_or(0))))
}

pub(super) fn ingest_trace_batch_tx(
    tx: &Transaction<'_>,
    sandbox_id: &str,
    batch: &TraceBatch,
    batch_bytes: &[u8],
) -> Result<(), TraceStoreError> {
    let trace_id = batch.records.first().map_or_else(
        || "trace_batch_empty".to_owned(),
        |record| record.trace_id.to_string(),
    );
    append_audit_entry_tx(
        tx,
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
        },
    )?;
    if let Some(dropped_delta) = dropped_trace_delta_tx(
        tx,
        sandbox_id,
        batch.daemon_boot_id.as_deref(),
        batch.dropped_traces,
    )? {
        append_dropped_traces_loss_tx(
            tx,
            sandbox_id,
            dropped_delta,
            batch.dropped_traces,
            batch
                .daemon_boot_id
                .as_deref()
                .filter(|boot_id| !boot_id.is_empty()),
        )?;
    }
    project_trace_batch_tx(tx, batch)?;
    Ok(())
}

fn dropped_trace_delta_tx(
    tx: &Transaction<'_>,
    sandbox_id: &str,
    daemon_boot_id: Option<&str>,
    dropped_traces_total: u64,
) -> Result<Option<u64>, rusqlite::Error> {
    if dropped_traces_total == 0 {
        return Ok(None);
    }
    let daemon_boot_id = daemon_boot_id
        .filter(|boot_id| !boot_id.is_empty())
        .unwrap_or("_unknown");
    let previous: Option<i64> = tx
        .query_row(
            "SELECT dropped_traces_total FROM trace_spool_drop_cursors
             WHERE sandbox_id=?1 AND daemon_boot_id=?2",
            params![sandbox_id, daemon_boot_id],
            |row| row.get(0),
        )
        .optional()?;
    let previous = previous
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO trace_spool_drop_cursors
         (sandbox_id, daemon_boot_id, dropped_traces_total, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(sandbox_id, daemon_boot_id) DO UPDATE SET
           dropped_traces_total=excluded.dropped_traces_total,
           updated_at_ms=excluded.updated_at_ms",
        params![
            sandbox_id,
            daemon_boot_id,
            u64_to_i64(dropped_traces_total),
            u64_to_i64(now_ms())
        ],
    )?;
    Ok((dropped_traces_total > previous).then_some(dropped_traces_total - previous))
}
