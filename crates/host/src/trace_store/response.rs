use prost::Message;
use rusqlite::params;
use trace::budget::{BoundedJson, DetailBudget};
use trace::sha256_hex;

use super::audit::{
    append_audit_entry_tx, append_loss_tx, AuditAppend, AUDIT_SCHEMA, RESPONSE_PERSISTED_SCHEMA,
};
use super::payload::{
    encode_audit_payload, response_persisted_to_proto, ResponseMissingPayload,
    ResponsePersistedPayload,
};
use super::projection::project_response_persisted_tx;
use super::{
    now_ms, usize_to_u64, write_transaction, ResponseMissingInput, ResponsePersistedInput,
    TraceStore, TraceStoreError,
};

impl TraceStore {
    pub fn record_response_persisted(
        &self,
        input: ResponsePersistedInput<'_>,
    ) -> Result<(), TraceStoreError> {
        if self
            .fail_next_response_persisted
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(TraceStoreError::InjectedResponsePersistedFailure);
        }

        let status = crate::daemon_wire::response_envelope_status(input.response).to_owned();
        let error_kind =
            crate::daemon_wire::response_fault_kind(input.response).map(ToOwned::to_owned);
        let summary = BoundedJson::capture(input.response.clone(), DetailBudget::ResponseSummary);
        let payload = ResponsePersistedPayload {
            trace_id: input.trace_id.to_string(),
            request_id: input.request_id.to_string(),
            status,
            error_kind: error_kind.clone(),
            received_at_ms: now_ms(),
            host_rtt_ms: input.host_rtt_ms,
            response_digest: sha256_hex(input.raw_response_bytes),
            response_len: usize_to_u64(input.raw_response_bytes.len()),
            response_summary: summary.encoded_value(),
        };
        let payload_bytes = response_persisted_to_proto(&payload).encode_to_vec();
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: input.sandbox_id,
                trace_id: input.trace_id.as_str(),
                request_id: Some(input.request_id.as_str()),
                entry_kind: "response_persisted",
                schema_name: RESPONSE_PERSISTED_SCHEMA,
                schema_version: 1,
                received_at_ms: payload.received_at_ms,
                payload: &payload_bytes,
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
        let tx = write_transaction(&mut conn)?;
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
        let tx = write_transaction(&mut conn)?;
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
}
