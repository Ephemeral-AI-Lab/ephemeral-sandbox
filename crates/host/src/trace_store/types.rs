use serde_json::Value;
use trace::{RequestId, TraceId};

pub struct RequestStartInput<'a> {
    pub sandbox_id: &'a str,
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub op: &'a str,
    pub family: &'a str,
    pub caller_id: Option<&'a str>,
    pub mutates_state: bool,
    pub args: Value,
}

pub(super) struct DegradedRequestInput<'a> {
    pub(super) sandbox_id: &'a str,
    pub(super) trace_id: TraceId,
    pub(super) request_id: RequestId,
    pub(super) op: &'a str,
    pub(super) family: &'a str,
    pub(super) caller_id: Option<&'a str>,
    pub(super) args: Value,
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

pub(super) struct TraceEventLossInput<'a> {
    pub(super) sandbox_id: &'a str,
    pub(super) trace_id: &'a TraceId,
    pub(super) request_id: Option<&'a RequestId>,
    pub(super) module: &'a str,
    pub(super) event: &'a str,
    pub(super) message: &'a str,
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

pub struct TraceIngestFailedInput<'a> {
    pub sandbox_id: &'a str,
    pub trace_id: &'a TraceId,
    pub request_id: &'a RequestId,
    pub error_kind: &'a str,
    pub message: &'a str,
}

pub struct PendingSidecarInput<'a> {
    pub sandbox_id: &'a str,
    pub trace_id: &'a TraceId,
    pub request_id: &'a RequestId,
    pub batch_bytes: &'a [u8],
    pub error: &'a str,
}

pub struct HeartbeatInput<'a> {
    pub sandbox_id: &'a str,
    pub daemon_boot_id: Option<&'a str>,
    pub reachable: bool,
    pub spool_pending: Option<u64>,
    pub spool_dropped_total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardTraceDecision {
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub degraded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceVerifyReport {
    pub ok: bool,
    pub trace_id: Option<String>,
    pub scope: String,
    pub checked_entries: usize,
    pub first_error: Option<TraceVerifyFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TraceVerifyFailure {
    pub audit_seq: i64,
    pub kind: String,
    pub message: String,
}

impl TraceVerifyReport {
    pub(super) fn failed(
        trace_id: Option<&str>,
        checked_entries: usize,
        audit_seq: i64,
        kind: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            ok: false,
            trace_id: trace_id.map(ToOwned::to_owned),
            scope: verify_scope(trace_id).to_owned(),
            checked_entries,
            first_error: Some(TraceVerifyFailure {
                audit_seq,
                kind: kind.into(),
                message: message.into(),
            }),
        }
    }
}

pub(super) fn verify_scope(trace_id: Option<&str>) -> &'static str {
    if trace_id.is_some() {
        "global_chain_with_trace_projection"
    } else {
        "global_chain"
    }
}
