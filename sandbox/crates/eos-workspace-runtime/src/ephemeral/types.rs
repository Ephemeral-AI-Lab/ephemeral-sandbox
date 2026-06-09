use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use eos_protocol::{CallerId, InvocationId};

/// Root of the LayerStack workspace whose snapshot is used by the operation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorkspaceRoot(pub PathBuf);

/// Snapshot lease material needed to mount a fresh overlay.
///
/// Shared value object owned by `eos-workspace-contract`.
pub use eos_workspace_contract::SnapshotLease;

/// Fresh writable paths allocated for one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EphemeralRunDirs {
    pub run_dir: PathBuf,
    pub upperdir: PathBuf,
    pub workdir: PathBuf,
    pub output_path: PathBuf,
    pub final_path: PathBuf,
    pub request_path: Option<PathBuf>,
    pub result_path: Option<PathBuf>,
}

/// Resolved fresh workspace passed to runner/capture/finalize helpers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EphemeralWorkspace {
    pub layer_stack_root: WorkspaceRoot,
    pub workspace_root: PathBuf,
    pub caller_id: CallerId,
    pub invocation_id: InvocationId,
    pub snapshot: SnapshotLease,
    pub dirs: EphemeralRunDirs,
}

/// Local path-kind classification for captured upperdir changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathChange {
    pub path: String,
    pub kind: PathChangeKind,
}

/// The path operation kind observed in the upperdir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathChangeKind {
    Write,
    Delete,
    Symlink,
    OpaqueDir,
}

/// Publisher response normalized away from daemon-specific OCC result types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishOutcome {
    pub status: PublishStatus,
    pub manifest_version: Option<u64>,
    pub published_paths: Vec<String>,
    pub conflicts: Vec<String>,
    pub timings: BTreeMap<String, Value>,
    pub raw: Value,
}

/// Normalized publish status for daemon response shaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishStatus {
    Published,
    NoChanges,
    Conflict,
    Rejected,
}
