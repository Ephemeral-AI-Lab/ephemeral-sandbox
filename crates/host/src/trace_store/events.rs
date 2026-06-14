use prost::Message;
use rusqlite::params;
use trace::budget::{BoundedJson, DetailBudget};
use trace::codec::proto;

use super::audit::{append_audit_entry_tx, AuditAppend, AUDIT_SCHEMA};
use super::payload::{
    encode_audit_payload, HeartbeatPayload, HostTraceEventPayload, TraceEventLossPayload,
};
use super::projection::project_host_trace_event_tx;
use super::types::TraceEventLossInput;
use super::{
    now_ms, u64_to_i64, write_transaction, HeartbeatInput, TraceEventInput, TraceStore,
    TraceStoreError, HOST_SANDBOX_ID,
};

impl TraceStore {
    pub fn append_trace_event(&self, input: TraceEventInput<'_>) -> Result<(), TraceStoreError> {
        if self
            .fail_next_trace_event
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(TraceStoreError::InjectedTraceEventFailure);
        }

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
        let tx = write_transaction(&mut conn)?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: input.sandbox_id,
                trace_id: input.trace_id.as_str(),
                request_id: input.request_id.map(trace::RequestId::as_str),
                entry_kind: "trace_event",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms: now_ms(),
                payload: &payload_bytes,
            },
        )?;
        project_host_trace_event_tx(&tx, &payload)?;
        tx.commit()?;
        Ok(())
    }

    pub fn append_trace_event_or_loss(
        &self,
        input: TraceEventInput<'_>,
    ) -> Result<(), TraceStoreError> {
        let trace_id = input.trace_id.clone();
        let request_id = input.request_id.cloned();
        let sandbox_id = input.sandbox_id.to_owned();
        let module = input.module.to_owned();
        let event = input.event.to_owned();
        match self.append_trace_event(input) {
            Ok(()) => Ok(()),
            Err(err) => {
                let message = err.to_string();
                let _ = self.record_trace_event_loss(TraceEventLossInput {
                    sandbox_id: &sandbox_id,
                    trace_id: &trace_id,
                    request_id: request_id.as_ref(),
                    module: &module,
                    event: &event,
                    message: &message,
                });
                Err(err)
            }
        }
    }

    fn record_trace_event_loss(
        &self,
        input: TraceEventLossInput<'_>,
    ) -> Result<(), TraceStoreError> {
        let received_at_ms = now_ms();
        let payload = TraceEventLossPayload {
            reason: "trace_event_append_failed".to_owned(),
            trace_id: input.trace_id.to_string(),
            request_id: input.request_id.map(ToString::to_string),
            module: input.module.to_owned(),
            event: input.event.to_owned(),
            message: input.message.to_owned(),
            received_at_ms,
        };
        let payload_bytes = encode_audit_payload(&payload);
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: input.sandbox_id,
                trace_id: input.trace_id.as_str(),
                request_id: input.request_id.map(trace::RequestId::as_str),
                entry_kind: "loss",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms,
                payload: &payload_bytes,
            },
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Durable backpressure signal: each drain pass appends an audit-backed
    /// heartbeat and projects the latest daemon-reported spool depth
    /// (`spool_pending`) plus cumulative overflow count (`spool_dropped_total`)
    /// into the query table.
    pub fn record_heartbeat(&self, input: HeartbeatInput<'_>) -> Result<(), TraceStoreError> {
        let received_at_ms = now_ms();
        let payload = HeartbeatPayload {
            sandbox_id: input.sandbox_id.to_owned(),
            daemon_boot_id: input.daemon_boot_id.map(ToOwned::to_owned),
            reachable: input.reachable,
            spool_pending: input.spool_pending,
            spool_dropped_total: input.spool_dropped_total,
            received_at_ms,
        };
        let payload_bytes = encode_audit_payload(&payload);
        let mut conn = self.lock();
        let tx = write_transaction(&mut conn)?;
        append_audit_entry_tx(
            &tx,
            AuditAppend {
                sandbox_id: input.sandbox_id,
                trace_id: "_heartbeat",
                request_id: None,
                entry_kind: "heartbeat",
                schema_name: AUDIT_SCHEMA,
                schema_version: 1,
                received_at_ms,
                payload: &payload_bytes,
            },
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO sandbox_heartbeats
             (sandbox_id, ts_ms, daemon_boot_id, reachable, spool_pending, spool_dropped_total)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                input.sandbox_id,
                received_at_ms,
                input.daemon_boot_id,
                i64::from(input.reachable),
                input.spool_pending.map(u64_to_i64),
                input.spool_dropped_total.map(u64_to_i64),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn record_host_boot(&self) -> Result<(), TraceStoreError> {
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
        let tx = write_transaction(&mut conn)?;
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
            },
        )?;
        tx.commit()?;
        Ok(())
    }
}
