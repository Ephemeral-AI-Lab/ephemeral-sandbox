//! Daemon-owned adapters for neutral workspace-mode traits.

#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

#[cfg(target_os = "linux")]
use eos_layerstack::MergedView;
use eos_layerstack::{require_workspace_binding, LayerStack, WorkspaceBinding};
use eos_occ::ChangesetResult;
use eos_protocol::{LayerChange, LayerPath};
#[cfg(target_os = "linux")]
use eos_protocol::{LayerRef, Manifest};
use eos_workspace_api::{
    ChangedPathKinds, ResolvedWorkspacePath, WorkspaceApiError, WorkspaceConflict, WorkspaceMode,
    WorkspaceMutationKind, WorkspaceMutationOutcome, WorkspaceMutationRequest,
    WorkspaceMutationSink, WorkspaceReadBytes, WorkspaceReadView, WorkspaceTimings,
};
use serde_json::json;

#[cfg(target_os = "linux")]
use crate::response_timings::usize_to_f64_saturating;
use crate::response_timings::{resource_timings, timing_map};
use crate::services::occ::{apply_occ_changeset, hash_current, manifest_version_u64};

fn api_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("daemon_workspace_error", error.to_string())
}

fn resolve_layer_path(
    binding: &WorkspaceBinding,
    request_path: &str,
) -> Result<ResolvedWorkspacePath, WorkspaceApiError> {
    let path = if request_path.starts_with('/') {
        binding
            .layer_path_from_absolute(request_path)
            .map_err(api_error)?
    } else {
        binding
            .layer_path_from_relative(request_path)
            .map_err(api_error)?
    };
    Ok(ResolvedWorkspacePath::new(path))
}

/// LayerStack/OCC-backed direct file ports for `ephemeral_workspace`.
#[derive(Debug, Clone)]
pub(crate) struct EphemeralFilePorts {
    root: PathBuf,
}

impl EphemeralFilePorts {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl WorkspaceReadView for EphemeralFilePorts {
    fn resolve_path(&self, request_path: &str) -> Result<ResolvedWorkspacePath, WorkspaceApiError> {
        let binding = require_workspace_binding(&self.root).map_err(api_error)?;
        resolve_layer_path(&binding, request_path)
    }

    fn read_bytes(
        &self,
        path: &ResolvedWorkspacePath,
    ) -> Result<WorkspaceReadBytes, WorkspaceApiError> {
        let stack = LayerStack::open(self.root.clone()).map_err(api_error)?;
        let read_start = Instant::now();
        let (bytes, exists) = stack.read_bytes(&path.path).map_err(api_error)?;
        let manifest = stack.read_active_manifest().map_err(api_error)?;
        let mut timings = resource_timings(&manifest, 0);
        timings.insert(
            "api.read.layer_stack_read_s".to_owned(),
            json!(read_start.elapsed().as_secs_f64()),
        );
        Ok(WorkspaceReadBytes {
            bytes,
            exists,
            manifest_version: Some(manifest.version),
            timings: timing_map(timings),
        })
    }
}

impl WorkspaceMutationSink for EphemeralFilePorts {
    fn commit_or_record(
        &self,
        request: WorkspaceMutationRequest,
    ) -> Result<WorkspaceMutationOutcome, WorkspaceApiError> {
        let path = LayerPath::parse(&request.path.path)
            .map_err(eos_layerstack::LayerStackError::from)
            .map_err(api_error)?;
        let base_hash = hash_current(request.base.bytes.as_deref(), request.base.exists);
        let snapshot_version = request
            .base
            .manifest_version
            .map(manifest_version_u64)
            .transpose()
            .map_err(api_error)?;
        let occ_start = Instant::now();
        let result = apply_occ_changeset(
            &self.root,
            snapshot_version,
            &[LayerChange::Write {
                path: path.clone(),
                content: request.content,
            }],
            &[(path, base_hash)],
        )
        .map_err(api_error)?;
        let manifest = LayerStack::open(self.root.clone())
            .and_then(|stack| stack.read_active_manifest())
            .map_err(api_error)?;
        let mut timings = resource_timings(&manifest, result.published_file_count());
        timings.insert(
            format!("api.{}.occ_apply_s", request.kind.verb()),
            json!(occ_start.elapsed().as_secs_f64()),
        );
        Ok(changeset_outcome(
            request.kind,
            &result,
            timing_map(timings),
        ))
    }
}

fn changeset_outcome(
    kind: WorkspaceMutationKind,
    result: &ChangesetResult,
    mut timings: WorkspaceTimings,
) -> WorkspaceMutationOutcome {
    for (key, value) in &result.timings {
        timings.insert(key.clone(), json!(value));
    }
    let changed_paths = result.published_paths();
    let changed_path_kinds = changed_paths
        .iter()
        .map(|path| (path.clone(), "write".to_owned()))
        .collect::<ChangedPathKinds>();
    let conflict = result.first_conflict();
    WorkspaceMutationOutcome {
        mode: WorkspaceMode::Ephemeral,
        success: result.success(),
        published: result.success(),
        status: conflict
            .as_ref()
            .map_or("committed", |file| file.status.wire_str())
            .to_owned(),
        conflict: conflict.as_ref().map(|file| {
            let reason = file.status.wire_str();
            WorkspaceConflict::path(reason, file.path.as_str(), file.conflict_message(reason))
        }),
        conflict_reason: conflict
            .as_ref()
            .map(|file| file.conflict_message(file.status.wire_str()).to_owned()),
        changed_paths,
        changed_path_kinds,
        mutation_source: match kind {
            WorkspaceMutationKind::Write => "api_write",
            WorkspaceMutationKind::Edit => "api_edit",
        }
        .to_owned(),
        error: None,
        timings,
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub(crate) struct IsolatedFilePorts {
    handle: crate::services::isolated_workspace::CommandHandle,
    started_at: Instant,
}

#[cfg(target_os = "linux")]
impl IsolatedFilePorts {
    pub(crate) fn new(handle: crate::services::isolated_workspace::CommandHandle) -> Self {
        Self {
            handle,
            started_at: Instant::now(),
        }
    }

    pub(crate) fn record_read_file(&self) {
        record_isolated_tool_call(&self.handle, "read_file", "ok", &[], self.started_at);
    }
}

#[cfg(target_os = "linux")]
impl WorkspaceReadView for IsolatedFilePorts {
    fn resolve_path(&self, request_path: &str) -> Result<ResolvedWorkspacePath, WorkspaceApiError> {
        let binding = WorkspaceBinding {
            workspace_root: self.handle.workspace_root.to_string_lossy().into_owned(),
            layer_stack_root: self.handle.layer_stack_root.to_string_lossy().into_owned(),
            active_manifest_version: self.handle.manifest_version,
            active_root_hash: self.handle.manifest_root_hash.clone(),
            base_manifest_version: self.handle.manifest_version,
            base_root_hash: self.handle.manifest_root_hash.clone(),
        };
        resolve_layer_path(&binding, request_path)
    }

    fn read_bytes(
        &self,
        path: &ResolvedWorkspacePath,
    ) -> Result<WorkspaceReadBytes, WorkspaceApiError> {
        let read_start = Instant::now();
        let layer_path = LayerPath::parse(&path.path)
            .map_err(eos_layerstack::LayerStackError::from)
            .map_err(api_error)?;
        let (bytes, exists) = read_isolated_current(&self.handle, &layer_path)?;
        let mut timings = isolated_timings(0);
        timings.insert(
            "api.read.layer_stack_read_s".to_owned(),
            json!(read_start.elapsed().as_secs_f64()),
        );
        Ok(WorkspaceReadBytes {
            bytes,
            exists,
            manifest_version: Some(self.handle.manifest_version),
            timings,
        })
    }
}

#[cfg(target_os = "linux")]
impl WorkspaceMutationSink for IsolatedFilePorts {
    fn commit_or_record(
        &self,
        request: WorkspaceMutationRequest,
    ) -> Result<WorkspaceMutationOutcome, WorkspaceApiError> {
        let layer_path = LayerPath::parse(&request.path.path)
            .map_err(eos_layerstack::LayerStackError::from)
            .map_err(api_error)?;
        write_isolated_upper(&self.handle, &layer_path, &request.content)?;
        let changed_paths = vec![layer_path.as_str().to_owned()];
        record_isolated_tool_call(
            &self.handle,
            match request.kind {
                WorkspaceMutationKind::Write => "write_file",
                WorkspaceMutationKind::Edit => "edit_file",
            },
            "committed",
            &changed_paths,
            self.started_at,
        );
        Ok(WorkspaceMutationOutcome {
            mode: WorkspaceMode::Isolated,
            success: true,
            published: false,
            status: "committed".to_owned(),
            conflict: None,
            conflict_reason: None,
            changed_path_kinds: BTreeMap::from([(
                layer_path.as_str().to_owned(),
                "write".to_owned(),
            )]),
            changed_paths,
            mutation_source: "isolated_workspace".to_owned(),
            error: None,
            timings: isolated_timings(1),
        })
    }
}

#[cfg(target_os = "linux")]
fn isolated_upper_path(
    handle: &crate::services::isolated_workspace::CommandHandle,
    layer_path: &LayerPath,
) -> PathBuf {
    handle.upperdir.join(layer_path.as_str())
}

#[cfg(target_os = "linux")]
fn read_isolated_current(
    handle: &crate::services::isolated_workspace::CommandHandle,
    layer_path: &LayerPath,
) -> Result<(Option<Vec<u8>>, bool), WorkspaceApiError> {
    let upper_path = isolated_upper_path(handle, layer_path);
    match std::fs::symlink_metadata(&upper_path) {
        Ok(metadata) if metadata.is_file() => {
            return Ok((Some(std::fs::read(upper_path).map_err(api_error)?), true));
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Ok((
                Some(
                    std::fs::read_link(upper_path)
                        .map_err(api_error)?
                        .to_string_lossy()
                        .as_bytes()
                        .to_vec(),
                ),
                true,
            ));
        }
        Ok(_) => return Ok((None, false)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(api_error(error)),
    }
    MergedView::new(handle.layer_stack_root.clone())
        .read_bytes(layer_path.as_str(), &isolated_manifest(handle))
        .map_err(api_error)
}

#[cfg(target_os = "linux")]
fn write_isolated_upper(
    handle: &crate::services::isolated_workspace::CommandHandle,
    layer_path: &LayerPath,
    content: &[u8],
) -> Result<(), WorkspaceApiError> {
    let path = isolated_upper_path(handle, layer_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(api_error)?;
    }
    std::fs::write(path, content).map_err(api_error)
}

#[cfg(target_os = "linux")]
fn isolated_manifest(handle: &crate::services::isolated_workspace::CommandHandle) -> Manifest {
    Manifest {
        version: handle.manifest_version,
        schema_version: 1,
        layers: handle
            .layer_paths
            .iter()
            .enumerate()
            .map(|(index, path)| LayerRef {
                layer_id: format!("isolated-{index}"),
                path: isolated_manifest_layer_path(handle, path),
            })
            .collect(),
    }
}

#[cfg(target_os = "linux")]
fn isolated_manifest_layer_path(
    handle: &crate::services::isolated_workspace::CommandHandle,
    path: &Path,
) -> String {
    path.strip_prefix(&handle.layer_stack_root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(target_os = "linux")]
fn isolated_timings(changed_path_count: usize) -> WorkspaceTimings {
    BTreeMap::from([(
        "resource.command_exec.changed_path_count".to_owned(),
        json!(usize_to_f64_saturating(changed_path_count)),
    )])
}

#[cfg(target_os = "linux")]
fn record_isolated_tool_call(
    handle: &crate::services::isolated_workspace::CommandHandle,
    tool_name: &str,
    status: &str,
    changed_paths: &[String],
    total_start: Instant,
) {
    let duration_s = total_start.elapsed().as_secs_f64();
    crate::services::isolated_workspace::record_tool_call(
        &handle.agent_id,
        json!({
            "tool_name": tool_name,
            "workspace_handle_id": handle.workspace_handle_id,
            "argv0": tool_name,
            "exit_code": 0,
            "status": status,
            "changed_paths": changed_paths,
            "published": false,
            "duration_s": duration_s,
            "total_ms": duration_s * 1000.0,
            "phases_ms": {
                "exec": duration_s * 1000.0,
            },
        }),
    );
}
