use std::path::{Path, PathBuf};

use crate::commit::{ChangesetResult, CommitOptions};
use crate::model::{LayerChange, Manifest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub manifest_version: i64,
    pub root_hash: String,
    pub layer_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeasedSnapshot {
    pub lease_id: String,
    pub manifest_version: i64,
    pub root_hash: String,
    pub layer_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub struct PublishChangesRequest<'a> {
    pub root: &'a Path,
    pub snapshot_manifest_version: i64,
    pub snapshot_layer_paths: &'a [PathBuf],
    pub changes: &'a [LayerChange],
    pub options: CommitOptions,
}

pub type PublishChangesResult = ChangesetResult;

#[derive(Debug, Clone, Copy)]
pub struct CompactSnapshotLayersRequest<'a> {
    pub root: &'a Path,
    pub snapshot_manifest_version: i64,
    pub snapshot_layer_paths: &'a [PathBuf],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactSnapshotLayersResult {
    pub manifest: Manifest,
    pub layer_paths: Vec<PathBuf>,
    pub before_layer_count: usize,
    pub after_layer_count: usize,
}
