use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{AllocationId, RunId};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStatus {
    Passed,
    Failed,
    Cancelled,
    Incomplete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Passed,
    Failed,
    OptionalUnavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProbeReceipt {
    pub name: String,
    pub mandatory: bool,
    pub status: ProbeStatus,
    pub observed: String,
    pub expected: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvironmentReceipt {
    pub architecture: String,
    pub kernel_release: String,
    pub filesystem_type: String,
    pub filesystem_mount_options: Vec<String>,
    pub payload_mount_id: u64,
    pub control_mount_id: u64,
    pub cpu_count: u16,
    pub memory_bytes: u64,
    pub free_bytes: u64,
    pub free_inodes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct InodeWitness {
    pub relative_path: PathBuf,
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PhysicalSnapshot {
    pub allocation_id: AllocationId,
    pub allocation_path: PathBuf,
    pub device: u64,
    pub representative_inodes: Vec<InodeWitness>,
    /// Sum of regular-file logical sizes. Directory metadata is intentionally
    /// excluded so this metric agrees with semantic fixture byte accounting.
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    pub inode_count: u64,
    pub file_count: u64,
    pub directory_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QualificationReceipt {
    pub schema_version: u32,
    pub interface_version: String,
    pub run_id: RunId,
    pub status: ArtifactStatus,
    pub created_unix_ms: u64,
    pub required_image_digest: String,
    pub environment: EnvironmentReceipt,
    pub probes: Vec<ProbeReceipt>,
    pub before: PhysicalSnapshot,
    pub after: PhysicalSnapshot,
    pub artifact_path: PathBuf,
}
