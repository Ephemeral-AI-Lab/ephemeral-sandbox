use serde_json::{json, Value};

use crate::daemon_wire::{decode_trace_sidecar_checked, strip_trace_sidecar};
use crate::trace_store::{PendingSidecarInput, TraceIngestFailedInput};

use super::audit::record_event;
use super::response_meta::{mark_response_trace_degraded, mark_response_trace_ingested};
use super::ForwardAttempt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SidecarIngest {
    pub(crate) present: bool,
    pub(crate) ingested: bool,
    pub(crate) degraded: bool,
}

pub(crate) fn ingest_and_strip_sidecar(
    attempt: &ForwardAttempt<'_>,
    response: &mut Value,
) -> SidecarIngest {
    let batch = match decode_trace_sidecar_checked(response) {
        Ok(Some(batch)) => batch,
        Ok(None) => {
            return SidecarIngest {
                present: false,
                ingested: false,
                degraded: false,
            };
        }
        Err(err) => {
            let message = format!("sidecar decode failed: {}", err.kind());
            record_event(
                attempt,
                "host.transport",
                "sidecar_decode_failed",
                json!({"error_kind": err.kind()}),
            );
            record_trace_ingest_failed(attempt, err.kind(), &message);
            mark_response_trace_degraded(attempt, response, err.kind(), &message);
            strip_trace_sidecar(response);
            return SidecarIngest {
                present: true,
                ingested: false,
                degraded: true,
            };
        }
    };
    match attempt
        .trace_store
        .ingest_trace_batch(&attempt.record.sandbox_id, &batch)
    {
        Ok(()) => {
            strip_trace_sidecar(response);
            mark_response_trace_ingested(attempt, response);
            SidecarIngest {
                present: true,
                ingested: true,
                degraded: false,
            }
        }
        Err(err) => {
            let message = err.to_string();
            let pending_recorded = attempt
                .trace_store
                .record_pending_sidecar(PendingSidecarInput {
                    sandbox_id: &attempt.record.sandbox_id,
                    trace_id: &attempt.trace_id,
                    request_id: &attempt.request_id,
                    batch_bytes: &batch,
                    error: &message,
                })
                .is_ok();
            record_event(
                attempt,
                "host.transport",
                "sidecar_ingest_failed",
                json!({
                    "error_kind": "trace_batch_ingest_failed",
                    "message": message.clone(),
                    "pending_recorded": pending_recorded,
                }),
            );
            record_trace_ingest_failed(attempt, "trace_batch_ingest_failed", &message);
            mark_response_trace_degraded(attempt, response, "trace_batch_ingest_failed", &message);
            strip_trace_sidecar(response);
            SidecarIngest {
                present: true,
                ingested: false,
                degraded: true,
            }
        }
    }
}

fn record_trace_ingest_failed(attempt: &ForwardAttempt<'_>, error_kind: &str, message: &str) {
    let _ = attempt
        .trace_store
        .record_trace_ingest_failed(TraceIngestFailedInput {
            sandbox_id: &attempt.record.sandbox_id,
            trace_id: &attempt.trace_id,
            request_id: &attempt.request_id,
            error_kind,
            message,
        });
}
