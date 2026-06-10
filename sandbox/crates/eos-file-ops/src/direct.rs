//! The direct fast path: no overlay anywhere.
//!
//! Reads resolve against the latest merged state of the layer stack; applies
//! commit through the per-root single writer, gated by the content hash the
//! read observed (so a concurrent commit surfaces as a typed conflict, never a
//! silent clobber).

use std::path::PathBuf;
use std::time::Instant;

use eos_layerstack::{
    hash_current, require_workspace_binding, service, ChangesetResult, LayerChange, LayerPath,
    LayerStack, WorkspaceBinding,
};
use serde_json::json;

use crate::{
    ChangedPathKinds, FileBackend, FileOpsError, Mutation, MutationKind, MutationOutcome,
    ReadBytes, ResolvedWorkspacePath, WorkspaceConflict, WorkspaceMode, WorkspaceTimings,
};

/// Latest-state file backend for one layer-stack root.
#[derive(Debug, Clone)]
pub struct DirectBackend {
    root: PathBuf,
}

impl DirectBackend {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl FileBackend for DirectBackend {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Ephemeral
    }

    fn mutation_source(&self, kind: MutationKind) -> &'static str {
        match kind {
            MutationKind::Write => "api_write",
            MutationKind::Edit => "api_edit",
        }
    }

    fn resolve_path(&self, request_path: &str) -> Result<ResolvedWorkspacePath, FileOpsError> {
        let binding = require_workspace_binding(&self.root).map_err(api_error)?;
        resolve_layer_path(&binding, request_path)
    }

    fn read_bytes(&self, path: &ResolvedWorkspacePath) -> Result<ReadBytes, FileOpsError> {
        let stack = LayerStack::open(self.root.clone()).map_err(api_error)?;
        let read_start = Instant::now();
        let (bytes, exists) = stack.read_bytes(&path.path).map_err(api_error)?;
        let manifest = stack.read_active_manifest().map_err(api_error)?;
        let mut timings = WorkspaceTimings::new();
        timings.insert(
            "resource.layer_stack.manifest_depth".to_owned(),
            json!(usize_to_f64_saturating(manifest.depth())),
        );
        timings.insert(
            "api.read.layer_stack_read_s".to_owned(),
            json!(read_start.elapsed().as_secs_f64()),
        );
        Ok(ReadBytes {
            bytes,
            exists,
            manifest_version: Some(manifest.version),
            timings,
        })
    }

    fn apply(&self, mutation: Mutation) -> Result<MutationOutcome, FileOpsError> {
        let path = parse_layer_path(&mutation.path.path)?;
        let base_hash = hash_current(mutation.base.bytes.as_deref(), mutation.base.exists);
        let snapshot_version = mutation
            .base
            .manifest_version
            .map(service::manifest_version_u64)
            .transpose()
            .map_err(api_error)?;
        let occ_start = Instant::now();
        let result = service::commit_direct(
            &self.root,
            snapshot_version,
            &[LayerChange::Write {
                path: path.clone(),
                content: mutation.content,
            }],
            &[(path, base_hash)],
        )
        .map_err(api_error)?;
        let manifest = LayerStack::open(self.root.clone())
            .and_then(|stack| stack.read_active_manifest())
            .map_err(api_error)?;
        let mut timings = WorkspaceTimings::new();
        timings.insert(
            "resource.layer_stack.manifest_depth".to_owned(),
            json!(usize_to_f64_saturating(manifest.depth())),
        );
        timings.insert(
            format!("api.{}.occ_apply_s", mutation.kind.verb()),
            json!(occ_start.elapsed().as_secs_f64()),
        );
        Ok(changeset_outcome(
            self.mutation_source(mutation.kind),
            &result,
            timings,
        ))
    }
}

fn changeset_outcome(
    mutation_source: &str,
    result: &ChangesetResult,
    mut timings: WorkspaceTimings,
) -> MutationOutcome {
    for (key, value) in &result.timings {
        timings.insert(key.clone(), json!(value));
    }
    let changed_paths = result.published_paths();
    let changed_path_kinds = changed_paths
        .iter()
        .map(|path| (path.clone(), "write".to_owned()))
        .collect::<ChangedPathKinds>();
    let conflict = result.first_conflict();
    MutationOutcome {
        mode: WorkspaceMode::Ephemeral,
        success: result.success(),
        published: result.success(),
        status: conflict
            .map_or("committed", |file| file.status.wire_str())
            .to_owned(),
        conflict: conflict.map(|file| {
            let reason = file.status.wire_str();
            WorkspaceConflict::path(reason, file.path.as_str(), file.conflict_message(reason))
        }),
        conflict_reason: conflict
            .map(|file| file.conflict_message(file.status.wire_str()).to_owned()),
        changed_paths,
        changed_path_kinds,
        mutation_source: mutation_source.to_owned(),
        timings,
    }
}

pub(crate) fn resolve_layer_path(
    binding: &WorkspaceBinding,
    request_path: &str,
) -> Result<ResolvedWorkspacePath, FileOpsError> {
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

pub(crate) fn parse_layer_path(raw: &str) -> Result<LayerPath, FileOpsError> {
    LayerPath::parse(raw)
        .map_err(eos_layerstack::LayerStackError::from)
        .map_err(api_error)
}

pub(crate) fn api_error(error: impl std::fmt::Display) -> FileOpsError {
    FileOpsError::new("daemon_workspace_error", error.to_string())
}

pub(crate) fn usize_to_f64_saturating(value: usize) -> f64 {
    u32::try_from(value).map_or_else(|_| f64::from(u32::MAX), f64::from)
}
