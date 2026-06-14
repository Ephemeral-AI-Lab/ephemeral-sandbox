use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ::protocol::catalog::{SANDBOX_TRACE_EXPORT, SANDBOX_TRACE_EXPORT_ACK};
use base64::Engine as _;
use serde_json::{json, Value};

use crate::daemon_wire::{
    decode_trace_sidecar_checked, encode_request_with_forward_metadata, ProtocolClient,
};
use crate::service::registry::{cached_or_resolve_endpoint, SandboxRecord, SandboxRegistry};
use crate::service::HostConfig;
use crate::trace_store::{
    HeartbeatInput, PendingSidecarInput, TraceEventInput, TraceIngestFailedInput, TraceStore,
};
use trace::{RequestId, TraceId};

#[derive(Clone, Default)]
pub(crate) struct TraceExportDrainer {
    state: Arc<Mutex<TraceDrainState>>,
}

#[derive(Default)]
struct TraceDrainState {
    in_flight: HashSet<String>,
    pending: HashSet<String>,
}

/// Background drain cadence. Idle sandboxes with background activity can
/// overflow the daemon's bounded spool before the next foreground forward; the
/// timer flushes traces independent of foreground traffic.
const TRACE_DRAIN_INTERVAL: Duration = Duration::from_secs(15);

impl TraceExportDrainer {
    /// Timer thread that schedules a drain for every known sandbox on a fixed
    /// cadence, reusing the per-sandbox single-flight slot so it coalesces with
    /// foreground-triggered drains.
    pub(crate) fn spawn_periodic(
        &self,
        registry: Arc<SandboxRegistry>,
        config: HostConfig,
        trace_store: Arc<TraceStore>,
    ) {
        let drainer = self.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(TRACE_DRAIN_INTERVAL);
            for record in registry.list() {
                drainer.schedule(record, &config, Arc::clone(&trace_store));
            }
        });
    }

    /// Single-flight per sandbox, never on the forwarding caller's thread.
    /// Each pass drains oldest-first until the spool is empty; schedules
    /// arriving mid-drain coalesce into one follow-up pass.
    pub(crate) fn schedule(
        &self,
        record: Arc<SandboxRecord>,
        config: &HostConfig,
        trace_store: Arc<TraceStore>,
    ) {
        let sandbox_id = record.sandbox_id.clone();
        if !self.reserve(sandbox_id.clone()) {
            return;
        }

        let target = TraceDrainTarget {
            sandbox_id,
            forward_token: record.forward_token.clone(),
            record,
            request_timeout: config.request_timeout,
        };
        let drainer = self.clone();
        std::thread::spawn(move || loop {
            let _ = drain_trace_export_to_empty(&target, &trace_store);
            if !drainer.finish(&target.sandbox_id) {
                break;
            }
        });
    }

    /// Returns true when a coalesced schedule arrived mid-drain and this
    /// thread should run another pass while still holding the slot.
    fn finish(&self, sandbox_id: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let rerun = state.pending.remove(sandbox_id);
        if !rerun {
            state.in_flight.remove(sandbox_id);
        }
        rerun
    }

    fn reserve(&self, sandbox_id: String) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.in_flight.insert(sandbox_id.clone()) {
            true
        } else {
            state.pending.insert(sandbox_id);
            false
        }
    }
}

pub(super) struct TraceDrainTarget {
    pub(super) sandbox_id: String,
    pub(super) forward_token: String,
    pub(super) record: Arc<SandboxRecord>,
    pub(super) request_timeout: Duration,
}

const TRACE_DRAIN_MAX_RECORDS: u64 = 64;

fn drain_trace_export_to_empty(
    target: &TraceDrainTarget,
    trace_store: &TraceStore,
) -> anyhow::Result<()> {
    loop {
        if drain_trace_export_once(target, trace_store)? < TRACE_DRAIN_MAX_RECORDS {
            return Ok(());
        }
    }
}

pub(super) fn drain_trace_export_once(
    target: &TraceDrainTarget,
    trace_store: &TraceStore,
) -> anyhow::Result<u64> {
    let endpoint = match cached_or_resolve_endpoint(&target.record) {
        Ok(endpoint) => endpoint,
        Err(err) => {
            let message = err.to_string();
            let _ = trace_store.record_heartbeat(HeartbeatInput {
                sandbox_id: &target.sandbox_id,
                daemon_boot_id: None,
                reachable: false,
                spool_pending: None,
                spool_dropped_total: None,
            });
            record_drain_event(
                trace_store,
                target,
                "missing_endpoint",
                json!({"error_kind": "missing_endpoint", "message": message}),
            );
            return Ok(0);
        }
    };
    let client = ProtocolClient::new(endpoint, None, target.request_timeout);
    let args = json!({"max_records": TRACE_DRAIN_MAX_RECORDS});
    let mut line = encode_request_with_forward_metadata(
        SANDBOX_TRACE_EXPORT,
        "trace-export-drain",
        &args,
        Some(&target.forward_token),
    );
    line.push(b'\n');
    let response = match client.request_raw_observed(&line) {
        Ok(response) => response,
        Err(err) => {
            let _ = trace_store.record_heartbeat(HeartbeatInput {
                sandbox_id: &target.sandbox_id,
                daemon_boot_id: None,
                reachable: false,
                spool_pending: None,
                spool_dropped_total: None,
            });
            record_drain_event(
                trace_store,
                target,
                "daemon_request_failed",
                json!({"endpoint": endpoint.to_string(), "error_kind": "daemon_request_failed", "message": err.to_string()}),
            );
            return Ok(0);
        }
    };
    match decode_trace_sidecar_checked(&response.value) {
        Ok(Some(sidecar)) => {
            ingest_drained_batch(trace_store, target, &sidecar, "sidecar_ingest_failed");
        }
        Ok(None) => {}
        Err(err) => record_drain_event(
            trace_store,
            target,
            "sidecar_decode_failed",
            json!({"error_kind": err.kind()}),
        ),
    }
    if let Some(encoded) = response
        .value
        .get("trace_batch_base64")
        .and_then(Value::as_str)
    {
        match base64::engine::general_purpose::STANDARD.decode(encoded) {
            Ok(batch) => {
                let export = trace_export_identity(&response.value);
                if let Some(export) = export.as_ref() {
                    if trace::sha256_hex(&batch) != export.batch_sha256 {
                        record_drain_event(
                            trace_store,
                            target,
                            "trace_batch_digest_mismatch",
                            json!({
                                "export_id": export.export_id,
                                "expected_sha256": export.batch_sha256,
                                "actual_sha256": trace::sha256_hex(&batch),
                            }),
                        );
                        return Ok(0);
                    }
                }
                let ingested = match export.as_ref() {
                    Some(export) => ingest_trace_export_batch(
                        trace_store,
                        target,
                        export,
                        &batch,
                        "trace_batch_ingest_failed",
                    ),
                    None => ingest_drained_batch(
                        trace_store,
                        target,
                        &batch,
                        "trace_batch_ingest_failed",
                    ),
                };
                if ingested {
                    if let Some(export) = export.as_ref() {
                        if !ack_trace_export(trace_store, target, endpoint, export) {
                            return Ok(0);
                        }
                    }
                } else {
                    return Ok(0);
                }
            }
            Err(err) => record_drain_event(
                trace_store,
                target,
                "trace_batch_decode_failed",
                json!({"error_kind": "invalid_base64", "message": err.to_string()}),
            ),
        }
    }
    let record_count = response
        .value
        .get("record_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let spool_pending_after = response
        .value
        .get("spool_pending_after")
        .and_then(Value::as_u64)
        .or_else(|| (record_count < TRACE_DRAIN_MAX_RECORDS).then_some(0));
    let dropped_traces = response.value.get("dropped_traces").and_then(Value::as_u64);
    let _ = trace_store.record_heartbeat(HeartbeatInput {
        sandbox_id: &target.sandbox_id,
        daemon_boot_id: None,
        reachable: true,
        spool_pending: spool_pending_after,
        spool_dropped_total: dropped_traces,
    });
    Ok(record_count)
}

#[derive(Debug)]
struct TraceExportIdentity {
    export_id: String,
    batch_sha256: String,
    record_count: u64,
}

fn trace_export_identity(value: &Value) -> Option<TraceExportIdentity> {
    Some(TraceExportIdentity {
        export_id: value.get("export_id")?.as_str()?.to_owned(),
        batch_sha256: value.get("batch_sha256")?.as_str()?.to_owned(),
        record_count: value.get("record_count")?.as_u64()?,
    })
}

fn ingest_trace_export_batch(
    trace_store: &TraceStore,
    target: &TraceDrainTarget,
    export: &TraceExportIdentity,
    batch: &[u8],
    event: &str,
) -> bool {
    match trace_store.ingest_trace_export_batch_once(
        &target.sandbox_id,
        &export.export_id,
        &export.batch_sha256,
        export.record_count,
        batch,
    ) {
        Ok(()) => true,
        Err(err) => {
            record_ingest_failure(trace_store, target, batch, event, err.to_string());
            false
        }
    }
}

fn ack_trace_export(
    trace_store: &TraceStore,
    target: &TraceDrainTarget,
    endpoint: std::net::SocketAddr,
    export: &TraceExportIdentity,
) -> bool {
    let client = ProtocolClient::new(endpoint, None, target.request_timeout);
    let args = json!({
        "export_id": export.export_id,
        "batch_sha256": export.batch_sha256,
        "record_count": export.record_count,
    });
    let mut line = encode_request_with_forward_metadata(
        SANDBOX_TRACE_EXPORT_ACK,
        "trace-export-ack",
        &args,
        Some(&target.forward_token),
    );
    line.push(b'\n');
    let response = match client.request_raw_observed(&line) {
        Ok(response) => response.value,
        Err(err) => {
            let message = err.to_string();
            let _ = trace_store.record_trace_export_ack_failure(&export.export_id, &message);
            record_drain_event(
                trace_store,
                target,
                "trace_export_ack_failed",
                json!({"export_id": export.export_id, "error_kind": "daemon_request_failed", "message": message}),
            );
            return false;
        }
    };
    if response.get("acked").and_then(Value::as_bool) == Some(true) {
        let _ = trace_store.record_trace_export_ack_success(&export.export_id);
        return true;
    }
    let message = response.to_string();
    let _ = trace_store.record_trace_export_ack_failure(&export.export_id, &message);
    record_drain_event(
        trace_store,
        target,
        "trace_export_ack_rejected",
        json!({"export_id": export.export_id, "response": response}),
    );
    false
}

fn ingest_drained_batch(
    trace_store: &TraceStore,
    target: &TraceDrainTarget,
    batch: &[u8],
    event: &str,
) -> bool {
    if let Err(err) = trace_store.ingest_trace_batch(&target.sandbox_id, batch) {
        record_ingest_failure(trace_store, target, batch, event, err.to_string());
        return false;
    }
    true
}

fn record_ingest_failure(
    trace_store: &TraceStore,
    target: &TraceDrainTarget,
    batch: &[u8],
    event: &str,
    message: String,
) {
    let identity = drained_batch_identity(batch).ok();
    let pending_recorded = identity.as_ref().is_some_and(|(trace_id, request_id)| {
        trace_store
            .record_pending_sidecar(PendingSidecarInput {
                sandbox_id: &target.sandbox_id,
                trace_id,
                request_id,
                batch_bytes: batch,
                error: &message,
            })
            .is_ok()
    });
    if let Some((trace_id, request_id)) = identity.as_ref() {
        let _ = trace_store.record_trace_ingest_failed(TraceIngestFailedInput {
            sandbox_id: &target.sandbox_id,
            trace_id,
            request_id,
            error_kind: "trace_batch_ingest_failed",
            message: &message,
        });
    }
    record_drain_event_with_identity(
        trace_store,
        target,
        identity.as_ref(),
        event,
        json!({
            "error_kind": "trace_batch_ingest_failed",
            "message": message,
            "pending_recorded": pending_recorded,
            "batch_len": batch.len(),
        }),
    );
}

fn drained_batch_identity(batch: &[u8]) -> Result<(TraceId, RequestId), trace::DecodeTraceError> {
    let batch = trace::decode_trace_batch(batch)?;
    let Some(record) = batch.records.first() else {
        return Ok((
            TraceId::parse("trace-export-empty").unwrap_or_default(),
            RequestId::parse("trace-export-drain").unwrap_or_default(),
        ));
    };
    let request_id = record.request_id.clone().unwrap_or_else(|| {
        RequestId::parse(format!("trace-export-drain-{}", record.trace_id.as_str()))
            .unwrap_or_default()
    });
    Ok((record.trace_id.clone(), request_id))
}

fn record_drain_event(
    trace_store: &TraceStore,
    target: &TraceDrainTarget,
    event: &str,
    details: Value,
) {
    record_drain_event_with_identity(trace_store, target, None, event, details);
}

fn record_drain_event_with_identity(
    trace_store: &TraceStore,
    target: &TraceDrainTarget,
    identity: Option<&(TraceId, RequestId)>,
    event: &str,
    details: Value,
) {
    let trace_id = TraceId::new();
    let request_id = RequestId::parse("trace-export-drain").unwrap_or_default();
    let (trace_id, request_id) = identity
        .map(|(trace_id, request_id)| (trace_id, Some(request_id)))
        .unwrap_or((&trace_id, Some(&request_id)));
    let _ = trace_store.append_trace_event_or_loss(TraceEventInput {
        sandbox_id: &target.sandbox_id,
        trace_id,
        request_id,
        span_id: None,
        module: "host.trace_drain",
        event,
        details,
    });
}

#[cfg(test)]
mod tests {
    use super::TraceExportDrainer;

    #[test]
    fn trace_drainer_coalesces_pending_schedule_without_second_lock() {
        let drainer = TraceExportDrainer::default();

        assert!(drainer.reserve("sb-deadlock-proof".to_owned()));
        assert!(
            !drainer.reserve("sb-deadlock-proof".to_owned()),
            "second schedule should coalesce while a drain is in flight"
        );
        assert!(
            drainer.finish("sb-deadlock-proof"),
            "coalesced schedule should request one follow-up drain"
        );
        assert!(
            !drainer.finish("sb-deadlock-proof"),
            "second finish should clear the in-flight slot"
        );
        assert!(
            drainer.reserve("sb-deadlock-proof".to_owned()),
            "slot should be reusable after the follow-up drain finishes"
        );
    }
}
