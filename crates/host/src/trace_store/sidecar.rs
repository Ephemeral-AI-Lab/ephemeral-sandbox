use rusqlite::{params, OptionalExtension};
use trace::budget::{BoundedJson, DetailBudget};

use super::audit::append_loss_tx;
use super::ingest::ingest_trace_batch_tx;
use super::{
    now_ms, write_transaction, PendingSidecarInput, TraceIngestFailedInput, TraceStore,
    TraceStoreError,
};

pub(super) const MAX_STARTUP_PENDING_SIDECAR_RECOVERY: usize = 128;

impl TraceStore {
    pub fn record_pending_sidecar(
        &self,
        input: PendingSidecarInput<'_>,
    ) -> Result<(), TraceStoreError> {
        let bounded = BoundedJson::capture(
            serde_json::json!({"batch_len": input.batch_bytes.len()}),
            DetailBudget::SidecarRecord,
        );
        if input.batch_bytes.len() > DetailBudget::SidecarRecord.bytes() {
            let mut conn = self.lock();
            let tx = write_transaction(&mut conn)?;
            append_loss_tx(
                &tx,
                input.sandbox_id,
                input.trace_id.as_str(),
                input.request_id.as_str(),
                "pending_sidecar_too_large",
                &bounded.encoded_value(),
            )?;
            tx.commit()?;
            return Ok(());
        }

        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        tx.execute(
            "INSERT INTO pending_trace_sidecars
             (sandbox_id, trace_id, request_id, batch, first_error, last_error,
              retry_count, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 0, ?6, ?6)",
            params![
                input.sandbox_id,
                input.trace_id.as_str(),
                input.request_id.as_str(),
                input.batch_bytes,
                input.error,
                now_ms(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn recover_pending_sidecars(&self) -> Result<usize, TraceStoreError> {
        self.recover_pending_sidecars_with_limit(usize::MAX)
    }

    pub(crate) fn recover_startup_pending_sidecars(&self) -> Result<usize, TraceStoreError> {
        self.recover_pending_sidecars_with_limit(MAX_STARTUP_PENDING_SIDECAR_RECOVERY)
    }

    fn recover_pending_sidecars_with_limit(&self, limit: usize) -> Result<usize, TraceStoreError> {
        let mut recovered = 0_usize;
        let mut last_seen_id = 0_i64;
        while recovered < limit {
            let pending = self.next_pending_sidecar(last_seen_id)?;
            let Some((id, sandbox_id, batch)) = pending else {
                break;
            };
            last_seen_id = id;
            match self.ingest_pending_sidecar(id, &sandbox_id, &batch) {
                Ok(()) => {
                    recovered = recovered.saturating_add(1);
                }
                Err(err) => {
                    self.lock().execute(
                        "UPDATE pending_trace_sidecars
                         SET retry_count=retry_count+1, last_error=?2, updated_at_ms=?3
                         WHERE id=?1",
                        params![id, err.to_string(), now_ms()],
                    )?;
                }
            }
        }
        Ok(recovered)
    }

    fn next_pending_sidecar(
        &self,
        after_id: i64,
    ) -> Result<Option<(i64, String, Vec<u8>)>, TraceStoreError> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT id, sandbox_id, batch FROM pending_trace_sidecars
                 WHERE id > ?1
                 ORDER BY id
                 LIMIT 1",
                params![after_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()?)
    }

    fn ingest_pending_sidecar(
        &self,
        id: i64,
        sandbox_id: &str,
        batch_bytes: &[u8],
    ) -> Result<(), TraceStoreError> {
        let batch = self.prepare_trace_batch_ingest(batch_bytes)?;
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        ingest_trace_batch_tx(&tx, sandbox_id, &batch, batch_bytes)?;
        tx.execute("DELETE FROM pending_trace_sidecars WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_trace_ingest_failed(
        &self,
        input: TraceIngestFailedInput<'_>,
    ) -> Result<(), TraceStoreError> {
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        append_loss_tx(
            &tx,
            input.sandbox_id,
            input.trace_id.as_str(),
            input.request_id.as_str(),
            "trace_ingest_failed",
            &format!("{}: {}", input.error_kind, input.message),
        )?;
        tx.commit()?;
        Ok(())
    }
}
