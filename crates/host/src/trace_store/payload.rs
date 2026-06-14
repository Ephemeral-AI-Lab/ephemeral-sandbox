use prost::Message;
use trace::codec::proto;

use super::now_ms;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct HostTraceEventPayload {
    pub(super) trace_id: String,
    pub(super) request_id: Option<String>,
    pub(super) span_id: Option<i64>,
    pub(super) module: String,
    pub(super) event: String,
    pub(super) details_json: String,
    pub(super) ts_us: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct TraceDegradedPayload {
    pub(super) trace_id: String,
    pub(super) request_id: String,
    pub(super) sandbox_id: String,
    pub(super) op: String,
    pub(super) family: String,
    pub(super) caller_id: Option<String>,
    pub(super) args_summary: String,
    pub(super) args_digest: String,
    pub(super) sent_at_ms: u64,
    pub(super) host_boot_id: String,
    pub(super) error_kind: String,
    pub(super) message: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct ResponsePersistedPayload {
    pub(super) trace_id: String,
    pub(super) request_id: String,
    pub(super) status: String,
    pub(super) error_kind: Option<String>,
    pub(super) received_at_ms: u64,
    pub(super) host_rtt_ms: u64,
    pub(super) response_digest: String,
    pub(super) response_len: u64,
    pub(super) response_summary: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct ResponseMissingPayload {
    pub(super) trace_id: String,
    pub(super) request_id: String,
    pub(super) status: String,
    pub(super) error_kind: String,
    pub(super) message: String,
    pub(super) received_at_ms: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct HeartbeatPayload {
    pub(super) sandbox_id: String,
    pub(super) daemon_boot_id: Option<String>,
    pub(super) reachable: bool,
    pub(super) spool_pending: Option<u64>,
    pub(super) spool_dropped_total: Option<u64>,
    pub(super) received_at_ms: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct TraceEventLossPayload {
    pub(super) reason: String,
    pub(super) trace_id: String,
    pub(super) request_id: Option<String>,
    pub(super) module: String,
    pub(super) event: String,
    pub(super) message: String,
    pub(super) received_at_ms: u64,
}

pub(super) fn response_persisted_to_proto(
    payload: &ResponsePersistedPayload,
) -> proto::ResponsePersisted {
    proto::ResponsePersisted {
        trace_id: payload.trace_id.clone(),
        request_id: payload.request_id.clone(),
        status: payload.status.clone(),
        error_kind: payload.error_kind.clone().unwrap_or_default(),
        received_at_unix_ms: payload.received_at_ms,
        host_rtt_ms: payload.host_rtt_ms,
        response_digest: payload.response_digest.clone(),
        response_len: payload.response_len,
        response_summary_json: payload.response_summary.clone(),
    }
}

pub(super) fn encode_audit_payload<T: serde::Serialize>(payload: &T) -> Vec<u8> {
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
