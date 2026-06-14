use std::collections::HashMap;

use super::audit::{compute_entry_hash, EntryHashInput};
use super::types::verify_scope;
use super::{
    query, sha256_hex, TraceAuditEntryRow, TraceEventRow, TraceLinkRow, TraceRequestRow,
    TraceResourceRow, TraceSpanRow, TraceStore, TraceStoreError, TraceVerifyReport,
};

impl TraceStore {
    #[cfg(any(test, feature = "e2e-support"))]
    pub fn events_for_trace(&self, trace_id: &str) -> Result<Vec<TraceEventRow>, TraceStoreError> {
        query::events_for_trace(&self.lock(), trace_id)
    }

    pub fn events_for_trace_limited(
        &self,
        trace_id: &str,
        limit: usize,
    ) -> Result<Vec<TraceEventRow>, TraceStoreError> {
        query::events_for_trace_limited(&self.lock(), trace_id, limit)
    }

    pub fn event_count_for_trace(&self, trace_id: &str) -> Result<usize, TraceStoreError> {
        let count: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM trace_events WHERE trace_id=?1",
            [trace_id],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn request_by_id(
        &self,
        request_id: &str,
    ) -> Result<Option<TraceRequestRow>, TraceStoreError> {
        query::request_by_id(&self.lock(), request_id)
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn trace_ids_for_link(
        &self,
        link_kind: &str,
        link_id: &str,
    ) -> Result<Vec<String>, TraceStoreError> {
        query::trace_ids_for_link(&self.lock(), link_kind, link_id)
    }

    pub fn recent_requests(
        &self,
        sandbox_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TraceRequestRow>, TraceStoreError> {
        query::recent_requests(&self.lock(), sandbox_id, limit)
    }

    pub fn requests_for_trace_limited(
        &self,
        trace_id: &str,
        limit: usize,
    ) -> Result<Vec<TraceRequestRow>, TraceStoreError> {
        query::requests_for_trace_limited(&self.lock(), trace_id, limit)
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn spans_for_trace(&self, trace_id: &str) -> Result<Vec<TraceSpanRow>, TraceStoreError> {
        query::spans_for_trace(&self.lock(), trace_id)
    }

    pub fn spans_for_trace_limited(
        &self,
        trace_id: &str,
        limit: usize,
    ) -> Result<Vec<TraceSpanRow>, TraceStoreError> {
        query::spans_for_trace_limited(&self.lock(), trace_id, limit)
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn resources_for_trace(
        &self,
        trace_id: &str,
    ) -> Result<Vec<TraceResourceRow>, TraceStoreError> {
        query::resources_for_trace(&self.lock(), trace_id)
    }

    pub fn resources_for_trace_limited(
        &self,
        trace_id: &str,
        limit: usize,
    ) -> Result<Vec<TraceResourceRow>, TraceStoreError> {
        query::resources_for_trace_limited(&self.lock(), trace_id, limit)
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn links_for_trace(&self, trace_id: &str) -> Result<Vec<TraceLinkRow>, TraceStoreError> {
        query::links_for_trace(&self.lock(), trace_id)
    }

    pub fn links_for_trace_limited(
        &self,
        trace_id: &str,
        limit: usize,
    ) -> Result<Vec<TraceLinkRow>, TraceStoreError> {
        query::links_for_trace_limited(&self.lock(), trace_id, limit)
    }

    pub fn audit_entries_for_trace_limited(
        &self,
        trace_id: &str,
        limit: usize,
    ) -> Result<Vec<TraceAuditEntryRow>, TraceStoreError> {
        query::audit_entries_for_trace_limited(&self.lock(), trace_id, limit)
    }

    pub fn verify_audit(
        &self,
        trace_id: Option<&str>,
    ) -> Result<TraceVerifyReport, TraceStoreError> {
        let conn = self.lock();
        let rows = query::audit_rows_for_verification(&conn)?;
        let mut previous_global: Option<String> = None;
        let mut previous_by_sandbox: HashMap<String, String> = HashMap::new();
        let mut checked_entries = 0_usize;
        for row in &rows {
            checked_entries = checked_entries.saturating_add(1);
            let payload_sha256 = sha256_hex(&row.payload);
            if row.payload_sha256 != payload_sha256 {
                return Ok(TraceVerifyReport::failed(
                    trace_id,
                    checked_entries,
                    row.audit_seq,
                    "payload_hash_mismatch",
                    format!(
                        "payload hash mismatch: stored {}, computed {}",
                        row.payload_sha256, payload_sha256
                    ),
                ));
            }
            if row.prev_global_sha256.as_deref() != previous_global.as_deref() {
                return Ok(TraceVerifyReport::failed(
                    trace_id,
                    checked_entries,
                    row.audit_seq,
                    "global_chain_mismatch",
                    "previous global hash does not match prior audit entry",
                ));
            }
            let previous_sandbox = previous_by_sandbox.get(&row.sandbox_id).map(String::as_str);
            if row.prev_sandbox_sha256.as_deref() != previous_sandbox {
                return Ok(TraceVerifyReport::failed(
                    trace_id,
                    checked_entries,
                    row.audit_seq,
                    "sandbox_chain_mismatch",
                    "previous sandbox hash does not match prior audit entry for sandbox",
                ));
            }
            let received_at_ms = u64::try_from(row.received_at_ms).unwrap_or_default();
            let expected = compute_entry_hash(EntryHashInput {
                sandbox_id: &row.sandbox_id,
                trace_id: &row.trace_id,
                request_id: row.request_id.as_deref(),
                entry_kind: &row.entry_kind,
                schema_name: &row.schema_name,
                schema_version: row.schema_version,
                received_at_ms,
                payload_sha256: &row.payload_sha256,
                prev_global_sha256: row.prev_global_sha256.as_deref(),
                prev_sandbox_sha256: row.prev_sandbox_sha256.as_deref(),
            });
            if row.entry_sha256 != expected {
                return Ok(TraceVerifyReport::failed(
                    trace_id,
                    checked_entries,
                    row.audit_seq,
                    "entry_hash_mismatch",
                    format!(
                        "entry hash mismatch: stored {}, computed {}",
                        row.entry_sha256, expected
                    ),
                ));
            }
            previous_global = Some(row.entry_sha256.clone());
            previous_by_sandbox.insert(row.sandbox_id.clone(), row.entry_sha256.clone());
        }

        let gaps = query::projection_gaps(&conn, trace_id)?;
        if let Some(gap) = gaps.first() {
            return Ok(TraceVerifyReport::failed(
                trace_id,
                checked_entries,
                gap.audit_seq,
                "projection_missing_request",
                format!(
                    "audit entry request_id {} has no trace_requests projection row",
                    gap.request_id
                ),
            ));
        }

        Ok(TraceVerifyReport {
            ok: true,
            trace_id: trace_id.map(ToOwned::to_owned),
            scope: verify_scope(trace_id).to_owned(),
            checked_entries,
            first_error: None,
        })
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn query_plan_for(&self, sql: &str) -> Result<Vec<String>, TraceStoreError> {
        query::query_plan_for(&self.lock(), sql)
    }

    #[cfg(test)]
    pub fn resource_span_ids_for_request(
        &self,
        request_id: &str,
    ) -> Result<Vec<Option<i64>>, TraceStoreError> {
        query::resource_span_ids_for_request(&self.lock(), request_id)
    }

    #[cfg(any(test, feature = "e2e-support"))]
    pub fn sqlite_posture(&self) -> Result<super::SqlitePosture, TraceStoreError> {
        query::sqlite_posture(&self.lock())
    }
}
