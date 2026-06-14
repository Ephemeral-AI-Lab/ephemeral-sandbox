use serde_json::{json, Value};

use crate::daemon_wire::ClientError;
use crate::trace_store::{ResponseMissingInput, ResponsePersistedInput, TraceEventInput};

use super::response_meta::{mark_response_trace_degraded, refresh_response_trace_receipt};
use super::ForwardAttempt;

pub(super) fn record_event(
    attempt: &ForwardAttempt<'_>,
    module: &str,
    event: &str,
    details: Value,
) {
    let _ = attempt
        .trace_store
        .append_trace_event_or_loss(TraceEventInput {
            sandbox_id: &attempt.record.sandbox_id,
            trace_id: &attempt.trace_id,
            request_id: Some(&attempt.request_id),
            span_id: None,
            module,
            event,
            details,
        });
}

pub(super) fn persist_response_or_mark_degraded(
    attempt: &ForwardAttempt<'_>,
    response: &mut Value,
    raw_response_bytes: &[u8],
    host_rtt_ms: u64,
) -> Result<(), ClientError> {
    match attempt
        .trace_store
        .record_response_persisted(ResponsePersistedInput {
            sandbox_id: &attempt.record.sandbox_id,
            trace_id: &attempt.trace_id,
            request_id: &attempt.request_id,
            response,
            raw_response_bytes,
            host_rtt_ms,
        }) {
        Ok(()) => {
            refresh_response_trace_receipt(attempt, response);
            Ok(())
        }
        Err(err) if attempt.mutates_state => Err(ClientError::Io(std::io::Error::other(format!(
            "terminal trace persistence failed after daemon response: {err}"
        )))),
        Err(err) => {
            let message = format!("terminal trace persistence failed after daemon response: {err}");
            mark_response_trace_degraded(
                attempt,
                response,
                "trace_response_persist_failed",
                &message,
            );
            Ok(())
        }
    }
}

pub(super) fn record_missing(
    attempt: &ForwardAttempt<'_>,
    status: &str,
    error_kind: &str,
    message: &str,
) {
    record_event(
        attempt,
        "host.protocol",
        "response_missing",
        json!({"status": status, "error_kind": error_kind, "message": message}),
    );
    let _ = attempt
        .trace_store
        .record_response_missing(ResponseMissingInput {
            sandbox_id: &attempt.record.sandbox_id,
            trace_id: &attempt.trace_id,
            request_id: &attempt.request_id,
            status,
            error_kind,
            message,
        });
}
