//! Overlay publish conversion helpers.

use std::path::Path;

use eos_occ::{ChangesetResult, FileResult, OccStatus};
use eos_protocol::{LayerPath, LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};
use eos_workspace_runtime::ephemeral::{
    SnapshotLease, EphemeralWorkspaceError, PathChange, PathChangeKind, PublishOutcome,
    PublishStatus,
};
use serde_json::{json, Value};

use crate::error::DaemonError;

pub(super) fn manifest_from_snapshot(
    root: &Path,
    snapshot: &SnapshotLease,
) -> Result<Manifest, EphemeralWorkspaceError> {
    let layers = snapshot
        .layer_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let relative = match path.strip_prefix(root) {
                Ok(relative) => relative,
                Err(_) if path.is_relative() => path,
                Err(_) => {
                    return Err(EphemeralWorkspaceError::PublishFailed {
                        reason: format!(
                            "snapshot layer path {} is outside {}",
                            path.display(),
                            root.display()
                        ),
                    });
                }
            };
            Ok(LayerRef {
                layer_id: format!("snapshot-{index}"),
                path: relative.to_string_lossy().into_owned(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Manifest::new(snapshot.manifest_version, layers, MANIFEST_SCHEMA_VERSION)
        .map_err(super::publish_failed)
}

pub(super) fn overlay_daemon_error(context: &str, err: &eos_overlay::OverlayError) -> DaemonError {
    DaemonError::OverlayPipeline(format!("{context}: {err}"))
}

pub(crate) fn ephemeral_daemon_error(error: EphemeralWorkspaceError) -> DaemonError {
    DaemonError::OverlayPipeline(error.to_string())
}

pub(crate) fn path_changes_to_wire(path_changes: &[PathChange]) -> Vec<(String, String)> {
    path_changes
        .iter()
        .map(|change| {
            (
                change.path.clone(),
                path_change_kind_wire(change.kind).to_owned(),
            )
        })
        .collect()
}

fn path_change_kind_wire(kind: PathChangeKind) -> &'static str {
    match kind {
        PathChangeKind::Write => "write",
        PathChangeKind::Delete => "delete",
        PathChangeKind::Symlink => "symlink",
        PathChangeKind::OpaqueDir => "opaque_dir",
    }
}

pub(crate) fn changeset_from_publish_outcome(
    outcome: &PublishOutcome,
) -> Result<ChangesetResult, DaemonError> {
    let raw = outcome
        .raw
        .as_object()
        .ok_or_else(|| DaemonError::OverlayPipeline("publish outcome raw must be object".into()))?;
    let files = raw
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| DaemonError::OverlayPipeline("publish outcome missing files".into()))?
        .iter()
        .map(file_result_from_value)
        .collect::<Result<Vec<_>, _>>()?;
    let timings = raw
        .get("timings")
        .and_then(Value::as_object)
        .map(|timings| {
            timings
                .iter()
                .filter_map(|(key, value)| value.as_f64().map(|value| (key.clone(), value)))
                .collect()
        })
        .unwrap_or_default();
    Ok(ChangesetResult {
        files,
        published_manifest_version: raw
            .get("published_manifest_version")
            .and_then(Value::as_u64),
        timings,
    })
}

pub(super) fn publish_outcome_from_changeset(result: &ChangesetResult) -> PublishOutcome {
    let published_paths = result.published_paths();
    let conflicts = result
        .files
        .iter()
        .filter(|file| !file.status.is_success())
        .map(|file| file.path.as_str().to_owned())
        .collect::<Vec<_>>();
    let status = if !conflicts.is_empty() {
        if result.files.iter().any(|file| {
            matches!(
                file.status,
                OccStatus::AbortedVersion | OccStatus::AbortedOverlap
            )
        }) {
            PublishStatus::Conflict
        } else {
            PublishStatus::Rejected
        }
    } else if published_paths.is_empty() {
        PublishStatus::NoChanges
    } else {
        PublishStatus::Published
    };
    PublishOutcome {
        status,
        manifest_version: result.published_manifest_version,
        published_paths,
        conflicts,
        timings: result
            .timings
            .iter()
            .map(|(key, value)| (key.clone(), json!(value)))
            .collect(),
        raw: json!({
            "files": result.files.iter().map(file_result_to_value).collect::<Vec<_>>(),
            "published_manifest_version": result.published_manifest_version,
            "timings": result.timings,
        }),
    }
}

fn file_result_to_value(file: &FileResult) -> Value {
    json!({
        "path": file.path.as_str(),
        "status": file.status,
        "message": file.message,
    })
}

fn file_result_from_value(value: &Value) -> Result<FileResult, DaemonError> {
    let object = value
        .as_object()
        .ok_or_else(|| DaemonError::OverlayPipeline("publish file result must be object".into()))?;
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| DaemonError::OverlayPipeline("publish file result missing path".into()))?;
    let status_value = object
        .get("status")
        .cloned()
        .ok_or_else(|| DaemonError::OverlayPipeline("publish file result missing status".into()))?;
    let status = serde_json::from_value::<OccStatus>(status_value)
        .map_err(|error| DaemonError::InvalidEnvelope(error.to_string()))?;
    Ok(FileResult {
        path: LayerPath::parse(path).map_err(eos_layerstack::LayerStackError::from)?,
        status,
        message: object
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

#[cfg(test)]
#[path = "../../../tests/overlay_convert/mod.rs"]
mod tests;
