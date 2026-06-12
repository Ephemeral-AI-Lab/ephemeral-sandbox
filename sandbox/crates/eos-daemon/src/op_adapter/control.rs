//! Runtime, heartbeat, cancel, and in-flight daemon ops.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine as _;
use eos_layerstack::{require_workspace_binding, LayerStack};
use eos_operation::control::contract::{
    CallerCountInput, CancelInvocationInput, CancelInvocationOutput, HeartbeatInput,
    HeartbeatOutput, InflightCountOutput, RuntimeReadyInput, RuntimeReadyOutput, TraceExportInput,
    TraceExportOutput,
};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::DispatchContext;

use super::to_wire_value;

/// `sandbox.runtime.ready` — binary readiness plus the three plane probes
/// (`control_plane` / `data_plane` / `mutation_gate`). Requires `layer_stack_root`.
pub(crate) fn op_runtime_ready(
    input: RuntimeReadyInput,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let total_start = Instant::now();
    let root = input.layer_stack_root.to_string_lossy().into_owned();
    let mut timings = serde_json::Map::new();
    let probes = vec![
        run_probe("control_plane", || probe_control_plane(&root), &mut timings),
        run_probe(
            "data_plane",
            || {
                Ok(json!({
                    "handlers_services_ready": true,
                    "shell_services_ready": true,
                    "workspace_mount_mode": "private_namespace",
                }))
            },
            &mut timings,
        ),
        run_probe(
            "mutation_gate",
            || {
                Ok(json!({
                    "backend_ready": true,
                    "backend_fields": ["layer_stack", "occ_service", "occ_client", "gitignore", "layer_stack_manager"],
                    "occ_client_class": "OccClient",
                }))
            },
            &mut timings,
        ),
    ];
    timings.insert(
        "runtime.ready.total_s".to_owned(),
        json!(total_start.elapsed().as_secs_f64()),
    );
    Ok(to_wire_value(RuntimeReadyOutput {
        success: true,
        ready: probes
            .iter()
            .all(|probe| probe.get("status") == Some(&Value::String("ok".to_owned()))),
        probes,
        daemon_pid: std::process::id(),
        uptime_s: crate::dispatcher::daemon_uptime_s(),
        timings: Value::Object(timings),
    }))
}

/// `sandbox.call.cancel` — cancel one in-flight invocation id.
pub(crate) fn op_cancel(input: CancelInvocationInput, context: DispatchContext<'_>) -> Value {
    let invocation_id = input.invocation_id.to_string();
    let (cancelled, cleanup_done) =
        context
            .invocation_registry()
            .map_or((false, true), |registry| {
                let cancelled = registry.cancel(&invocation_id);
                let cleanup_done =
                    !cancelled || registry.wait_for_cleanup(&invocation_id, Duration::from_secs(5));
                (cancelled, cleanup_done)
            });
    to_wire_value(CancelInvocationOutput {
        success: true,
        invocation_id,
        cancelled,
        already_done: !cancelled,
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

/// `sandbox.trace.export` — drain daemon background trace roots for host ingest.
pub(crate) fn op_trace_export(input: TraceExportInput) -> Value {
    let (records, dropped_traces) = crate::trace::drain_background_records(input.max_records);
    let record_count = records.len();
    let trace_batch_base64 = (!records.is_empty()).then(|| {
        base64::engine::general_purpose::STANDARD.encode(eos_trace::encode_trace_batch(
            &eos_trace::TraceBatch {
                records,
                dropped_traces,
            },
        ))
    });
    to_wire_value(TraceExportOutput {
        success: true,
        record_count,
        dropped_traces,
        trace_batch_base64,
    })
}

fn run_probe<F>(name: &str, probe: F, timings: &mut serde_json::Map<String, Value>) -> Value
where
    F: FnOnce() -> Result<Value, DaemonError>,
{
    let start = Instant::now();
    let (status, details) = match probe() {
        Ok(details) => ("ok", details),
        Err(err) => (
            "down",
            json!({"error_type": error_type(&err), "error": err.to_string()}),
        ),
    };
    timings.insert(
        format!("runtime.ready.{name}_s"),
        json!(start.elapsed().as_secs_f64()),
    );
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
        DaemonError::LayerStack(eos_layerstack::LayerStackError::WorkspaceBinding(_)) => {
            "WorkspaceBindingError"
        }
        DaemonError::LayerStack(eos_layerstack::LayerStackError::Manifest(_)) => {
            "ManifestConflictError"
        }
        DaemonError::Io(_) => "OSError",
        DaemonError::InvalidRequest(_) => "ValueError",
        _ => "RuntimeError",
    }
}
