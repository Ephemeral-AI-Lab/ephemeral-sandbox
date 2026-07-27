use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    AllocationId, OperationId, OwnerGeneration, PhysicalSnapshot, PublicationId, RunId, SessionId,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AllocationDescriptor {
    pub schema_version: u32,
    pub allocation_id: AllocationId,
    pub created_by_operation: OperationId,
    pub created_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AllocationHandle {
    pub descriptor: AllocationDescriptor,
    pub allocation_root: PathBuf,
    pub upper_dir: PathBuf,
    pub work_dir: PathBuf,
    pub owner_dir: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WriterCapability {
    pub allocation_id: AllocationId,
    pub session_id: SessionId,
    pub lease_epoch: u64,
    pub owner_epoch: u64,
    pub nonce: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeletionCapability {
    pub allocation_id: AllocationId,
    pub session_id: SessionId,
    pub lease_epoch: u64,
    pub owner_epoch: u64,
    pub nonce: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutableLease {
    pub schema_version: u32,
    pub allocation_id: AllocationId,
    pub session_id: SessionId,
    pub lease_epoch: u64,
    pub owner_epoch: u64,
    pub writer: WriterCapability,
    pub deleter: DeletionCapability,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StableAllocationReceipt {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub allocation: AllocationDescriptor,
    pub expected_owner_epoch: u64,
    pub before: PhysicalSnapshot,
    pub after: PhysicalSnapshot,
    pub sync_completed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OwnerTransitionRequest {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub session_id: SessionId,
    pub allocation_id: AllocationId,
    pub expected_lease_epoch: u64,
    pub expected_owner_epoch: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdoptionReceipt {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub allocation_id: AllocationId,
    pub prior_owner: OwnerGeneration,
    pub new_owner: OwnerGeneration,
    pub idempotent_replay: bool,
    pub committed_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QualificationRequest {
    pub schema_version: u32,
    pub run_id: RunId,
    pub allocation_id: AllocationId,
    pub payload_root: PathBuf,
    pub control_root: PathBuf,
    pub fixtures_root: PathBuf,
    pub evidence_root: PathBuf,
    pub lower_dir: PathBuf,
    pub allocation_root: PathBuf,
    pub workspace_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageAdminAction {
    Mount,
    Quiesce,
    StrictUnmount,
    Cleanup,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageAdminOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminScope {
    pub run_id: RunId,
    pub sandbox_id: String,
    pub workspace_session_id: String,
    pub session_id: SessionId,
    pub allocation_id: AllocationId,
    pub lease_id: String,
    pub lease_epoch: u64,
    pub mount_namespace_id: String,
    pub payload_root: PathBuf,
    pub control_root: PathBuf,
    pub lower_dirs_newest_first: Vec<PathBuf>,
    pub allocation_root: PathBuf,
    pub workspace_root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminRequest {
    pub schema_version: u32,
    pub interface_version: String,
    pub profile_id: String,
    pub operation_id: OperationId,
    pub action: StorageAdminAction,
    pub scope: StorageAdminScope,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminAuthorization {
    pub authenticated: bool,
    pub actor_id: String,
    pub operation_id: OperationId,
    pub run_id: RunId,
    pub sandbox_id: String,
    pub workspace_session_id: String,
    pub session_id: SessionId,
    pub allocation_id: AllocationId,
    pub lease_id: String,
    pub lease_epoch: u64,
    pub mount_namespace_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminReceipt {
    pub schema_version: u32,
    pub interface_version: String,
    pub profile_id: String,
    pub operation_id: OperationId,
    pub action: StorageAdminAction,
    pub request_sha256: String,
    pub trusted_executable: PathBuf,
    pub effective_capabilities: Vec<String>,
    pub allowed_privileged_syscalls: Vec<String>,
    pub process_evidence: crate::storage_admin::StorageAdminProcessEvidence,
    pub mount_plan_evidence: crate::storage_admin::StorageAdminMountPlanEvidence,
    pub scope: StorageAdminScope,
    pub outcome: StorageAdminOutcome,
    pub idempotent_replay: bool,
    pub cleanup_complete: bool,
    pub failure: Option<String>,
    pub started_unix_ms: u64,
    pub completed_unix_ms: u64,
    pub receipt_path: PathBuf,
}
