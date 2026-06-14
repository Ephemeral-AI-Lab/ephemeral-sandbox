//! Runtime, heartbeat, cancel, and in-flight daemon ops.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use layerstack::{require_workspace_binding, LayerStack};
use operation::control::contract::{
    CallerCountInput, CancelInvocationInput, CancelInvocationOutput, HeartbeatInput,
    HeartbeatOutput, InflightCountOutput, RuntimeReadyInput, RuntimeReadyOutput,
    TraceExportAckInput, TraceExportAckOutput, TraceExportInput, TraceExportOutput,
};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::runtime::invocation_registry::InvocationCancelResult;
use crate::DispatchContext;

use super::to_wire_value;

/// `sandbox.runtime.ready` — binary readiness plus the three plane probes
/// (`control_plane` / `data_plane` / `mutation_gate`). Requires `layer_stack_root`.
pub(crate) fn op_runtime_ready(
    input: RuntimeReadyInput,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let root = input.layer_stack_root.to_string_lossy().into_owned();
    let probes = vec![
        run_probe("control_plane", || probe_control_plane(&root)),
        run_probe("data_plane", || {
            Ok(json!({
                "handlers_services_ready": true,
                "shell_services_ready": true,
                "workspace_mount_mode": "private_namespace",
            }))
        }),
        run_probe("mutation_gate", || {
            Ok(json!({
                "backend_ready": true,
                "backend_fields": ["layer_stack", "occ_service", "occ_client", "gitignore", "layer_stack_manager"],
                "occ_client_class": "OccClient",
            }))
        }),
    ];
    Ok(to_wire_value(RuntimeReadyOutput {
        success: true,
        ready: probes
            .iter()
            .all(|probe| probe.get("status") == Some(&Value::String("ok".to_owned()))),
        probes,
        daemon_pid: std::process::id(),
        uptime_s: crate::dispatcher::daemon_uptime_s(),
    }))
}

/// `sandbox.call.cancel` — cancel one in-flight invocation id.
pub(crate) fn op_cancel(input: CancelInvocationInput, context: DispatchContext<'_>) -> Value {
    let invocation_id = input.invocation_id.to_string();
    let (cancelled, already_done, cleanup_done) =
        context
            .invocation_registry()
            .map_or((false, true, true), |registry| {
                match registry.cancel_invocation(&invocation_id) {
                    InvocationCancelResult::Cancelled => (
                        true,
                        false,
                        registry.wait_for_cleanup(&invocation_id, Duration::from_secs(5)),
                    ),
                    InvocationCancelResult::AlreadyDone => (false, true, true),
                    InvocationCancelResult::RunningUncancellable => (false, false, false),
                }
            });
    to_wire_value(CancelInvocationOutput {
        success: true,
        invocation_id,
        cancelled,
        already_done,
        cleanup_done,
    })
}

/// `sandbox.call.heartbeat` — touch `last_seen` for the given invocation ids.
pub(crate) fn op_heartbeat(input: HeartbeatInput, context: DispatchContext<'_>) -> Value {
    let invocation_ids: Vec<String> = input
        .invocation_ids
        .iter()
        .map(ToString::to_string)
        .collect();
    let touched = context
        .invocation_registry()
        .map_or(0, |registry| registry.heartbeat(&invocation_ids));
    to_wire_value(HeartbeatOutput {
        success: true,
        touched,
    })
}

/// `sandbox.call.count` — count background daemon invocations for one agent.
pub(crate) fn op_inflight_count(input: CallerCountInput, context: DispatchContext<'_>) -> Value {
    let caller_id = input.caller.to_string();
    let count = context
        .invocation_registry()
        .map_or(0, |registry| registry.count_by_caller(&caller_id));
    to_wire_value(InflightCountOutput {
        success: true,
        caller_id,
        count,
    })
}

/// `sandbox.trace.export` — lease daemon background trace roots for host ingest.
pub(crate) fn op_trace_export(input: TraceExportInput) -> Value {
    to_wire_value(trace_export_output(crate::trace::lease_background_records(
        input.max_records,
    )))
}

pub(crate) fn op_trace_export_ack(input: TraceExportAckInput) -> Value {
    let export_id = input.export_id;
    let acked = trace::ExportId::parse(export_id.clone()).is_ok_and(|export_id| {
        crate::trace::ack_background_export(&export_id, &input.batch_sha256, input.record_count)
    });
    to_wire_value(TraceExportAckOutput {
        success: true,
        export_id,
        acked,
    })
}

fn trace_export_output(batch: trace::TraceExportBatch) -> TraceExportOutput {
    let trace_batch_base64 = batch
        .trace_batch_bytes
        .as_ref()
        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes));
    TraceExportOutput {
        success: true,
        record_count: batch.record_count,
        spool_pending_after: batch.spool_pending_after,
        dropped_traces: batch.dropped_traces,
        export_id: batch.export_id.map(|export_id| export_id.to_string()),
        batch_sha256: batch.batch_sha256,
        trace_batch_base64,
    }
}

fn run_probe<F>(name: &str, probe: F) -> Value
where
    F: FnOnce() -> Result<Value, DaemonError>,
{
    let (status, details) = match probe() {
        Ok(details) => ("ok", details),
        Err(err) => (
            "down",
            json!({"error_type": error_type(&err), "error": err.to_string()}),
        ),
    };
    json!({"name": name, "status": status, "details": details})
}

fn probe_control_plane(root: &str) -> Result<Value, DaemonError> {
    let binding = require_workspace_binding(root)?;
    let stack = LayerStack::open(PathBuf::from(root))?;
    let manifest = stack.read_active_manifest()?;
    Ok(json!({
        "workspace_root": binding.workspace_root,
        "manifest_version": manifest.version,
        "manifest_depth": manifest.depth(),
        "base_root_hash": binding.base_root_hash,
    }))
}

const fn error_type(err: &DaemonError) -> &'static str {
    match err {
        DaemonError::LayerStack(layerstack::LayerStackError::WorkspaceBinding(_)) => {
            "WorkspaceBindingError"
        }
        DaemonError::LayerStack(layerstack::LayerStackError::Manifest(_)) => {
            "ManifestConflictError"
        }
        DaemonError::Io(_) => "OSError",
        DaemonError::InvalidRequest(_) => "ValueError",
        _ => "RuntimeError",
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use trace::decode_trace_batch;

    use super::*;

    #[test]
    fn trace_export_encodes_loss_only_batches() {
        let encoded_batch = trace::encode_trace_batch(&trace::TraceBatch {
            records: Vec::new(),
            dropped_traces: 7,
            daemon_boot_id: Some("boot-loss-only".to_owned()),
        });
        let output = trace_export_output(trace::TraceExportBatch {
            export_id: Some(trace::ExportId::parse("export-loss-only").expect("export id")),
            record_count: 0,
            spool_pending_after: 0,
            dropped_traces: 7,
            batch_sha256: Some(trace::sha256_hex(&encoded_batch)),
            trace_batch_bytes: Some(encoded_batch),
        });
        let encoded = output
            .trace_batch_base64
            .as_deref()
            .expect("loss-only export must include batch bytes");
        let batch = decode_trace_batch(
            &base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .expect("base64"),
        )
        .expect("trace batch decodes");

        assert!(batch.records.is_empty());
        assert_eq!(batch.dropped_traces, 7);
        assert!(batch.daemon_boot_id.is_some());
    }
}
