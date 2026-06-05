//! Shared overlay ns-runner helpers and daemon adapters.

use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use eos_ephemeral_workspace::{
    EphemeralDirAllocator, EphemeralRunDirs, EphemeralSnapshot, EphemeralWorkspaceError,
    InvocationId, PathChange, PublishOutcome, PublishStatus, WorkspacePublisherPort, WorkspaceRoot,
};
use eos_occ::{ChangesetResult, FileResult, OccStatus};
use eos_overlay::overlay_writable_root;
use eos_protocol::{LayerChange, LayerPath, LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};
use eos_runner::{RunRequest, RunResult};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::invocation_registry::InFlightRegistry;
use crate::services::occ::{
    apply_occ_changeset, base_hashes_for_snapshot, insert_occ_route_timings, manifest_version_u64,
    occ_route_metrics,
};

pub(crate) use eos_ephemeral_workspace::RunDirCleanup;

pub(crate) struct DaemonPublisherPort<'a> {
    root: &'a Path,
}

impl<'a> DaemonPublisherPort<'a> {
    pub(crate) const fn new(root: &'a Path) -> Self {
        Self { root }
    }
}

impl WorkspacePublisherPort for DaemonPublisherPort<'_> {
    fn publish_upperdir_changes(
        &self,
        _root: &WorkspaceRoot,
        snapshot: &EphemeralSnapshot,
        changes: &[LayerChange],
        _path_kinds: &[PathChange],
    ) -> Result<PublishOutcome, EphemeralWorkspaceError> {
        let route_start = std::time::Instant::now();
        let route_metrics = occ_route_metrics(self.root, changes).map_err(|error| {
            EphemeralWorkspaceError::PublishFailed {
                reason: error.to_string(),
            }
        })?;
        let route_s = route_start.elapsed().as_secs_f64();
        let snapshot_manifest = manifest_from_snapshot(self.root, snapshot)?;
        let base_hashes = base_hashes_for_snapshot(self.root, &snapshot_manifest, changes)
            .map_err(|error| EphemeralWorkspaceError::PublishFailed {
                reason: error.to_string(),
            })?;
        let occ_start = std::time::Instant::now();
        let mut changeset = apply_occ_changeset(
            self.root,
            Some(
                manifest_version_u64(snapshot.manifest_version).map_err(|error| {
                    EphemeralWorkspaceError::PublishFailed {
                        reason: error.to_string(),
                    }
                })?,
            ),
            changes,
            &base_hashes,
        )
        .map_err(|error| EphemeralWorkspaceError::PublishFailed {
            reason: error.to_string(),
        })?;
        let occ_s = occ_start.elapsed().as_secs_f64();
        let mut timing_values = serde_json::Map::new();
        insert_occ_route_timings(&mut timing_values, route_metrics, route_s, occ_s);
        for (key, value) in timing_values {
            if let Some(value) = value.as_f64() {
                changeset.timings.entry(key).or_insert(value);
            }
        }
        Ok(publish_outcome_from_changeset(&changeset))
    }
}

fn manifest_from_snapshot(
    root: &Path,
    snapshot: &EphemeralSnapshot,
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
    Manifest::new(snapshot.manifest_version, layers, MANIFEST_SCHEMA_VERSION).map_err(|error| {
        EphemeralWorkspaceError::PublishFailed {
            reason: error.to_string(),
        }
    })
}

pub(crate) fn ephemeral_dir_allocator() -> Result<EphemeralDirAllocator, DaemonError> {
    Ok(EphemeralDirAllocator::new(
        overlay_writable_root()
            .map_err(|err| overlay_daemon_error("overlay writable root", &err))?
            .join("runtime"),
    ))
}

pub(crate) fn overlay_run_dirs(
    kind: &str,
    invocation_id: &str,
) -> Result<EphemeralRunDirs, DaemonError> {
    ephemeral_dir_allocator()?
        .allocate(kind, &InvocationId(invocation_id.to_owned()))
        .map_err(ephemeral_daemon_error)
}

pub(crate) fn run_ns_runner_child(
    request: &RunRequest,
    invocation_registry: Option<&InFlightRegistry>,
) -> Result<RunResult, DaemonError> {
    let payload =
        serde_json::to_vec(request).map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("ns-runner")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    command.process_group(0);
    let mut child = command.spawn()?;
    if let Some(registry) = invocation_registry {
        if let Ok(pgid) = i32::try_from(child.id()) {
            registry.register_process_group(&request.tool_call.invocation_id, pgid);
        }
    }
    child
        .stdin
        .as_mut()
        .ok_or_else(|| DaemonError::OverlayPipeline("ns-runner stdin unavailable".to_owned()))?
        .write_all(&payload)?;
    let output = child.wait_with_output()?;
    if let Some(registry) = invocation_registry {
        registry.clear_process_group(&request.tool_call.invocation_id);
    }
    if !output.status.success() {
        return Err(DaemonError::OverlayPipeline(format!(
            "ns-runner exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    serde_json::from_slice::<RunResult>(&output.stdout)
        .map_err(|err| DaemonError::OverlayPipeline(format!("invalid ns-runner output: {err}")))
}

pub(crate) fn overlay_daemon_error(context: &str, err: &eos_overlay::OverlayError) -> DaemonError {
    DaemonError::OverlayPipeline(format!("{context}: {err}"))
}

pub(crate) fn ephemeral_daemon_error(error: EphemeralWorkspaceError) -> DaemonError {
    match error {
        EphemeralWorkspaceError::InvalidArgument(message) => DaemonError::InvalidEnvelope(message),
        EphemeralWorkspaceError::Io { source, .. } => DaemonError::Io(source),
        other => DaemonError::OverlayPipeline(other.to_string()),
    }
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

pub(crate) fn path_change_kind_wire(kind: eos_ephemeral_workspace::PathChangeKind) -> &'static str {
    match kind {
        eos_ephemeral_workspace::PathChangeKind::Write => "write",
        eos_ephemeral_workspace::PathChangeKind::Delete => "delete",
        eos_ephemeral_workspace::PathChangeKind::Symlink => "symlink",
        eos_ephemeral_workspace::PathChangeKind::OpaqueDir => "opaque_dir",
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

fn publish_outcome_from_changeset(result: &ChangesetResult) -> PublishOutcome {
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
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn manifest_from_snapshot_converts_absolute_layer_paths_to_relative() {
        let root = PathBuf::from("/stack");
        let manifest = manifest_from_snapshot(
            &root,
            &EphemeralSnapshot {
                lease_id: "lease-1".to_owned(),
                manifest_version: 7,
                manifest_root_hash: "hash".to_owned(),
                layer_paths: vec![root.join("layers/a"), root.join("layers/b")],
            },
        )
        .expect("snapshot manifest");

        assert_eq!(manifest.version, 7);
        assert_eq!(manifest.layers[0].path, "layers/a");
        assert_eq!(manifest.layers[1].path, "layers/b");
    }

    #[test]
    fn manifest_from_snapshot_rejects_absolute_layer_paths_outside_root() {
        let error = manifest_from_snapshot(
            &PathBuf::from("/stack"),
            &EphemeralSnapshot {
                lease_id: "lease-1".to_owned(),
                manifest_version: 7,
                manifest_root_hash: "hash".to_owned(),
                layer_paths: vec![PathBuf::from("/other/layers/a")],
            },
        )
        .expect_err("outside-root path should fail");

        assert!(
            error.to_string().contains("outside /stack"),
            "unexpected error: {error}"
        );
    }
}
