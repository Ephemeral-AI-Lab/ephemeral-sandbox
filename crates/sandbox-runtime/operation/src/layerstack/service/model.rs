use std::path::PathBuf;

use crate::layerstack::LayerStackServiceError;

/// A classified, windowed read of one path from the active snapshot. Non-regular
/// and non-UTF-8 paths are reported instead of read; only the selected output is
/// capped (`OutputTooLarge`), never the whole source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestReadWindow {
    Absent,
    Text {
        content: String,
        start_line: u64,
        num_lines: usize,
        total_lines: u64,
        bytes_read: usize,
        total_bytes: u64,
        next_offset: Option<u64>,
        truncated: bool,
    },
    Directory,
    Symlink,
    NotUtf8,
    OutputTooLarge {
        limit: usize,
    },
}

/// Outcome of a committed `amend_path`. Blame is recorded inside `amend_path`
/// from the layerstack origin, so the caller never sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmendOutcome {
    pub existed_before: bool,
    pub bytes_written: usize,
}

/// Failure of `amend_path`: either the caller's transform rejected the read, or
/// the layerstack read/commit failed.
#[derive(Debug)]
pub enum AmendError<E> {
    Transform(E),
    LayerStack(LayerStackServiceError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStackRevision {
    pub manifest_version: i64,
    pub root_hash: String,
    pub layer_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishChangesRequest {
    pub expected_base: LayerStackRevision,
    pub base_manifest: sandbox_runtime_layerstack::Manifest,
    pub protected_drops: Vec<sandbox_runtime_layerstack::LayerProtectedDrop>,
    pub changes: Vec<sandbox_runtime_layerstack::LayerChange>,
    /// Opaque owner string this publish stamps onto its `Command` lines
    /// (`workspace_session:<id>` when a workspace was mounted, else
    /// `operation:<id>`). Not passed to layerstack — mapped to audit events
    /// above it, after the layer commits.
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishChangesResult {
    pub revision: LayerStackRevision,
    pub manifest: sandbox_runtime_layerstack::Manifest,
    pub layer_paths: Vec<PathBuf>,
    pub route_summary: sandbox_runtime_layerstack::PublishRouteSummary,
    pub no_op: bool,
}
