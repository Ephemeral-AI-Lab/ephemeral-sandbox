use std::collections::HashMap;

use sandbox_observability_telemetry::{LayerStackBytes, Reader, SampleDelta};
use sandbox_runtime_layerstack::service::StackObservation;
use sandbox_runtime_layerstack::{LayerDeltaDescription, LayerDeltaEntryKind};
use serde_json::{json, Value};

use crate::ports::{
    NamespaceExecutionSnapshot, ObservabilitySnapshot, QueryContext, WorkspaceSnapshot,
};

const LATEST_SAMPLE_WINDOW_MS: i64 = i64::MAX / 4;

pub(crate) fn snapshot_value(context: &QueryContext, snapshot: ObservabilitySnapshot) -> Value {
    let ObservabilitySnapshot {
        workspaces,
        active_namespace_executions,
        partial_errors,
    } = snapshot;
    let availability = if partial_errors.is_empty() {
        "available"
    } else {
        "partial"
    };
    json!({
        "sandbox_id": context.sandbox_id,
        "lifecycle_state": "ready",
        "availability": availability,
        "sampled_at_unix_ms": unix_ms(),
        "errors": partial_errors,
        "daemon": {
            "daemon_pid": context.daemon_pid,
            "runtime_dir": context.runtime_dir,
        },
        "resources": resource_bundle(&context.reader, "sandbox"),
        "workspaces": workspaces
            .iter()
            .map(|workspace| {
                workspace_value(
                    &context.reader,
                    workspace,
                    &active_namespace_executions,
                )
            })
            .collect::<Vec<_>>(),
    })
}

pub(crate) fn cgroup_series(reader: &Reader, scope: &str, window_ms: u64) -> Value {
    Value::Array(
        reader
            .samples(scope, window_to_i64(window_ms))
            .iter()
            .map(sample_delta_value)
            .collect(),
    )
}

pub(crate) fn stack_trend(reader: &Reader, window_ms: u64) -> Vec<Value> {
    reader
        .samples("stack", window_to_i64(window_ms))
        .iter()
        .map(|delta| {
            let mut value = delta.metrics.clone();
            value.insert("ts".to_owned(), json!(delta.ts));
            Value::Object(value)
        })
        .collect()
}

pub(crate) fn latest_upper_bytes(reader: &Reader, scope: &str) -> Option<u64> {
    latest_sample(reader, scope)?
        .metrics
        .get("disk_bytes")?
        .as_u64()
}

pub(crate) fn layerstack_value(observation: &StackObservation, bytes: &LayerStackBytes) -> Value {
    let bytes_by_id: HashMap<&str, (Option<u64>, Option<u64>)> = bytes
        .layers
        .iter()
        .map(|layer| {
            (
                layer.layer_id.as_str(),
                (layer.bytes, layer.allocated_bytes),
            )
        })
        .collect();
    let layers = observation
        .layers
        .iter()
        .enumerate()
        .map(|(index, status)| {
            let layer_id = status.layer.layer_id.as_str();
            let booked_by = observation.layers[..index]
                .iter()
                .filter(|above| above.leased_by_workspaces > 0)
                .map(|above| above.layer.layer_id.as_str())
                .collect::<Vec<_>>();
            json!({
                "layer_id": layer_id,
                "bytes": bytes_by_id.get(layer_id).and_then(|value| value.0),
                "allocated_bytes": bytes_by_id.get(layer_id).and_then(|value| value.1),
                "leased_by_workspaces": status.leased_by_workspaces,
                "booked_by": booked_by,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "view": "layerstack",
        "manifest_version": observation.manifest_version,
        "root_hash": observation.root_hash,
        "active_lease_count": observation.active_lease_count,
        "total_bytes": bytes.total_bytes,
        "total_allocated_bytes": bytes.total_allocated_bytes,
        "storage_logical_bytes": bytes.storage_logical_bytes,
        "storage_allocated_bytes": bytes.storage_allocated_bytes,
        "staging_entry_count": bytes.staging_entry_count,
        "layers": layers,
    })
}

pub(crate) fn stack_summary_value(
    observation: &StackObservation,
    bytes: &LayerStackBytes,
) -> Value {
    json!({
        "layer_count": observation.layers.len(),
        "layers_bytes": bytes.total_bytes,
        "layers_allocated_bytes": bytes.total_allocated_bytes,
        "storage_allocated_bytes": bytes.storage_allocated_bytes,
        "staging_entry_count": bytes.staging_entry_count,
        "active_leases": observation.active_lease_count,
    })
}

pub(crate) fn workspace_layerstack_value(
    workspaces: &[WorkspaceSnapshot],
    target: &str,
    upper_bytes: Option<u64>,
) -> Option<Value> {
    let workspace = workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == target)?;
    let mounts = workspace
        .layer_ids
        .iter()
        .map(|layer_id| {
            let shared_with = workspaces
                .iter()
                .filter(|other| {
                    other.workspace_id != target && other.layer_ids.iter().any(|id| id == layer_id)
                })
                .map(|other| other.workspace_id.clone())
                .collect::<Vec<_>>();
            json!({ "layer_id": layer_id, "shared_with": shared_with })
        })
        .collect::<Vec<_>>();
    Some(json!({
        "view": "layerstack",
        "workspace": target,
        "mounts": mounts,
        "upper_bytes": upper_bytes,
    }))
}

pub(crate) fn layer_delta_value(layer_id: &str, delta: &LayerDeltaDescription) -> Value {
    let entries = delta
        .entries
        .iter()
        .map(|entry| {
            json!({
                "path": entry.path.as_str(),
                "kind": layer_delta_kind(entry.kind),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "view": "layerstack",
        "layer_id": layer_id,
        "entries": entries,
        "truncated": delta.truncated,
    })
}

fn resource_bundle(reader: &Reader, scope: &str) -> Value {
    let latest = latest_sample(reader, scope)
        .map(|delta| sample_delta_value(&delta))
        .unwrap_or(Value::Null);
    json!({ "latest": latest, "history": [] })
}

fn latest_sample(reader: &Reader, scope: &str) -> Option<SampleDelta> {
    reader.samples(scope, LATEST_SAMPLE_WINDOW_MS).pop()
}

fn sample_delta_value(delta: &SampleDelta) -> Value {
    json!({
        "ts": delta.ts,
        "sample_delta_ms": delta.sample_delta_ms,
        "metrics": delta.metrics,
        "deltas": delta.deltas,
    })
}

fn workspace_value(
    reader: &Reader,
    workspace: &WorkspaceSnapshot,
    executions: &[NamespaceExecutionSnapshot],
) -> Value {
    json!({
        "workspace_id": workspace.workspace_id,
        "lifecycle_state": "active",
        "network_profile": workspace.network_profile,
        "finalize_policy": workspace.finalize_policy,
        "layers": {
            "base_root_hash": workspace.base_root_hash,
            "layer_count": workspace.layer_count,
        },
        "namespace_fd_count": workspace.namespace_fd_count,
        "resources": resource_bundle(reader, &workspace.workspace_id),
        "active_namespace_executions": executions
            .iter()
            .filter(|execution| execution.workspace_session_id == workspace.workspace_id)
            .map(namespace_execution_value)
            .collect::<Vec<_>>(),
    })
}

fn namespace_execution_value(execution: &NamespaceExecutionSnapshot) -> Value {
    json!({
        "namespace_execution_id": execution.namespace_execution_id,
        "operation": execution.operation_name,
        "lifecycle_state": "running",
    })
}

fn layer_delta_kind(kind: LayerDeltaEntryKind) -> &'static str {
    match kind {
        LayerDeltaEntryKind::File => "file",
        LayerDeltaEntryKind::Symlink => "symlink",
        LayerDeltaEntryKind::Directory => "directory",
        LayerDeltaEntryKind::Delete => "delete",
        LayerDeltaEntryKind::OpaqueDir => "opaque_dir",
    }
}

fn window_to_i64(window_ms: u64) -> i64 {
    i64::try_from(window_ms).unwrap_or(i64::MAX)
}

fn unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}
