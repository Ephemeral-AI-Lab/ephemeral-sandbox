//! Shared overlay ns-runner helpers and daemon adapters.

mod convert;

use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use eos_overlay::overlay_writable_root;
use eos_protocol::LayerChange;
use eos_runner::{RunRequest, RunResult};
use eos_workspace_runtime::ephemeral::{
    EphemeralDirAllocator, EphemeralRunDirs, EphemeralSnapshot, EphemeralWorkspaceError,
    InvocationId, PathChange, PublishOutcome, WorkspacePublisherPort, WorkspaceRoot,
};

use crate::adapters::occ::{
    apply_occ_changeset, base_hashes_for_snapshot, insert_occ_route_timings, manifest_version_u64,
    occ_route_metrics,
};
use crate::error::DaemonError;
use crate::invocation_registry::InFlightRegistry;

pub(crate) use convert::{
    changeset_from_publish_outcome, ephemeral_daemon_error, path_changes_to_wire,
};
use convert::{manifest_from_snapshot, overlay_daemon_error, publish_outcome_from_changeset};

pub(crate) use eos_workspace_runtime::ephemeral::RunDirCleanup;

/// Wrap any displayable error as an `EphemeralWorkspaceError::PublishFailed`.
pub(crate) fn publish_failed(error: impl std::fmt::Display) -> EphemeralWorkspaceError {
    EphemeralWorkspaceError::PublishFailed {
        reason: error.to_string(),
    }
}

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
        let route_metrics = occ_route_metrics(self.root, changes).map_err(publish_failed)?;
        let route_s = route_start.elapsed().as_secs_f64();
        let snapshot_manifest = manifest_from_snapshot(self.root, snapshot)?;
        let base_hashes = base_hashes_for_snapshot(self.root, &snapshot_manifest, changes)
            .map_err(publish_failed)?;
        let occ_start = std::time::Instant::now();
        let mut changeset = apply_occ_changeset(
            self.root,
            Some(manifest_version_u64(snapshot.manifest_version).map_err(publish_failed)?),
            changes,
            &base_hashes,
        )
        .map_err(publish_failed)?;
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

fn ephemeral_dir_allocator() -> Result<EphemeralDirAllocator, DaemonError> {
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
