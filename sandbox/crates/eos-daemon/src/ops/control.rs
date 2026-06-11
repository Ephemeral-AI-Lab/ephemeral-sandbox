//! Runtime, heartbeat, cancel, and in-flight daemon ops.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eos_layerstack::{require_workspace_binding, LayerStack};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::request_args::{require_string, trimmed_string};
use crate::DispatchContext;

/// `api.runtime.ready` — binary readiness plus the three plane probes
/// (`control_plane` / `data_plane` / `mutation_gate`). Requires `layer_stack_root`.
pub(crate) fn op_runtime_ready(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let total_start = Instant::now();
    let root = require_string(args, "layer_stack_root")?;
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
    Ok(json!({
        "success": true,
        "ready": probes.iter().all(|probe| probe.get("status") == Some(&Value::String("ok".to_owned()))),
        "probes": probes,
        "daemon_pid": std::process::id(),
        "uptime_s": crate::dispatcher::daemon_uptime_s(),
        "timings": Value::Object(timings),
    }))
}

/// `api.v1.cancel` — cancel one in-flight invocation id.
// Op handlers share the fallible dispatcher ABI even when this handler encodes
// invalid/missing ids as ordinary JSON response fields.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub(crate) fn op_cancel(args: &Value, context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let invocation_id = trimmed_string(args, "invocation_id");
    let (cancelled, cleanup_done) =
        context
            .invocation_registry()
            .map_or((false, true), |registry| {
                let cancelled = registry.cancel(&invocation_id);
                let cleanup_done =
                    !cancelled || registry.wait_for_cleanup(&invocation_id, Duration::from_secs(5));
                (cancelled, cleanup_done)
            });
    Ok(json!({
        "success": true,
        "invocation_id": invocation_id,
        "cancelled": cancelled,
        "already_done": !cancelled,
        "cleanup_done": cleanup_done,
    }))
}

/// `api.v1.heartbeat` — touch `last_seen` for the given invocation ids.
// Op handlers share the fallible dispatcher ABI even when this handler encodes
// invalid/missing ids as ordinary JSON response fields.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub(crate) fn op_heartbeat(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let invocation_ids: Vec<String> = args
        .get("invocation_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let touched = context
        .invocation_registry()
        .map_or(0, |registry| registry.heartbeat(&invocation_ids));
    Ok(json!({"success": true, "touched": touched}))
}

/// `api.v1.inflight_count` — count background daemon invocations for one agent.
// Op handlers share the fallible ABI even when this handler encodes missing
// registry state as a zero count.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub(crate) fn op_inflight_count(
    args: &Value,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let caller_id = trimmed_string(args, "caller_id");
    let count = context
        .invocation_registry()
        .map_or(0, |registry| registry.count_by_caller(&caller_id));
    Ok(json!({"success": true, "caller_id": caller_id, "count": count}))
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
        DaemonError::InvalidEnvelope(_) => "ValueError",
        _ => "RuntimeError",
    }
}
