use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::fs::File;
use std::fs::{self, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_family = "unix")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

#[cfg(target_os = "linux")]
use rustix::fs::{statx, AtFlags, StatxFlags};
#[cfg(target_os = "linux")]
use sandbox_runtime_overlay::{
    mount_overlay_with_lower_inspection as mount_kernel_overlay_with_lower_inspection,
    OpenedLowerBinding, OpenedPathIdentity, OverlayHandle,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{
    begin_durability_batch, read_json, write_immutable_json, DurabilityBatch, FileLock,
};
use crate::{
    unix_time_ms, OperationId, PocError, PocResult, SemanticBuildReceipt, SemanticBuildRequest,
    StorageAdminAction, StorageAdminAuthorization, StorageAdminDurability, StorageAdminOutcome,
    StorageAdminReceipt, StorageAdminRequest, StorageAdminScope, INTERFACE_VERSION, SCHEMA_VERSION,
    STORAGE_ADMIN_EFFECTIVE_CAPABILITIES,
    STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_EFFECTIVE_CAPABILITIES,
    STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID,
    STORAGE_ADMIN_PRIVILEGED_SYSCALLS, STORAGE_ADMIN_PROFILE_ID, STORAGE_ADMIN_TRUSTED_EXECUTABLE,
};

const STORAGE_ADMIN_DIRECTORY: &str = "storage-admin";
const ATTEMPT_FILE: &str = "ATTEMPT.json";
const PUBLICATION_SEQUENCE_DIRECTORY: &str = "publication-sequences";
const PUBLICATION_SEQUENCE_ATTEMPTS_FILE: &str = "ATTEMPTS.json";
const RECEIPT_VALIDATION_DIAGNOSTIC_FILE: &str = "RECEIPT_VALIDATION_DIAGNOSTIC.json";
const RECEIPT_FILE: &str = "RECEIPT.json";
const LOCK_FILE: &str = "LOCK";
const MAX_INVOCATION_BYTES: usize = 1024 * 1024;
const MAX_INVOCATION_SEQUENCE_BYTES: usize = 3 * MAX_INVOCATION_BYTES;
pub const HOLDER_NAMESPACE_SEMANTIC_SNAPSHOT_FORMAT: &str =
    "mpla-holder-namespace-semantic-snapshot-v1";
const MAX_RECEIPT_DIAGNOSTIC_FIELD_BYTES: usize = 1024;
const MAX_RECEIPT_DIAGNOSTIC_MOUNT_OPTIONS: usize = 32;
const RAW_OVERLAY_MOUNTINFO_SOURCE: &str = "none";
const CAP_DAC_OVERRIDE_BIT: u64 = 1 << 1;
const CAP_SYS_ADMIN_BIT: u64 = 1 << 21;
pub const STORAGE_ADMIN_SECCOMP_PROFILE_ID: &str = "mpla-storage-admin-v1-seccomp-v1";
const STORAGE_ADMIN_SECCOMP_PROFILE_CANONICAL: &[u8] =
    b"mpla-storage-admin-v1-seccomp-v1;default=allow;deny=clone,clone3,execve,execveat,fork,vfork;errno=EPERM";
#[cfg(target_os = "linux")]
const CAP_DAC_OVERRIDE_NUMBER: u32 = 1;
#[cfg(target_os = "linux")]
const CAP_SYS_ADMIN_NUMBER: u32 = 21;
#[cfg(target_os = "linux")]
const CAP_SETPCAP_NUMBER: u32 = 8;
#[cfg(target_os = "linux")]
const CAPABILITY_WORDS: usize = 2;
#[cfg(target_os = "linux")]
const MAX_CAPABILITY: u32 = 63;
#[cfg(target_os = "linux")]
const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
#[cfg(target_os = "linux")]
const PR_CAP_AMBIENT: libc::c_int = 47;
#[cfg(target_os = "linux")]
const PR_CAP_AMBIENT_CLEAR_ALL: libc::c_ulong = 4;
#[cfg(target_os = "linux")]
const PR_CAPBSET_DROP: libc::c_int = 24;
#[cfg(target_os = "linux")]
const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SYS_SECCOMP: libc::c_long = 317;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const SYS_SECCOMP: libc::c_long = 277;
#[cfg(target_os = "linux")]
const SECCOMP_SET_MODE_FILTER: libc::c_long = 1;
#[cfg(target_os = "linux")]
const BPF_LD: u16 = 0x00;
#[cfg(target_os = "linux")]
const BPF_W: u16 = 0x00;
#[cfg(target_os = "linux")]
const BPF_ABS: u16 = 0x20;
#[cfg(target_os = "linux")]
const BPF_JMP: u16 = 0x05;
#[cfg(target_os = "linux")]
const BPF_JEQ: u16 = 0x10;
#[cfg(target_os = "linux")]
const BPF_JGE: u16 = 0x30;
#[cfg(target_os = "linux")]
const BPF_RET: u16 = 0x06;
#[cfg(target_os = "linux")]
const BPF_K: u16 = 0x00;
#[cfg(target_os = "linux")]
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
#[cfg(target_os = "linux")]
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
#[cfg(target_os = "linux")]
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
#[cfg(target_os = "linux")]
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
#[cfg(target_os = "linux")]
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
#[cfg(target_os = "linux")]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

#[cfg(target_os = "linux")]
#[repr(C)]
struct SockFprog {
    len: libc::c_ushort,
    filter: *const SockFilter,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageAdminInvocation {
    pub expected_request: StorageAdminRequest,
    pub request: StorageAdminRequest,
    pub authorization: StorageAdminAuthorization,
    pub trusted_actor_id: String,
    pub durability: StorageAdminDurability,
    /// Hash measured by the public runtime immediately before it executes the
    /// fixed helper.  The helper re-measures `/proc/self/exe` before mounting,
    /// closing the otherwise unrecorded executable-substitution gap.
    pub trusted_executable_sha256: String,
    /// Server-owned cgroup v2 membership file.  The public dispatcher places
    /// the helper there before sending this invocation; the helper verifies
    /// that membership again before any mount syscall.
    pub workload_cgroup_procs: PathBuf,
    pub mount_namespace_holder_pid: u32,
    pub mount_receipt_binding: Option<StorageAdminMountReceiptBinding>,
}

/// A fixed, server-bound request to scan the mounted OverlayFS tree from the
/// holder mount namespace.  It deliberately reuses the authenticated storage
/// authority rather than treating an arbitrary service-side path as a
/// semantic source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HolderNamespaceSemanticSnapshotInvocation {
    pub format: String,
    pub storage_admin: StorageAdminInvocation,
    pub semantic: SemanticBuildRequest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HolderNamespaceSemanticSnapshotReceipt {
    pub format: String,
    pub semantic: SemanticBuildReceipt,
    /// Evidence measured after the helper joined the holder namespace and
    /// installed its fixed privilege and syscall policy.
    pub process: StorageAdminProcessEvidence,
}

/// The only authority profiles the fixed storage helper understands.  The
/// profile enters the helper only through the daemon-reconstructed expected
/// request; public callers can at most echo that value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StorageAdminCapabilityProfile {
    #[default]
    Production,
    OverlayfsDacOverrideQualification,
}

impl StorageAdminCapabilityProfile {
    pub fn from_profile_id(profile_id: &str) -> PocResult<Self> {
        match profile_id {
            STORAGE_ADMIN_PROFILE_ID => Ok(Self::Production),
            STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID => {
                Ok(Self::OverlayfsDacOverrideQualification)
            }
            _ => Err(PocError::Integrity(
                "storage-admin profile id is not an approved server capability profile".to_owned(),
            )),
        }
    }

    #[must_use]
    pub const fn profile_id(self) -> &'static str {
        match self {
            Self::Production => STORAGE_ADMIN_PROFILE_ID,
            Self::OverlayfsDacOverrideQualification => {
                STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID
            }
        }
    }

    #[must_use]
    pub const fn effective_capabilities(self) -> &'static [&'static str] {
        match self {
            Self::Production => STORAGE_ADMIN_EFFECTIVE_CAPABILITIES,
            Self::OverlayfsDacOverrideQualification => {
                STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_EFFECTIVE_CAPABILITIES
            }
        }
    }

    #[must_use]
    pub const fn effective_capability_mask(self) -> u64 {
        match self {
            Self::Production => CAP_SYS_ADMIN_BIT,
            Self::OverlayfsDacOverrideQualification => CAP_SYS_ADMIN_BIT | CAP_DAC_OVERRIDE_BIT,
        }
    }

    #[cfg(target_os = "linux")]
    #[must_use]
    const fn capability_numbers(self) -> &'static [u32] {
        match self {
            Self::Production => &[CAP_SYS_ADMIN_NUMBER],
            Self::OverlayfsDacOverrideQualification => {
                &[CAP_SYS_ADMIN_NUMBER, CAP_DAC_OVERRIDE_NUMBER]
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageAdminSelection {
    request: StorageAdminRequest,
    request_sha256: String,
    profile: StorageAdminCapabilityProfile,
}

impl StorageAdminSelection {
    #[must_use]
    pub fn request(&self) -> &StorageAdminRequest {
        &self.request
    }

    #[must_use]
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }

    #[must_use]
    pub const fn profile_id(&self) -> &'static str {
        self.profile.profile_id()
    }

    #[must_use]
    pub const fn profile(&self) -> StorageAdminCapabilityProfile {
        self.profile
    }

    #[must_use]
    pub fn trusted_executable(&self) -> &'static Path {
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageAdminExecution {
    outcome: StorageAdminOutcome,
    cleanup_complete: bool,
    failure: Option<String>,
}

impl StorageAdminExecution {
    #[must_use]
    pub const fn succeeded() -> Self {
        Self {
            outcome: StorageAdminOutcome::Succeeded,
            cleanup_complete: true,
            failure: None,
        }
    }

    #[must_use]
    pub fn failed(failure: impl Into<String>, cleanup_complete: bool) -> Self {
        Self {
            outcome: StorageAdminOutcome::Failed,
            cleanup_complete,
            failure: Some(failure.into()),
        }
    }

    #[must_use]
    pub fn cancelled(failure: impl Into<String>, cleanup_complete: bool) -> Self {
        Self {
            outcome: StorageAdminOutcome::Cancelled,
            cleanup_complete,
            failure: Some(failure.into()),
        }
    }
}

pub trait StorageAdminLifecycle {
    fn execute(
        &mut self,
        action: StorageAdminAction,
        scope: &StorageAdminScope,
    ) -> StorageAdminExecution;

    fn recover_incomplete(
        &mut self,
        action: StorageAdminAction,
        _scope: &StorageAdminScope,
    ) -> StorageAdminExecution {
        StorageAdminExecution::failed(
            format!("incomplete {action:?} operation requires lifecycle recovery"),
            false,
        )
    }

    fn receipt_committed(&mut self, _action: StorageAdminAction, _scope: &StorageAdminScope) {}

    /// Return the authority observed for this operation after the helper has
    /// entered the server-bound mount namespace. A receipt is never permitted
    /// to substitute profile constants for this measurement.
    fn authority_evidence(
        &mut self,
        _scope: &StorageAdminScope,
    ) -> PocResult<(StorageAdminProcessEvidence, StorageAdminMountPlanEvidence)> {
        Err(PocError::Integrity(
            "storage-admin lifecycle did not provide measured authority evidence".to_owned(),
        ))
    }

    fn mount_authority_evidence(
        &mut self,
        _selection: &StorageAdminSelection,
        _process: &StorageAdminProcessEvidence,
        _mount_plan: &StorageAdminMountPlanEvidence,
    ) -> PocResult<(
        Option<StorageAdminMountAttestation>,
        Option<StorageAdminMountReceiptBinding>,
    )> {
        Ok((None, None))
    }

    fn cleanup_after_receipt_failure(
        &mut self,
        _action: StorageAdminAction,
        _scope: &StorageAdminScope,
    ) -> PocResult<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StorageAdminProcessProfile;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageAdminPreparationStep {
    OpenAndValidateBoundUserNamespace,
    OpenAndValidateBoundMountNamespace,
    EnterBoundUserNamespace,
    VerifyEnteredUserNamespace,
    EnterBoundMountNamespace,
    VerifyEnteredMountNamespace,
    NarrowCapabilityMasks,
    SetNoNewPrivileges,
    VerifyExecutableAndCapabilityIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminCapabilitySetEvidence {
    pub effective: u64,
    pub permitted: u64,
    pub inheritable: u64,
    pub bounding: u64,
    pub ambient: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminSeccompEvidence {
    pub profile_id: String,
    pub profile_sha256: String,
    pub mode: u32,
    pub filter_count: u32,
    pub no_new_privs: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminProcessEvidence {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub capabilities: StorageAdminCapabilitySetEvidence,
    pub seccomp: StorageAdminSeccompEvidence,
    pub workload_cgroup_procs: PathBuf,
    pub workload_cgroup_member_pid: u32,
    pub mount_namespace_id: String,
    pub mount_namespace_inode: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminMountPlanEvidence {
    pub mount_namespace_id: String,
    pub source: String,
    pub filesystem_type: String,
    pub target: PathBuf,
    pub flags: Vec<String>,
    pub lower_dirs_newest_first: Vec<PathBuf>,
    pub upper_dir: PathBuf,
    pub work_dir: PathBuf,
    /// Read-only, effective-credential measurements of every directory the
    /// mount request names.  They make a rejected mount diagnosable without
    /// changing the helper's authority or the requested filesystem operation.
    #[serde(default)]
    pub input_access: StorageAdminMountInputAccessEvidence,
    /// A digest and parsed form of the bounded target-only mountinfo record
    /// make the observation durable without embedding the host mount table in
    /// every operation receipt.
    pub mountinfo_before: StorageAdminMountTableEvidence,
    pub mountinfo_after: StorageAdminMountTableEvidence,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminMountInputAccessEvidence {
    pub paths: Vec<StorageAdminPathAccessEvidence>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminPathAccessEvidence {
    pub label: String,
    pub path: PathBuf,
    pub metadata: Option<StorageAdminPathMetadataEvidence>,
    pub metadata_error: Option<String>,
    pub effective_access: Vec<StorageAdminEffectiveAccessCheck>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminPathMetadataEvidence {
    pub is_directory: bool,
    pub device: u64,
    pub inode: u64,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminEffectiveAccessCheck {
    pub requested: Vec<String>,
    pub allowed: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminMountTableEvidence {
    pub sha256: String,
    pub target: Option<StorageAdminObservedMount>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminObservedMount {
    pub mount_id: u64,
    pub parent_mount_id: u64,
    pub root: PathBuf,
    pub source: String,
    pub filesystem_type: String,
    pub target: PathBuf,
    pub mount_options: Vec<String>,
    pub optional_fields: Vec<String>,
    pub super_options: Vec<String>,
    pub upper_dir: Option<PathBuf>,
    pub work_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminPathIdentity {
    pub mount_id: u64,
    pub device_major: u32,
    pub device_minor: u32,
    pub inode: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminLowerBinding {
    pub index: usize,
    pub authorized_path_sha256: String,
    pub fd_identity: StorageAdminPathIdentity,
    pub authorized_path_identity: StorageAdminPathIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminTargetBinding {
    pub workspace_target: PathBuf,
    pub mount_namespace_id: String,
    pub mount_namespace_inode: u64,
    pub mount_id: u64,
    pub mountinfo_sha256: String,
    pub target_identity: StorageAdminPathIdentity,
    pub filesystem_type: String,
    pub source: String,
    pub mount_options: Vec<String>,
    pub super_options: Vec<String>,
    pub expected_upperdir_sha256: String,
    pub observed_upperdir_sha256: String,
    pub expected_workdir_sha256: String,
    pub observed_workdir_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminMountAttestation {
    pub schema_version: u32,
    pub run_id: crate::RunId,
    pub sandbox_id: String,
    pub workspace_session_id: String,
    pub session_id: crate::SessionId,
    pub allocation_id: crate::AllocationId,
    pub lease_id: String,
    pub lease_epoch: u64,
    pub mount_namespace_id: String,
    pub mount_namespace_inode: u64,
    pub storage_operation_id: crate::OperationId,
    pub request_sha256: String,
    pub lower_bindings_newest_first: Vec<StorageAdminLowerBinding>,
    pub target: StorageAdminTargetBinding,
    pub profile_id: String,
    pub effective_capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminMountReceiptBinding {
    pub storage_operation_id: crate::OperationId,
    pub attestation_sha256: String,
}

impl StorageAdminProcessProfile {
    #[must_use]
    pub const fn profile_id(self) -> &'static str {
        STORAGE_ADMIN_PROFILE_ID
    }

    #[must_use]
    pub fn trusted_executable(self) -> &'static Path {
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    }

    #[must_use]
    pub const fn effective_capabilities(self) -> &'static [&'static str] {
        STORAGE_ADMIN_EFFECTIVE_CAPABILITIES
    }

    #[must_use]
    pub const fn effective_capability_mask(self) -> u64 {
        CAP_SYS_ADMIN_BIT
    }

    #[must_use]
    pub const fn permitted_capability_mask(self) -> u64 {
        CAP_SYS_ADMIN_BIT
    }

    #[must_use]
    pub const fn inheritable_capability_mask(self) -> u64 {
        0
    }

    #[must_use]
    pub const fn ambient_capability_mask(self) -> u64 {
        0
    }

    #[must_use]
    pub const fn preparation_steps(self) -> &'static [StorageAdminPreparationStep] {
        &[
            StorageAdminPreparationStep::OpenAndValidateBoundUserNamespace,
            StorageAdminPreparationStep::OpenAndValidateBoundMountNamespace,
            StorageAdminPreparationStep::EnterBoundUserNamespace,
            StorageAdminPreparationStep::VerifyEnteredUserNamespace,
            StorageAdminPreparationStep::EnterBoundMountNamespace,
            StorageAdminPreparationStep::VerifyEnteredMountNamespace,
            StorageAdminPreparationStep::NarrowCapabilityMasks,
            StorageAdminPreparationStep::SetNoNewPrivileges,
            StorageAdminPreparationStep::VerifyExecutableAndCapabilityIdentity,
        ]
    }

    pub fn user_namespace_path(self, holder_pid: u32) -> PocResult<PathBuf> {
        user_namespace_path(holder_pid)
    }

    pub fn mount_namespace_path(self, holder_pid: u32) -> PocResult<PathBuf> {
        mount_namespace_path(holder_pid)
    }

    #[must_use]
    pub const fn allowed_privileged_syscalls(self) -> &'static [&'static str] {
        STORAGE_ADMIN_PRIVILEGED_SYSCALLS
    }

    #[must_use]
    pub const fn allows_arbitrary_executable(self) -> bool {
        false
    }

    #[must_use]
    pub const fn allows_workload_entry(self) -> bool {
        false
    }

    #[must_use]
    pub const fn allows_workload_descendants(self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OrdinaryWorkloadPolicy;

impl OrdinaryWorkloadPolicy {
    #[must_use]
    pub const fn effective_capabilities(self) -> &'static [&'static str] {
        &[]
    }

    #[must_use]
    pub const fn allowed_privileged_syscalls(self) -> &'static [&'static str] {
        &[]
    }

    #[must_use]
    pub const fn denies_syscall(self, syscall: &str) -> bool {
        matches!(syscall.as_bytes(), b"mount" | b"umount2")
    }

    #[must_use]
    pub const fn can_select_storage_admin_profile(self) -> bool {
        false
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StorageAdminAttempt {
    schema_version: u32,
    interface_version: String,
    operation_id: crate::OperationId,
    request_sha256: String,
    request: StorageAdminRequest,
    authorization: StorageAdminAuthorization,
    #[serde(default)]
    durability: StorageAdminDurability,
    trusted_executable_sha256: String,
    workload_cgroup_procs: PathBuf,
    mount_namespace_holder_pid: u32,
    mount_receipt_binding: Option<StorageAdminMountReceiptBinding>,
    started_unix_ms: u64,
}

/// One durable authorization record covers the three fixed publication
/// actions.  It replaces three independently-synced `ATTEMPT.json` records,
/// but deliberately leaves the canonical per-operation `RECEIPT.json` files
/// unchanged: external publication validation can therefore continue to
/// authenticate each action at its established path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicationSequenceAttempts {
    schema_version: u32,
    interface_version: String,
    attempts: Vec<StorageAdminAttempt>,
}

#[derive(Debug, Serialize)]
struct StorageAdminReceiptValidationDiagnostic {
    schema_version: u32,
    interface_version: String,
    operation_id: crate::OperationId,
    request_sha256: String,
    mountinfo_sha256: String,
    filesystem_type: String,
    parsed_source: String,
    mount_options: Vec<String>,
    trusted_expected_source: String,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: i32,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
#[repr(C)]
struct CapabilityData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

#[derive(Debug, Default)]
pub struct PlatformStorageLifecycle {
    mounted_by_this_process: Option<PathBuf>,
    workspace_prequiesced: bool,
    authority_evidence: Option<(StorageAdminProcessEvidence, StorageAdminMountPlanEvidence)>,
    authority_evidence_error: Option<PocError>,
    selection: Option<StorageAdminSelection>,
    mount_attestation: Option<StorageAdminMountAttestation>,
    trusted_mount_attestation: Option<StorageAdminMountAttestation>,
    mount_receipt_binding: Option<StorageAdminMountReceiptBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlatformPublicationUnmountResult {
    pub receipts: Vec<StorageAdminReceipt>,
    pub input_decode_elapsed_ns: u64,
    pub validation_elapsed_ns: u64,
    pub process_preparation_elapsed_ns: u64,
    pub quiesce_lifecycle_elapsed_ns: u64,
    pub quiesce_operation_elapsed_ns: u64,
    pub strict_unmount_lifecycle_elapsed_ns: u64,
    pub strict_unmount_operation_elapsed_ns: u64,
}

/// Immutable mount authority shared only by the three fixed actions of a
/// single publication helper invocation.  The action receipts still record
/// independently captured mount plans and are committed independently; this
/// object only avoids reopening the same already-validated mount receipt.
#[derive(Clone, Debug)]
struct PublicationSequenceAuthority {
    trusted_mount_attestation: StorageAdminMountAttestation,
}

impl PlatformStorageLifecycle {
    fn measured(
        process: StorageAdminProcessEvidence,
        selection: StorageAdminSelection,
        mount_receipt_binding: Option<StorageAdminMountReceiptBinding>,
    ) -> PocResult<Self> {
        let scope = &selection.request.scope;
        let mut mount_plan = storage_admin_mount_plan_evidence(scope)?;
        mount_plan.mountinfo_before = capture_storage_admin_mountinfo(scope)?;
        let trusted_mount_attestation = match selection.request.action {
            StorageAdminAction::Mount => None,
            _ => Some(load_mount_receipt_attestation(
                scope,
                selection.profile(),
                mount_receipt_binding.as_ref().ok_or_else(|| {
                    PocError::Integrity(
                        "storage-admin lifecycle action is missing mount receipt authority"
                            .to_owned(),
                    )
                })?,
            )?),
        };
        Ok(Self {
            mounted_by_this_process: None,
            workspace_prequiesced: false,
            authority_evidence: Some((process, mount_plan)),
            authority_evidence_error: None,
            selection: Some(selection),
            mount_attestation: None,
            trusted_mount_attestation,
            mount_receipt_binding,
        })
    }

    fn measured_with_publication_authority(
        process: StorageAdminProcessEvidence,
        selection: StorageAdminSelection,
        mount_receipt_binding: Option<StorageAdminMountReceiptBinding>,
        authority: &PublicationSequenceAuthority,
    ) -> PocResult<Self> {
        if selection.request.action == StorageAdminAction::Mount {
            return Err(PocError::Integrity(
                "publication sequence authority cannot be used for Mount".to_owned(),
            ));
        }
        let scope = &selection.request.scope;
        let binding = mount_receipt_binding.as_ref().ok_or_else(|| {
            PocError::Integrity(
                "storage-admin lifecycle action is missing mount receipt authority".to_owned(),
            )
        })?;
        validate_mount_receipt_binding_for_action(selection.request.action, Some(binding))?;
        validate_attestation_scope(scope, &authority.trusted_mount_attestation)?;
        require_equal(
            "publication sequence mount authority operation",
            &authority.trusted_mount_attestation.storage_operation_id,
            &binding.storage_operation_id,
        )?;
        require_equal(
            "publication sequence mount authority digest",
            &storage_admin_mount_attestation_sha256(&authority.trusted_mount_attestation)?,
            &binding.attestation_sha256,
        )?;

        let mut mount_plan = storage_admin_mount_plan_evidence(scope)?;
        mount_plan.mountinfo_before = capture_storage_admin_mountinfo(scope)?;
        Ok(Self {
            mounted_by_this_process: None,
            workspace_prequiesced: false,
            authority_evidence: Some((process, mount_plan)),
            authority_evidence_error: None,
            selection: Some(selection),
            mount_attestation: None,
            trusted_mount_attestation: Some(authority.trusted_mount_attestation.clone()),
            mount_receipt_binding,
        })
    }

    fn measured_after_quiesce_with_publication_authority(
        process: StorageAdminProcessEvidence,
        selection: StorageAdminSelection,
        mount_receipt_binding: Option<StorageAdminMountReceiptBinding>,
        authority: &PublicationSequenceAuthority,
    ) -> PocResult<Self> {
        let mut lifecycle = Self::measured_with_publication_authority(
            process,
            selection,
            mount_receipt_binding,
            authority,
        )?;
        lifecycle.workspace_prequiesced = true;
        Ok(lifecycle)
    }

    fn refresh_mountinfo_after(&mut self, scope: &StorageAdminScope) {
        let Some((_, mount_plan)) = self.authority_evidence.as_mut() else {
            self.authority_evidence_error = Some(PocError::Integrity(
                "platform storage-admin lifecycle lost its mount-table evidence".to_owned(),
            ));
            return;
        };
        match capture_storage_admin_mountinfo(scope) {
            Ok(observation) => mount_plan.mountinfo_after = observation,
            Err(error) => self.authority_evidence_error = Some(error),
        }
    }

    fn cleanup_verified_mount(&mut self, scope: &StorageAdminScope) -> PocResult<()> {
        if self.mounted_by_this_process.is_none() {
            return Ok(());
        }
        let attestation = self.mount_attestation.as_ref().ok_or_else(|| {
            PocError::RecoveryRequired(
                "automatic cleanup is forbidden without the just-created mount attestation"
                    .to_owned(),
            )
        })?;
        validate_current_attested_target(scope, attestation)?;
        strict_unmount_path(&scope.workspace_root)?;
        self.mounted_by_this_process = None;
        cleanup_platform_state(scope, &mut self.mounted_by_this_process)
    }
}

impl StorageAdminLifecycle for PlatformStorageLifecycle {
    fn execute(
        &mut self,
        action: StorageAdminAction,
        scope: &StorageAdminScope,
    ) -> StorageAdminExecution {
        let action_result = match action {
            StorageAdminAction::Mount => {
                let selection = self.selection.clone().ok_or_else(|| {
                    PocError::Integrity(
                        "platform storage-admin lifecycle lost its selected request".to_owned(),
                    )
                });
                let process = self
                    .authority_evidence
                    .as_ref()
                    .map(|(process, _)| process.clone())
                    .ok_or_else(|| {
                        PocError::Integrity(
                            "platform storage-admin lifecycle lost its process evidence".to_owned(),
                        )
                    });
                let mount_plan = self
                    .authority_evidence
                    .as_ref()
                    .map(|(_, mount_plan)| mount_plan.clone())
                    .ok_or_else(|| {
                        PocError::Integrity(
                            "platform storage-admin lifecycle lost its mount plan".to_owned(),
                        )
                    });
                selection
                    .and_then(|selection| process.map(|process| (selection, process)))
                    .and_then(|(selection, process)| {
                        mount_plan.map(|mount_plan| (selection, process, mount_plan))
                    })
                    .and_then(|(selection, process, mount_plan)| {
                        mount_overlay_with_attestation(
                            scope,
                            &selection,
                            &process,
                            &mount_plan,
                            &mut self.mounted_by_this_process,
                        )
                    })
                    .and_then(|(attestation, observation)| {
                        let mount_receipt_binding = StorageAdminMountReceiptBinding {
                            storage_operation_id: attestation.storage_operation_id.clone(),
                            attestation_sha256: storage_admin_mount_attestation_sha256(
                                &attestation,
                            )?,
                        };
                        self.mount_attestation = Some(attestation);
                        self.mount_receipt_binding = Some(mount_receipt_binding);
                        if let Some((_, mount_plan)) = self.authority_evidence.as_mut() {
                            mount_plan.mountinfo_after = observation;
                        }
                        Ok(())
                    })
            }
            _ => self
                .trusted_mount_attestation
                .clone()
                .ok_or_else(|| {
                    PocError::Integrity(
                        "platform storage-admin lifecycle lost its mount attestation".to_owned(),
                    )
                })
                .and_then(|attestation| {
                    validate_target_before_action(action, scope, &attestation)?;
                    execute_platform_action(
                        action,
                        scope,
                        &mut self.mounted_by_this_process,
                        self.workspace_prequiesced,
                    )
                    .and_then(|()| {
                        if action == StorageAdminAction::Quiesce {
                            validate_current_attested_target(scope, &attestation)
                        } else {
                            validate_target_after_action(action, scope)
                        }
                    })
                }),
        };
        let execution = match action_result {
            Ok(()) => StorageAdminExecution::succeeded(),
            Err(error) if action == StorageAdminAction::Mount => {
                let cleanup = self.cleanup_verified_mount(scope);
                let cleanup_complete = cleanup.is_ok();
                let failure = match cleanup {
                    Ok(()) => error.to_string(),
                    Err(cleanup_error) => format!("{error}; cleanup failed: {cleanup_error}"),
                };
                StorageAdminExecution::failed(failure, cleanup_complete)
            }
            Err(error) => StorageAdminExecution::failed(error.to_string(), false),
        };
        if action != StorageAdminAction::Mount || self.mount_attestation.is_none() {
            self.refresh_mountinfo_after(scope);
        }
        execution
    }

    fn recover_incomplete(
        &mut self,
        action: StorageAdminAction,
        scope: &StorageAdminScope,
    ) -> StorageAdminExecution {
        if action == StorageAdminAction::Mount {
            self.refresh_mountinfo_after(scope);
            return StorageAdminExecution::failed(
                "incomplete Mount has no durable lower-binding attestation; automatic cleanup is forbidden",
                false,
            );
        }
        let recovery = self
            .trusted_mount_attestation
            .clone()
            .ok_or_else(|| {
                PocError::Integrity(
                    "incomplete lifecycle recovery lost its mount attestation".to_owned(),
                )
            })
            .and_then(|attestation| {
                validate_attestation_scope(scope, &attestation)?;
                match action {
                    StorageAdminAction::Quiesce => {
                        validate_current_attested_target(scope, &attestation)?;
                        syncfs_path(&scope.workspace_root)?;
                        validate_current_attested_target(scope, &attestation)
                    }
                    StorageAdminAction::StrictUnmount => {
                        if capture_storage_admin_mountinfo(scope)?.target.is_some() {
                            validate_current_attested_target(scope, &attestation)?;
                            syncfs_path(&scope.workspace_root)?;
                            strict_unmount_path(&scope.workspace_root)?;
                        }
                        require_target_absent(scope)
                    }
                    StorageAdminAction::Cleanup => {
                        require_target_absent(scope)?;
                        cleanup_platform_state(scope, &mut self.mounted_by_this_process)?;
                        require_target_absent(scope)
                    }
                    StorageAdminAction::Mount => Err(PocError::Integrity(
                        "mount recovery reached a forbidden lifecycle branch".to_owned(),
                    )),
                }
            });
        let execution = match recovery {
            Ok(()) => StorageAdminExecution::succeeded(),
            Err(error) => StorageAdminExecution::failed(
                format!("incomplete {action:?} recovery rejected: {error}"),
                false,
            ),
        };
        self.refresh_mountinfo_after(scope);
        execution
    }

    fn receipt_committed(&mut self, _action: StorageAdminAction, _scope: &StorageAdminScope) {
        self.mounted_by_this_process = None;
    }

    fn authority_evidence(
        &mut self,
        _scope: &StorageAdminScope,
    ) -> PocResult<(StorageAdminProcessEvidence, StorageAdminMountPlanEvidence)> {
        if let Some(error) = self.authority_evidence_error.take() {
            return Err(error);
        }
        self.authority_evidence.clone().ok_or_else(|| {
            PocError::Integrity(
                "platform storage-admin lifecycle lost its measured authority evidence".to_owned(),
            )
        })
    }

    fn mount_authority_evidence(
        &mut self,
        _selection: &StorageAdminSelection,
        _process: &StorageAdminProcessEvidence,
        _mount_plan: &StorageAdminMountPlanEvidence,
    ) -> PocResult<(
        Option<StorageAdminMountAttestation>,
        Option<StorageAdminMountReceiptBinding>,
    )> {
        Ok((
            self.mount_attestation.clone(),
            self.mount_receipt_binding.clone(),
        ))
    }

    fn cleanup_after_receipt_failure(
        &mut self,
        action: StorageAdminAction,
        scope: &StorageAdminScope,
    ) -> PocResult<()> {
        match action {
            StorageAdminAction::Mount => self.cleanup_verified_mount(scope),
            StorageAdminAction::Quiesce
            | StorageAdminAction::StrictUnmount
            | StorageAdminAction::Cleanup => Ok(()),
        }
    }
}

impl Drop for PlatformStorageLifecycle {
    fn drop(&mut self) {
        let Some(workspace_root) = self.mounted_by_this_process.take() else {
            return;
        };
        let Some(attestation) = self.mount_attestation.as_ref() else {
            return;
        };
        let scope = self
            .selection
            .as_ref()
            .map(|selection| &selection.request.scope);
        if scope.is_some_and(|scope| {
            scope.workspace_root == workspace_root
                && validate_current_attested_target(scope, attestation).is_ok()
        }) {
            let _ = strict_unmount_path(&workspace_root);
        }
    }
}

pub fn decode_invocation(bytes: &[u8]) -> PocResult<StorageAdminInvocation> {
    if bytes.len() > MAX_INVOCATION_BYTES {
        return Err(PocError::Integrity(format!(
            "storage-admin invocation exceeds {MAX_INVOCATION_BYTES} bytes"
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    validate_wire_shape(&value)?;
    let invocation: StorageAdminInvocation = serde_json::from_value(value)?;
    validate_storage_admin_durability(invocation.request.action, invocation.durability)?;
    validate_mount_namespace_holder_pid(invocation.mount_namespace_holder_pid)?;
    validate_sha256(
        "bound trusted executable hash",
        &invocation.trusted_executable_sha256,
    )?;
    validate_workload_cgroup_procs(&invocation.workload_cgroup_procs)?;
    Ok(invocation)
}

/// Decode the one holder-namespace semantic snapshot wire format.  The
/// request has an exact JSON shape so an untrusted caller cannot smuggle
/// scanner paths or ambient storage authority through an ignored field.
pub fn decode_holder_namespace_semantic_snapshot_invocation(
    bytes: &[u8],
) -> PocResult<HolderNamespaceSemanticSnapshotInvocation> {
    if bytes.len() > MAX_INVOCATION_BYTES {
        return Err(PocError::Integrity(format!(
            "holder-namespace semantic snapshot exceeds {MAX_INVOCATION_BYTES} bytes"
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    validate_holder_namespace_semantic_snapshot_wire_shape(&value)?;
    let invocation: HolderNamespaceSemanticSnapshotInvocation = serde_json::from_value(value)?;
    if invocation.format != HOLDER_NAMESPACE_SEMANTIC_SNAPSHOT_FORMAT {
        return Err(PocError::Integrity(
            "holder-namespace semantic snapshot format is unsupported".to_owned(),
        ));
    }
    validate_storage_admin_durability(
        invocation.storage_admin.request.action,
        invocation.storage_admin.durability,
    )?;
    validate_mount_namespace_holder_pid(invocation.storage_admin.mount_namespace_holder_pid)?;
    validate_sha256(
        "bound trusted executable hash",
        &invocation.storage_admin.trusted_executable_sha256,
    )?;
    validate_workload_cgroup_procs(&invocation.storage_admin.workload_cgroup_procs)?;
    Ok(invocation)
}

/// Execute the typed initial semantic scan after entering the live holder's
/// mount namespace.  This is not a storage lifecycle action: Quiesce is used
/// solely as the already-mounted, attested authority shape and no Quiesce
/// attempt or receipt is written here.
pub fn run_platform_holder_namespace_semantic_snapshot(
    invocation: &HolderNamespaceSemanticSnapshotInvocation,
) -> PocResult<HolderNamespaceSemanticSnapshotReceipt> {
    if invocation.format != HOLDER_NAMESPACE_SEMANTIC_SNAPSHOT_FORMAT {
        return Err(PocError::Integrity(
            "holder-namespace semantic snapshot format is unsupported".to_owned(),
        ));
    }
    let storage = &invocation.storage_admin;
    let selection = authorize_storage_admin(
        &storage.expected_request,
        &storage.request,
        &storage.authorization,
        &storage.trusted_actor_id,
    )?;
    if selection.request.action != StorageAdminAction::Quiesce {
        return Err(PocError::Integrity(
            "holder-namespace semantic snapshot requires Quiesce mount authority".to_owned(),
        ));
    }
    validate_storage_admin_durability(selection.request.action, storage.durability)?;
    validate_mount_namespace_holder_pid(storage.mount_namespace_holder_pid)?;
    validate_sha256(
        "bound trusted executable hash",
        &storage.trusted_executable_sha256,
    )?;
    validate_workload_cgroup_procs(&storage.workload_cgroup_procs)?;
    validate_mount_receipt_binding_for_action(
        selection.request.action,
        storage.mount_receipt_binding.as_ref(),
    )?;
    validate_holder_namespace_snapshot_request(&selection, &invocation.semantic)?;

    let process = prepare_platform_process(storage, selection.profile())?;
    require_equal(
        "measured executable hash",
        process.executable_sha256.as_str(),
        storage.trusted_executable_sha256.as_str(),
    )?;
    let mount_binding = storage.mount_receipt_binding.as_ref().ok_or_else(|| {
        PocError::Integrity(
            "holder-namespace semantic snapshot is missing mount receipt authority".to_owned(),
        )
    })?;
    let attestation = load_mount_receipt_attestation(
        &selection.request.scope,
        selection.profile(),
        mount_binding,
    )?;
    validate_current_attested_target(&selection.request.scope, &attestation)?;

    let output = crate::semantic::build_with_output_serial(&invocation.semantic)?;
    Ok(HolderNamespaceSemanticSnapshotReceipt {
        format: HOLDER_NAMESPACE_SEMANTIC_SNAPSHOT_FORMAT.to_owned(),
        semantic: output.receipt,
        process,
    })
}

/// Decode the one fixed multi-action transaction accepted by the privileged
/// helper. The sequence wire format is intentionally narrower than an
/// arbitrary batch: publication may only quiesce, strictly unmount, and clean
/// up one already-mounted session, in that order.
pub fn decode_publication_invocation_sequence(
    bytes: &[u8],
) -> PocResult<Vec<StorageAdminInvocation>> {
    if bytes.len() > MAX_INVOCATION_SEQUENCE_BYTES {
        return Err(PocError::Integrity(format!(
            "storage-admin publication sequence exceeds {MAX_INVOCATION_SEQUENCE_BYTES} bytes"
        )));
    }
    let values: Vec<serde_json::Value> = serde_json::from_slice(bytes)?;
    if values.len() != 3 {
        return Err(PocError::Integrity(
            "storage-admin publication sequence must contain exactly three invocations".to_owned(),
        ));
    }
    values
        .into_iter()
        .map(|value| {
            validate_wire_shape(&value)?;
            let invocation: StorageAdminInvocation = serde_json::from_value(value)?;
            validate_storage_admin_durability(invocation.request.action, invocation.durability)?;
            validate_mount_namespace_holder_pid(invocation.mount_namespace_holder_pid)?;
            validate_sha256(
                "bound trusted executable hash",
                &invocation.trusted_executable_sha256,
            )?;
            validate_workload_cgroup_procs(&invocation.workload_cgroup_procs)?;
            Ok(invocation)
        })
        .collect()
}

pub fn authorize_storage_admin(
    expected: &StorageAdminRequest,
    request: &StorageAdminRequest,
    authorization: &StorageAdminAuthorization,
    trusted_actor_id: &str,
) -> PocResult<StorageAdminSelection> {
    let profile = validate_request(expected)?;
    validate_exact_request(expected, request)?;
    validate_authorization(expected, authorization, trusted_actor_id)?;
    Ok(StorageAdminSelection {
        request: request.clone(),
        request_sha256: request_sha256(request)?,
        profile,
    })
}

pub fn run_storage_admin<L: StorageAdminLifecycle>(
    invocation: &StorageAdminInvocation,
    lifecycle: &mut L,
) -> PocResult<StorageAdminReceipt> {
    validate_mount_namespace_holder_pid(invocation.mount_namespace_holder_pid)?;
    validate_sha256(
        "bound trusted executable hash",
        &invocation.trusted_executable_sha256,
    )?;
    validate_workload_cgroup_procs(&invocation.workload_cgroup_procs)?;
    let selection = authorize_storage_admin(
        &invocation.expected_request,
        &invocation.request,
        &invocation.authorization,
        &invocation.trusted_actor_id,
    )?;
    validate_mount_receipt_binding_for_action(
        selection.request.action,
        invocation.mount_receipt_binding.as_ref(),
    )?;
    validate_storage_admin_durability(selection.request.action, invocation.durability)?;
    let session_lifetime_mount = invocation.durability == StorageAdminDurability::SessionLifetime;
    // An externally recoverable Mount has two crash-consistency publication
    // points: ATTEMPT before the mount and RECEIPT before authority returns.
    // The daemon's internal live mount is different: its allocation, holder
    // namespace, and mount graph are session-lifetime state reconstructed from
    // the durable activation journal. For that mode, keep the files available
    // to same-daemon publication but discard their deferred barriers after the
    // receipt is complete. A fresh holder namespace gets a distinct operation
    // id, so an old session-lifetime receipt cannot authorize a recovered one.
    let mut attempt_durability =
        (selection.request.action == StorageAdminAction::Mount).then(begin_durability_batch);
    let paths = operation_paths(&selection.request)?;
    prepare_operation_store(&paths)?;
    let _lock = FileLock::exclusive(&paths.lock)?;

    let stored_attempt = if paths.attempt.exists() {
        let attempt: StorageAdminAttempt = read_json(&paths.attempt)?;
        validate_stored_attempt(&attempt, &selection, invocation)?;
        Some(attempt)
    } else {
        None
    };

    if paths.receipt.exists() {
        // Replay did not create authority or new durable state. End the
        // speculative batch without adding a filesystem barrier.
        drop(attempt_durability.take());
        if stored_attempt.is_none() {
            return Err(PocError::Integrity(
                "durable storage-admin receipt is missing its bound attempt".to_owned(),
            ));
        }
        let mut receipt: StorageAdminReceipt = read_json(&paths.receipt)?;
        validate_stored_receipt(&receipt, &selection, &paths.receipt)?;
        receipt.idempotent_replay = true;
        return Ok(receipt);
    }

    let started_unix_ms = if let Some(attempt) = stored_attempt {
        attempt.started_unix_ms
    } else {
        let started_unix_ms = unix_time_ms()?;
        write_immutable_json(
            &paths.attempt,
            &StorageAdminAttempt {
                schema_version: SCHEMA_VERSION,
                interface_version: INTERFACE_VERSION.to_owned(),
                operation_id: selection.request.operation_id.clone(),
                request_sha256: selection.request_sha256.clone(),
                request: selection.request.clone(),
                authorization: invocation.authorization.clone(),
                durability: invocation.durability,
                trusted_executable_sha256: invocation.trusted_executable_sha256.clone(),
                workload_cgroup_procs: invocation.workload_cgroup_procs.clone(),
                mount_namespace_holder_pid: invocation.mount_namespace_holder_pid,
                mount_receipt_binding: invocation.mount_receipt_binding.clone(),
                started_unix_ms,
            },
        )?;
        if selection.request.action == StorageAdminAction::Mount && !session_lifetime_mount {
            let batch = attempt_durability.take().ok_or_else(|| {
                PocError::Integrity("durable Mount lost its attempt durability batch".to_owned())
            })?;
            batch.commit(&[&selection.request.scope.control_root])?;
        }
        let execution = lifecycle.execute(selection.request.action, &selection.request.scope);
        return commit_execution(
            &selection,
            lifecycle,
            execution,
            started_unix_ms,
            &paths.receipt_validation_diagnostic,
            &paths.receipt,
            if session_lifetime_mount {
                attempt_durability.take()
            } else {
                (selection.request.action == StorageAdminAction::Mount).then(begin_durability_batch)
            },
            session_lifetime_mount,
        );
    };

    // Recovery begins from an already installed ATTEMPT. Exact operations
    // retain the conservative durable path; a same-session helper retry may
    // conservatively make a session-lifetime recovery receipt durable.
    drop(attempt_durability.take());
    let execution =
        lifecycle.recover_incomplete(selection.request.action, &selection.request.scope);
    commit_execution(
        &selection,
        lifecycle,
        execution,
        started_unix_ms,
        &paths.receipt_validation_diagnostic,
        &paths.receipt,
        None,
        false,
    )
}

pub fn run_platform_invocation(
    invocation: &StorageAdminInvocation,
) -> PocResult<StorageAdminReceipt> {
    let selection = authorize_storage_admin(
        &invocation.expected_request,
        &invocation.request,
        &invocation.authorization,
        &invocation.trusted_actor_id,
    )?;
    validate_mount_namespace_holder_pid(invocation.mount_namespace_holder_pid)?;
    // Reject malformed server bindings before changing this helper's process
    // credentials.  The cgroup membership itself is verified again after the
    // helper has been placed in the bound workload cgroup.
    validate_sha256(
        "bound trusted executable hash",
        &invocation.trusted_executable_sha256,
    )?;
    validate_workload_cgroup_procs(&invocation.workload_cgroup_procs)?;
    validate_mount_receipt_binding_for_action(
        selection.request.action,
        invocation.mount_receipt_binding.as_ref(),
    )?;
    validate_storage_admin_durability(selection.request.action, invocation.durability)?;
    let process_evidence = prepare_platform_process(invocation, selection.profile())?;
    require_equal(
        "measured executable hash",
        process_evidence.executable_sha256.as_str(),
        invocation.trusted_executable_sha256.as_str(),
    )?;
    let mut lifecycle = PlatformStorageLifecycle::measured(
        process_evidence,
        selection,
        invocation.mount_receipt_binding.clone(),
    )?;
    run_storage_admin(invocation, &mut lifecycle)
}

/// Run the fixed publication storage transaction after preparing this helper
/// process exactly once. Each invocation still passes the complete
/// authorization path and commits its own durable attempt and receipt.
pub fn run_platform_publication_sequence(
    invocations: &[StorageAdminInvocation],
    before_cleanup: impl FnOnce(&PlatformPublicationUnmountResult) -> Result<(), String>,
) -> PocResult<Vec<StorageAdminReceipt>> {
    const ACTIONS: [StorageAdminAction; 3] = [
        StorageAdminAction::Quiesce,
        StorageAdminAction::StrictUnmount,
        StorageAdminAction::Cleanup,
    ];
    if invocations.len() != ACTIONS.len() {
        return Err(PocError::Integrity(
            "storage-admin publication sequence must contain exactly three invocations".to_owned(),
        ));
    }

    let sequence_started = Instant::now();
    let mut selections = Vec::with_capacity(invocations.len());
    for (invocation, action) in invocations.iter().zip(ACTIONS) {
        let selection = authorize_storage_admin(
            &invocation.expected_request,
            &invocation.request,
            &invocation.authorization,
            &invocation.trusted_actor_id,
        )?;
        if selection.request.action != action {
            return Err(PocError::Integrity(
                "storage-admin publication actions are not in the fixed order".to_owned(),
            ));
        }
        validate_mount_namespace_holder_pid(invocation.mount_namespace_holder_pid)?;
        validate_sha256(
            "bound trusted executable hash",
            &invocation.trusted_executable_sha256,
        )?;
        validate_workload_cgroup_procs(&invocation.workload_cgroup_procs)?;
        validate_mount_receipt_binding_for_action(
            selection.request.action,
            invocation.mount_receipt_binding.as_ref(),
        )?;
        validate_storage_admin_durability(selection.request.action, invocation.durability)?;
        selections.push(selection);
    }

    let first = &invocations[0];
    for invocation in &invocations[1..] {
        if invocation.request.scope != first.request.scope
            || invocation.trusted_actor_id != first.trusted_actor_id
            || invocation.durability != first.durability
            || invocation.trusted_executable_sha256 != first.trusted_executable_sha256
            || invocation.workload_cgroup_procs != first.workload_cgroup_procs
            || invocation.mount_namespace_holder_pid != first.mount_namespace_holder_pid
            || invocation.mount_receipt_binding != first.mount_receipt_binding
            || StorageAdminCapabilityProfile::from_profile_id(
                &invocation.expected_request.profile_id,
            )? != StorageAdminCapabilityProfile::from_profile_id(
                &first.expected_request.profile_id,
            )?
        {
            return Err(PocError::Integrity(
                "storage-admin publication sequence does not preserve one exact authority"
                    .to_owned(),
            ));
        }
    }
    let validation_elapsed_ns = elapsed_ns(sequence_started);

    let process_preparation_started = Instant::now();
    let process_evidence = prepare_platform_process(first, selections[0].profile())?;
    require_equal(
        "measured executable hash",
        process_evidence.executable_sha256.as_str(),
        first.trusted_executable_sha256.as_str(),
    )?;
    let process_preparation_elapsed_ns = elapsed_ns(process_preparation_started);

    // The sequence already proved all three actions have the exact same
    // binding.  Load and validate that immutable receipt once, then retain
    // fresh per-action mount observations and durable receipts below.
    let mount_receipt_binding = first.mount_receipt_binding.as_ref().ok_or_else(|| {
        PocError::Integrity(
            "storage-admin publication sequence is missing mount receipt authority".to_owned(),
        )
    })?;
    let publication_authority = PublicationSequenceAuthority {
        trusted_mount_attestation: load_mount_receipt_attestation(
            &first.request.scope,
            selections[0].profile(),
            mount_receipt_binding,
        )?,
    };

    // Persist the complete, exact authorization set before any lifecycle
    // action.  A retry can then recover a missing action safely without the
    // six file-and-directory barriers previously paid by three independent
    // attempt records.  Receipts remain independently immutable below.
    let (operation_paths, sequence_resumed, sequence_started_unix_ms, _sequence_lock) =
        prepare_publication_sequence_store(invocations, &selections)?;

    let mut receipts = Vec::with_capacity(invocations.len());
    let mut lifecycle_elapsed_ns = [0_u64; 2];
    let mut operation_elapsed_ns = [0_u64; 2];
    for (index, (invocation, selection)) in invocations
        .iter()
        .zip(selections.iter().cloned())
        .take(2)
        .enumerate()
    {
        let lifecycle_started = Instant::now();
        let mut lifecycle = if index == 1 {
            PlatformStorageLifecycle::measured_after_quiesce_with_publication_authority(
                process_evidence.clone(),
                selection.clone(),
                invocation.mount_receipt_binding.clone(),
                &publication_authority,
            )?
        } else {
            PlatformStorageLifecycle::measured_with_publication_authority(
                process_evidence.clone(),
                selection.clone(),
                invocation.mount_receipt_binding.clone(),
                &publication_authority,
            )?
        };
        lifecycle_elapsed_ns[index] = elapsed_ns(lifecycle_started);
        let operation_started = Instant::now();
        let receipt = run_publication_sequence_action(
            &selection,
            &operation_paths[index],
            sequence_resumed,
            sequence_started_unix_ms,
            &mut lifecycle,
        )?;
        operation_elapsed_ns[index] = elapsed_ns(operation_started);
        let succeeded = receipt.outcome == StorageAdminOutcome::Succeeded;
        receipts.push(receipt);
        if !succeeded {
            before_cleanup(&PlatformPublicationUnmountResult {
                receipts: receipts.clone(),
                input_decode_elapsed_ns: 0,
                validation_elapsed_ns,
                process_preparation_elapsed_ns,
                quiesce_lifecycle_elapsed_ns: lifecycle_elapsed_ns[0],
                quiesce_operation_elapsed_ns: operation_elapsed_ns[0],
                strict_unmount_lifecycle_elapsed_ns: lifecycle_elapsed_ns[1],
                strict_unmount_operation_elapsed_ns: operation_elapsed_ns[1],
            })
            .map_err(PocError::Integrity)?;
            return Ok(receipts);
        }
    }
    before_cleanup(&PlatformPublicationUnmountResult {
        receipts: receipts.clone(),
        input_decode_elapsed_ns: 0,
        validation_elapsed_ns,
        process_preparation_elapsed_ns,
        quiesce_lifecycle_elapsed_ns: lifecycle_elapsed_ns[0],
        quiesce_operation_elapsed_ns: operation_elapsed_ns[0],
        strict_unmount_lifecycle_elapsed_ns: lifecycle_elapsed_ns[1],
        strict_unmount_operation_elapsed_ns: operation_elapsed_ns[1],
    })
    .map_err(PocError::Integrity)?;

    let invocation = &invocations[2];
    let mut lifecycle = PlatformStorageLifecycle::measured_with_publication_authority(
        process_evidence,
        selections[2].clone(),
        invocation.mount_receipt_binding.clone(),
        &publication_authority,
    )?;
    receipts.push(run_publication_sequence_action(
        &selections[2],
        &operation_paths[2],
        sequence_resumed,
        sequence_started_unix_ms,
        &mut lifecycle,
    )?);
    Ok(receipts)
}

fn run_publication_sequence_action<L: StorageAdminLifecycle>(
    selection: &StorageAdminSelection,
    paths: &OperationPaths,
    sequence_resumed: bool,
    started_unix_ms: u64,
    lifecycle: &mut L,
) -> PocResult<StorageAdminReceipt> {
    if paths.receipt.exists() {
        let mut receipt: StorageAdminReceipt = read_json(&paths.receipt)?;
        validate_stored_receipt(&receipt, selection, &paths.receipt)?;
        receipt.idempotent_replay = true;
        return Ok(receipt);
    }

    let execution = if sequence_resumed {
        lifecycle.recover_incomplete(selection.request.action, &selection.request.scope)
    } else {
        lifecycle.execute(selection.request.action, &selection.request.scope)
    };
    commit_execution(
        selection,
        lifecycle,
        execution,
        started_unix_ms,
        &paths.receipt_validation_diagnostic,
        &paths.receipt,
        None,
        false,
    )
}

fn prepare_publication_sequence_store(
    invocations: &[StorageAdminInvocation],
    selections: &[StorageAdminSelection],
) -> PocResult<(Vec<OperationPaths>, bool, u64, FileLock)> {
    if invocations.len() != 3 || selections.len() != invocations.len() {
        return Err(PocError::Integrity(
            "publication sequence store requires exactly three authorized invocations".to_owned(),
        ));
    }
    let operation_paths = selections
        .iter()
        .map(|selection| operation_paths(&selection.request))
        .collect::<PocResult<Vec<_>>>()?;
    let root = operation_paths
        .first()
        .and_then(|paths| paths.lock.parent())
        .ok_or_else(|| PocError::Integrity("storage-admin lock has no parent".to_owned()))?;
    if operation_paths
        .iter()
        .any(|paths| paths.lock.parent() != Some(root))
    {
        return Err(PocError::Integrity(
            "publication sequence attempts span multiple storage-admin roots".to_owned(),
        ));
    }

    let attempts_path =
        publication_sequence_attempts_path(root, &selections[2].request.operation_id)?;
    let sequence_directory = attempts_path.parent().ok_or_else(|| {
        PocError::Integrity("publication sequence attempts have no parent directory".to_owned())
    })?;
    for paths in &operation_paths {
        fs::create_dir_all(&paths.directory).map_err(|error| {
            PocError::io(
                "create publication storage-admin operation directory",
                &paths.directory,
                error,
            )
        })?;
    }
    fs::create_dir_all(&sequence_directory).map_err(|error| {
        PocError::io(
            "create publication sequence authorization directory",
            &sequence_directory,
            error,
        )
    })?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&operation_paths[0].lock)
    {
        Ok(file) => file.sync_all().map_err(|error| {
            PocError::io(
                "fsync storage-admin publication lock",
                &operation_paths[0].lock,
                error,
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(PocError::io(
                "create storage-admin publication lock",
                &operation_paths[0].lock,
                error,
            ));
        }
    }
    // This one parent sync durably installs the lock, the fixed sequence
    // directory, and all three canonical receipt directories.
    crate::durable::fsync_dir(root)?;
    let lock = FileLock::exclusive(&operation_paths[0].lock)?;

    if attempts_path.exists() {
        let attempts: PublicationSequenceAttempts = read_json(&attempts_path)?;
        validate_publication_sequence_attempts(&attempts, selections, invocations)?;
        return Ok((
            operation_paths,
            true,
            attempts.attempts[0].started_unix_ms,
            lock,
        ));
    }
    if operation_paths.iter().any(|paths| {
        paths.attempt.exists()
            || paths.receipt.exists()
            || paths.receipt_validation_diagnostic.exists()
    }) {
        return Err(PocError::RecoveryRequired(
            "publication sequence cannot replace an existing per-action storage record".to_owned(),
        ));
    }

    let started_unix_ms = unix_time_ms()?;
    let attempts = PublicationSequenceAttempts {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        attempts: selections
            .iter()
            .zip(invocations)
            .map(|(selection, invocation)| StorageAdminAttempt {
                schema_version: SCHEMA_VERSION,
                interface_version: INTERFACE_VERSION.to_owned(),
                operation_id: selection.request.operation_id.clone(),
                request_sha256: selection.request_sha256.clone(),
                request: selection.request.clone(),
                authorization: invocation.authorization.clone(),
                durability: invocation.durability,
                trusted_executable_sha256: invocation.trusted_executable_sha256.clone(),
                workload_cgroup_procs: invocation.workload_cgroup_procs.clone(),
                mount_namespace_holder_pid: invocation.mount_namespace_holder_pid,
                mount_receipt_binding: invocation.mount_receipt_binding.clone(),
                started_unix_ms,
            })
            .collect(),
    };
    write_immutable_json(&attempts_path, &attempts)?;
    Ok((operation_paths, false, started_unix_ms, lock))
}

fn publication_sequence_attempts_path(
    root: &Path,
    cleanup_operation_id: &OperationId,
) -> PocResult<PathBuf> {
    validate_path_atom(
        "publication cleanup operation id",
        cleanup_operation_id.as_str(),
    )?;
    Ok(root
        .join(PUBLICATION_SEQUENCE_DIRECTORY)
        .join(cleanup_operation_id.as_str())
        .join(PUBLICATION_SEQUENCE_ATTEMPTS_FILE))
}

fn validate_publication_sequence_attempts(
    attempts: &PublicationSequenceAttempts,
    selections: &[StorageAdminSelection],
    invocations: &[StorageAdminInvocation],
) -> PocResult<()> {
    require_equal(
        "publication sequence attempt schema version",
        &attempts.schema_version,
        &SCHEMA_VERSION,
    )?;
    require_equal(
        "publication sequence attempt interface version",
        attempts.interface_version.as_str(),
        INTERFACE_VERSION,
    )?;
    require_equal(
        "publication sequence attempt count",
        &attempts.attempts.len(),
        &selections.len(),
    )?;
    if selections.len() != invocations.len() {
        return Err(PocError::Integrity(
            "publication sequence authorization inputs have inconsistent lengths".to_owned(),
        ));
    }
    for ((attempt, selection), invocation) in
        attempts.attempts.iter().zip(selections).zip(invocations)
    {
        validate_stored_attempt(attempt, selection, invocation)?;
    }
    Ok(())
}

#[cfg(test)]
mod publication_sequence_store_tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::{AllocationId, RunId, SessionId};

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

    fn publication_invocations(root: &Path) -> Vec<StorageAdminInvocation> {
        let scope = StorageAdminScope {
            run_id: RunId::parse("m2r-20260728T015724p0800").expect("valid run id"),
            sandbox_id: "sandbox-sequence-store".to_owned(),
            workspace_session_id: "workspace-sequence-store".to_owned(),
            session_id: SessionId::from_string("session-sequence-store"),
            allocation_id: AllocationId::from_string("allocation-sequence-store"),
            lease_id: "m2r-lease-sequence-store:7".to_owned(),
            lease_epoch: 7,
            mount_namespace_id: "mnt:[4026532999]".to_owned(),
            payload_root: root.join("payload"),
            control_root: root.join("control"),
            lower_dirs_newest_first: vec![root.join("payload/lower-1")],
            allocation_root: root.join("allocation"),
            workspace_root: root.join("workspace"),
        };
        [
            ("sequence-quiesce", StorageAdminAction::Quiesce),
            ("sequence-unmount", StorageAdminAction::StrictUnmount),
            ("sequence-cleanup", StorageAdminAction::Cleanup),
        ]
        .into_iter()
        .map(|(operation_id, action)| {
            let request = StorageAdminRequest {
                schema_version: SCHEMA_VERSION,
                interface_version: INTERFACE_VERSION.to_owned(),
                profile_id: STORAGE_ADMIN_PROFILE_ID.to_owned(),
                operation_id: OperationId::from_string(operation_id),
                action,
                scope: scope.clone(),
            };
            let authorization = StorageAdminAuthorization {
                authenticated: true,
                actor_id: "mpla-sequence-store-test".to_owned(),
                operation_id: request.operation_id.clone(),
                run_id: scope.run_id.clone(),
                sandbox_id: scope.sandbox_id.clone(),
                workspace_session_id: scope.workspace_session_id.clone(),
                session_id: scope.session_id.clone(),
                allocation_id: scope.allocation_id.clone(),
                lease_id: scope.lease_id.clone(),
                lease_epoch: scope.lease_epoch,
                mount_namespace_id: scope.mount_namespace_id.clone(),
            };
            StorageAdminInvocation {
                expected_request: request.clone(),
                request,
                authorization,
                trusted_actor_id: "mpla-sequence-store-test".to_owned(),
                durability: StorageAdminDurability::ExactObjectGraph,
                trusted_executable_sha256: "00".repeat(32),
                workload_cgroup_procs: root.join("workload/cgroup.procs"),
                mount_namespace_holder_pid: 4_242,
                mount_receipt_binding: Some(StorageAdminMountReceiptBinding {
                    storage_operation_id: OperationId::from_string("sequence-mount"),
                    attestation_sha256: "11".repeat(32),
                }),
            }
        })
        .collect()
    }

    fn selections(invocations: &[StorageAdminInvocation]) -> Vec<StorageAdminSelection> {
        invocations
            .iter()
            .map(|invocation| {
                authorize_storage_admin(
                    &invocation.expected_request,
                    &invocation.request,
                    &invocation.authorization,
                    &invocation.trusted_actor_id,
                )
                .expect("authorized fixed sequence invocation")
            })
            .collect()
    }

    #[test]
    fn sequence_attempt_header_reopens_after_a_crash_point_and_rejects_substitution() {
        let root = std::env::temp_dir().join(format!(
            "mpla-publication-sequence-store-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).expect("create sequence test root");
        let invocations = publication_invocations(&root);
        let selections = selections(&invocations);

        let attempts_path = publication_sequence_attempts_path(
            &root.join("control").join(STORAGE_ADMIN_DIRECTORY),
            &invocations[2].request.operation_id,
        )
        .expect("sequence attempts path");
        {
            let (paths, resumed, _started, _lock) =
                prepare_publication_sequence_store(&invocations, &selections)
                    .expect("durably authorize fixed sequence before first action");
            assert!(!resumed);
            assert!(attempts_path.exists());
            // Simulate the first receipt having been committed before the
            // helper crashes. A later helper must accept the header and carry
            // on with recovery instead of treating the receipt as a legacy
            // collision.
            fs::write(&paths[0].receipt, b"receipt-created-before-crash\n")
                .expect("create crash-point receipt marker");
        }

        {
            let (_paths, resumed, _started, _lock) =
                prepare_publication_sequence_store(&invocations, &selections)
                    .expect("reopen exact durable sequence authorization");
            assert!(resumed);
        }

        let mut substituted = invocations.clone();
        substituted[1].trusted_executable_sha256 = "22".repeat(32);
        assert!(prepare_publication_sequence_store(&substituted, &selections).is_err());

        fs::remove_dir_all(root).expect("remove sequence test root");
    }

    #[test]
    fn legacy_cleanup_authority_remains_valid_without_compact_header() {
        let root = std::env::temp_dir().join(format!(
            "mpla-publication-sequence-legacy-authority-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).expect("create legacy authority test root");
        let invocations = publication_invocations(&root);
        let cleanup_invocation = &invocations[2];
        let cleanup_selection = authorize_storage_admin(
            &cleanup_invocation.expected_request,
            &cleanup_invocation.request,
            &cleanup_invocation.authorization,
            &cleanup_invocation.trusted_actor_id,
        )
        .expect("authorize exact legacy cleanup invocation");
        let cleanup_paths =
            operation_paths(&cleanup_selection.request).expect("derive legacy cleanup paths");
        fs::create_dir_all(&cleanup_paths.directory).expect("create legacy cleanup directory");
        write_immutable_json(
            &cleanup_paths.attempt,
            &StorageAdminAttempt {
                schema_version: SCHEMA_VERSION,
                interface_version: INTERFACE_VERSION.to_owned(),
                operation_id: cleanup_selection.request.operation_id.clone(),
                request_sha256: cleanup_selection.request_sha256.clone(),
                request: cleanup_selection.request.clone(),
                authorization: cleanup_invocation.authorization.clone(),
                durability: cleanup_invocation.durability,
                trusted_executable_sha256: cleanup_invocation.trusted_executable_sha256.clone(),
                workload_cgroup_procs: cleanup_invocation.workload_cgroup_procs.clone(),
                mount_namespace_holder_pid: cleanup_invocation.mount_namespace_holder_pid,
                mount_receipt_binding: cleanup_invocation.mount_receipt_binding.clone(),
                started_unix_ms: 1,
            },
        )
        .expect("write immutable legacy cleanup attempt");

        validate_publication_sequence_cleanup_attempt(&cleanup_paths, &cleanup_selection)
            .expect("exact legacy cleanup authority remains accepted");

        fs::remove_dir_all(root).expect("remove legacy authority test root");
    }

    fn write_legacy_cleanup_attempt(
        root: &Path,
        request_sha256: String,
    ) -> (OperationPaths, StorageAdminSelection) {
        let invocations = publication_invocations(root);
        let cleanup_invocation = &invocations[2];
        let cleanup_selection = authorize_storage_admin(
            &cleanup_invocation.expected_request,
            &cleanup_invocation.request,
            &cleanup_invocation.authorization,
            &cleanup_invocation.trusted_actor_id,
        )
        .expect("authorize exact legacy cleanup invocation");
        let cleanup_paths =
            operation_paths(&cleanup_selection.request).expect("derive legacy cleanup paths");
        fs::create_dir_all(&cleanup_paths.directory).expect("create legacy cleanup directory");
        write_immutable_json(
            &cleanup_paths.attempt,
            &StorageAdminAttempt {
                schema_version: SCHEMA_VERSION,
                interface_version: INTERFACE_VERSION.to_owned(),
                operation_id: cleanup_selection.request.operation_id.clone(),
                request_sha256,
                request: cleanup_selection.request.clone(),
                authorization: cleanup_invocation.authorization.clone(),
                durability: cleanup_invocation.durability,
                trusted_executable_sha256: cleanup_invocation.trusted_executable_sha256.clone(),
                workload_cgroup_procs: cleanup_invocation.workload_cgroup_procs.clone(),
                mount_namespace_holder_pid: cleanup_invocation.mount_namespace_holder_pid,
                mount_receipt_binding: cleanup_invocation.mount_receipt_binding.clone(),
                started_unix_ms: 1,
            },
        )
        .expect("write immutable legacy cleanup attempt");
        (cleanup_paths, cleanup_selection)
    }

    #[test]
    fn legacy_cleanup_authority_rejects_a_tampered_request_digest() {
        let root = std::env::temp_dir().join(format!(
            "mpla-publication-sequence-legacy-tamper-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).expect("create legacy tamper test root");
        let (cleanup_paths, cleanup_selection) =
            write_legacy_cleanup_attempt(&root, "ff".repeat(32));

        assert!(
            validate_publication_sequence_cleanup_attempt(&cleanup_paths, &cleanup_selection)
                .is_err()
        );

        fs::remove_dir_all(root).expect("remove legacy tamper test root");
    }

    #[test]
    fn malformed_compact_header_never_downgrades_to_legacy_authority() {
        let root = std::env::temp_dir().join(format!(
            "mpla-publication-sequence-compact-tamper-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).expect("create compact tamper test root");
        let request_sha256 = request_sha256(&publication_invocations(&root)[2].request)
            .expect("derive exact cleanup request digest");
        let (cleanup_paths, cleanup_selection) =
            write_legacy_cleanup_attempt(&root, request_sha256);
        let storage_admin_root = cleanup_paths
            .lock
            .parent()
            .expect("storage-admin root for cleanup path");
        let attempts_path = publication_sequence_attempts_path(
            storage_admin_root,
            &cleanup_selection.request.operation_id,
        )
        .expect("derive compact attempts path");
        fs::create_dir_all(attempts_path.parent().expect("compact attempts parent"))
            .expect("create compact attempts parent");
        fs::write(&attempts_path, b"{").expect("write malformed compact attempts header");

        assert!(
            validate_publication_sequence_cleanup_attempt(&cleanup_paths, &cleanup_selection)
                .is_err()
        );

        fs::remove_dir_all(root).expect("remove compact tamper test root");
    }
}

fn commit_execution<L: StorageAdminLifecycle>(
    selection: &StorageAdminSelection,
    lifecycle: &mut L,
    execution: StorageAdminExecution,
    started_unix_ms: u64,
    receipt_validation_diagnostic_path: &Path,
    receipt_path: &Path,
    durability_batch: Option<DurabilityBatch>,
    discard_durability_batch: bool,
) -> PocResult<StorageAdminReceipt> {
    validate_execution(&execution)?;
    let completed_unix_ms = unix_time_ms()?.max(started_unix_ms);
    let (process_evidence, mount_plan_evidence) =
        lifecycle.authority_evidence(&selection.request.scope)?;
    let (mount_attestation, mount_receipt_binding) =
        lifecycle.mount_authority_evidence(selection, &process_evidence, &mount_plan_evidence)?;
    if let Err(error) = validate_receipt_authority_evidence(
        &process_evidence,
        &mount_plan_evidence,
        &selection.request.scope,
        selection.profile(),
    ) {
        return fail_receipt_validation(
            selection,
            lifecycle,
            &mount_plan_evidence,
            receipt_validation_diagnostic_path,
            error,
        );
    }
    if let Err(error) = validate_mount_authority_evidence(
        selection,
        &process_evidence,
        &mount_plan_evidence,
        execution.outcome,
        mount_attestation.as_ref(),
        mount_receipt_binding.as_ref(),
    ) {
        return fail_receipt_validation(
            selection,
            lifecycle,
            &mount_plan_evidence,
            receipt_validation_diagnostic_path,
            error,
        );
    }
    let receipt = StorageAdminReceipt {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        profile_id: selection.profile_id().to_owned(),
        operation_id: selection.request.operation_id.clone(),
        action: selection.request.action,
        request_sha256: selection.request_sha256.clone(),
        trusted_executable: PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
        effective_capabilities: selection
            .profile()
            .effective_capabilities()
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect(),
        allowed_privileged_syscalls: STORAGE_ADMIN_PRIVILEGED_SYSCALLS
            .iter()
            .map(|syscall| (*syscall).to_owned())
            .collect(),
        process_evidence,
        mount_plan_evidence,
        mount_attestation,
        mount_receipt_binding,
        scope: selection.request.scope.clone(),
        outcome: execution.outcome,
        idempotent_replay: false,
        cleanup_complete: execution.cleanup_complete,
        failure: execution.failure,
        started_unix_ms,
        completed_unix_ms,
        receipt_path: receipt_path.to_path_buf(),
    };
    if let Err(error) = write_immutable_json(receipt_path, &receipt) {
        let cleanup = lifecycle
            .cleanup_after_receipt_failure(selection.request.action, &selection.request.scope);
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(PocError::RecoveryRequired(format!(
                "receipt commit failed: {error}; cleanup failed: {cleanup_error}"
            ))),
        };
    }
    if let Some(batch) = durability_batch {
        let durability_result = if discard_durability_batch {
            batch.discard();
            Ok(())
        } else {
            batch.commit(&[&selection.request.scope.control_root])
        };
        if let Err(error) = durability_result {
            let cleanup = lifecycle
                .cleanup_after_receipt_failure(selection.request.action, &selection.request.scope);
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(PocError::RecoveryRequired(format!(
                    "receipt durability commit failed: {error}; cleanup failed: {cleanup_error}"
                ))),
            };
        }
    }
    lifecycle.receipt_committed(selection.request.action, &selection.request.scope);
    Ok(receipt)
}

fn fail_receipt_validation<L: StorageAdminLifecycle>(
    selection: &StorageAdminSelection,
    lifecycle: &mut L,
    mount_plan_evidence: &StorageAdminMountPlanEvidence,
    diagnostic_path: &Path,
    validation_error: PocError,
) -> PocResult<StorageAdminReceipt> {
    let diagnostic = receipt_validation_diagnostic(selection, mount_plan_evidence)?;
    let rendered_diagnostic = diagnostic.as_ref().map(serde_json::to_string).transpose()?;
    if let Some(diagnostic) = diagnostic.as_ref() {
        if let Err(diagnostic_error) = write_immutable_json(diagnostic_path, diagnostic) {
            let cleanup = lifecycle
                .cleanup_after_receipt_failure(selection.request.action, &selection.request.scope);
            return match cleanup {
                Ok(()) => Err(PocError::RecoveryRequired(format!(
                    "receipt validation failed: {validation_error}; receipt-validation diagnostic persistence failed: {diagnostic_error}"
                ))),
                Err(cleanup_error) => Err(PocError::RecoveryRequired(format!(
                    "receipt validation failed: {validation_error}; receipt-validation diagnostic persistence failed: {diagnostic_error}; cleanup failed: {cleanup_error}"
                ))),
            };
        }
    }
    if let Err(cleanup_error) =
        lifecycle.cleanup_after_receipt_failure(selection.request.action, &selection.request.scope)
    {
        return Err(PocError::RecoveryRequired(format!(
            "receipt validation failed: {validation_error}; cleanup failed: {cleanup_error}"
        )));
    }
    match rendered_diagnostic {
        Some(diagnostic) => Err(PocError::Integrity(format!(
            "{validation_error}; storage-admin receipt-validation diagnostic={diagnostic}"
        ))),
        None => Err(validation_error),
    }
}

fn receipt_validation_diagnostic(
    selection: &StorageAdminSelection,
    mount_plan_evidence: &StorageAdminMountPlanEvidence,
) -> PocResult<Option<StorageAdminReceiptValidationDiagnostic>> {
    let Some(target) = mount_plan_evidence.mountinfo_after.target.as_ref() else {
        return Ok(None);
    };
    validate_sha256(
        "receipt diagnostic mountinfo hash",
        &mount_plan_evidence.mountinfo_after.sha256,
    )?;
    if target.mount_options.len() > MAX_RECEIPT_DIAGNOSTIC_MOUNT_OPTIONS {
        return Err(PocError::Integrity(
            "storage-admin receipt diagnostic mount option count exceeds its bounded budget"
                .to_owned(),
        ));
    }
    Ok(Some(StorageAdminReceiptValidationDiagnostic {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        operation_id: selection.request.operation_id.clone(),
        request_sha256: selection.request_sha256.clone(),
        mountinfo_sha256: bounded_receipt_diagnostic_field(
            "mountinfo hash",
            &mount_plan_evidence.mountinfo_after.sha256,
        )?,
        filesystem_type: bounded_receipt_diagnostic_field(
            "filesystem type",
            &target.filesystem_type,
        )?,
        parsed_source: bounded_receipt_diagnostic_field("parsed source", &target.source)?,
        mount_options: target
            .mount_options
            .iter()
            .map(|option| bounded_receipt_diagnostic_field("mount option", option))
            .collect::<PocResult<Vec<_>>>()?,
        trusted_expected_source: bounded_receipt_diagnostic_field(
            "trusted expected source",
            &mount_plan_evidence.source,
        )?,
    }))
}

fn bounded_receipt_diagnostic_field(label: &str, value: &str) -> PocResult<String> {
    if value.len() > MAX_RECEIPT_DIAGNOSTIC_FIELD_BYTES {
        return Err(PocError::Integrity(format!(
            "storage-admin receipt diagnostic {label} exceeds its bounded budget"
        )));
    }
    Ok(value.to_owned())
}

fn validate_request(request: &StorageAdminRequest) -> PocResult<StorageAdminCapabilityProfile> {
    require_equal("schema version", &request.schema_version, &SCHEMA_VERSION)?;
    require_equal(
        "interface version",
        request.interface_version.as_str(),
        INTERFACE_VERSION,
    )?;
    let profile = StorageAdminCapabilityProfile::from_profile_id(&request.profile_id)?;
    validate_path_atom("operation id", request.operation_id.as_str())?;
    validate_scope(&request.scope)?;
    Ok(profile)
}

fn validate_scope(scope: &StorageAdminScope) -> PocResult<()> {
    validate_text("sandbox id", &scope.sandbox_id)?;
    validate_text("workspace session id", &scope.workspace_session_id)?;
    validate_path_atom("MPLA session id", scope.session_id.as_str())?;
    validate_path_atom("allocation id", scope.allocation_id.as_str())?;
    validate_text("lease id", &scope.lease_id)?;
    if scope.lease_epoch == 0 {
        return Err(PocError::Integrity(
            "storage-admin lease epoch must be non-zero".to_owned(),
        ));
    }
    validate_mount_namespace_id(&scope.mount_namespace_id)?;

    let named_paths = [
        ("payload root", scope.payload_root.as_path()),
        ("control root", scope.control_root.as_path()),
        ("allocation root", scope.allocation_root.as_path()),
        ("workspace root", scope.workspace_root.as_path()),
    ];
    for (label, path) in named_paths {
        validate_absolute_normalized_path(label, path)?;
    }
    if scope.lower_dirs_newest_first.is_empty() {
        return Err(PocError::Integrity(
            "storage-admin lower directory set must not be empty".to_owned(),
        ));
    }
    let mut lower_dirs = BTreeSet::new();
    for path in &scope.lower_dirs_newest_first {
        validate_absolute_normalized_path("lower directory", path)?;
        if !lower_dirs.insert(path) {
            return Err(PocError::Integrity(format!(
                "storage-admin lower directory is duplicated: {}",
                path.display()
            )));
        }
    }
    let allowed_paths = [
        scope.payload_root.as_path(),
        scope.control_root.as_path(),
        scope.allocation_root.as_path(),
        scope.workspace_root.as_path(),
    ];
    if allowed_paths
        .iter()
        .enumerate()
        .any(|(index, path)| allowed_paths[..index].contains(path))
    {
        return Err(PocError::Integrity(
            "storage-admin named allowed roots must be distinct".to_owned(),
        ));
    }
    if lower_dirs.contains(&scope.workspace_root) {
        return Err(PocError::Integrity(
            "storage-admin workspace root cannot also be a lower directory".to_owned(),
        ));
    }
    Ok(())
}

fn validate_exact_request(
    expected: &StorageAdminRequest,
    request: &StorageAdminRequest,
) -> PocResult<()> {
    require_equal(
        "schema version",
        &request.schema_version,
        &expected.schema_version,
    )?;
    require_equal(
        "interface version",
        &request.interface_version,
        &expected.interface_version,
    )?;
    require_equal("profile id", &request.profile_id, &expected.profile_id)?;
    require_equal(
        "operation id",
        &request.operation_id,
        &expected.operation_id,
    )?;
    require_equal("lifecycle action", &request.action, &expected.action)?;
    require_equal("run id", &request.scope.run_id, &expected.scope.run_id)?;
    require_equal(
        "sandbox id",
        &request.scope.sandbox_id,
        &expected.scope.sandbox_id,
    )?;
    require_equal(
        "workspace session id",
        &request.scope.workspace_session_id,
        &expected.scope.workspace_session_id,
    )?;
    require_equal(
        "MPLA session id",
        &request.scope.session_id,
        &expected.scope.session_id,
    )?;
    require_equal(
        "allocation id",
        &request.scope.allocation_id,
        &expected.scope.allocation_id,
    )?;
    require_equal(
        "lease id",
        &request.scope.lease_id,
        &expected.scope.lease_id,
    )?;
    require_equal(
        "lease epoch",
        &request.scope.lease_epoch,
        &expected.scope.lease_epoch,
    )?;
    require_equal(
        "mount namespace id",
        &request.scope.mount_namespace_id,
        &expected.scope.mount_namespace_id,
    )?;
    require_equal(
        "payload root",
        &request.scope.payload_root,
        &expected.scope.payload_root,
    )?;
    require_equal(
        "control root",
        &request.scope.control_root,
        &expected.scope.control_root,
    )?;
    require_equal(
        "lower directories",
        &request.scope.lower_dirs_newest_first,
        &expected.scope.lower_dirs_newest_first,
    )?;
    require_equal(
        "allocation root",
        &request.scope.allocation_root,
        &expected.scope.allocation_root,
    )?;
    require_equal(
        "workspace root",
        &request.scope.workspace_root,
        &expected.scope.workspace_root,
    )
}

fn validate_authorization(
    request: &StorageAdminRequest,
    authorization: &StorageAdminAuthorization,
    trusted_actor_id: &str,
) -> PocResult<()> {
    validate_text("trusted actor id", trusted_actor_id)?;
    if !authorization.authenticated {
        return Err(PocError::Integrity(
            "storage-admin authorization is not authenticated".to_owned(),
        ));
    }
    require_equal(
        "authorization actor id",
        authorization.actor_id.as_str(),
        trusted_actor_id,
    )?;
    require_equal(
        "authorization operation id",
        &authorization.operation_id,
        &request.operation_id,
    )?;
    require_equal(
        "authorization run id",
        &authorization.run_id,
        &request.scope.run_id,
    )?;
    require_equal(
        "authorization sandbox id",
        &authorization.sandbox_id,
        &request.scope.sandbox_id,
    )?;
    require_equal(
        "authorization workspace session id",
        &authorization.workspace_session_id,
        &request.scope.workspace_session_id,
    )?;
    require_equal(
        "authorization MPLA session id",
        &authorization.session_id,
        &request.scope.session_id,
    )?;
    require_equal(
        "authorization allocation id",
        &authorization.allocation_id,
        &request.scope.allocation_id,
    )?;
    require_equal(
        "authorization lease id",
        &authorization.lease_id,
        &request.scope.lease_id,
    )?;
    require_equal(
        "authorization lease epoch",
        &authorization.lease_epoch,
        &request.scope.lease_epoch,
    )?;
    require_equal(
        "authorization mount namespace id",
        &authorization.mount_namespace_id,
        &request.scope.mount_namespace_id,
    )
}

fn validate_execution(execution: &StorageAdminExecution) -> PocResult<()> {
    match execution.outcome {
        StorageAdminOutcome::Succeeded if execution.failure.is_none() => Ok(()),
        StorageAdminOutcome::Succeeded => Err(PocError::Integrity(
            "successful storage-admin execution cannot contain a failure".to_owned(),
        )),
        StorageAdminOutcome::Failed | StorageAdminOutcome::Cancelled
            if execution
                .failure
                .as_deref()
                .is_some_and(|failure| !failure.is_empty()) =>
        {
            Ok(())
        }
        StorageAdminOutcome::Failed | StorageAdminOutcome::Cancelled => Err(PocError::Integrity(
            "failed or cancelled storage-admin execution must explain the failure".to_owned(),
        )),
    }
}

fn request_sha256(request: &StorageAdminRequest) -> PocResult<String> {
    let bytes = serde_json::to_vec(request)?;
    let digest = Sha256::digest(bytes);
    Ok(hex_digest(&digest))
}

pub fn storage_admin_mount_attestation_sha256(
    attestation: &StorageAdminMountAttestation,
) -> PocResult<String> {
    let bytes = serde_json::to_vec(attestation)?;
    Ok(hex_digest(&Sha256::digest(bytes)))
}

pub fn storage_admin_mountinfo_target_sha256(
    target: Option<&StorageAdminObservedMount>,
) -> PocResult<String> {
    let bytes = serde_json::to_vec(&target)?;
    Ok(hex_digest(&Sha256::digest(bytes)))
}

pub fn storage_admin_authorized_path_sha256(path: &Path) -> String {
    #[cfg(target_family = "unix")]
    let bytes = path.as_os_str().as_bytes();
    #[cfg(not(target_family = "unix"))]
    let bytes = path.to_string_lossy().as_bytes();
    hex_digest(&Sha256::digest(bytes))
}

fn storage_admin_seccomp_profile_sha256() -> String {
    hex_digest(&Sha256::digest(STORAGE_ADMIN_SECCOMP_PROFILE_CANONICAL))
}

fn validate_sha256(label: &str, value: &str) -> PocResult<()> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "storage-admin {label} is not a SHA-256 hex digest"
        )))
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

fn validate_mount_receipt_binding_for_action(
    action: StorageAdminAction,
    binding: Option<&StorageAdminMountReceiptBinding>,
) -> PocResult<()> {
    match (action, binding) {
        (StorageAdminAction::Mount, None) => Ok(()),
        (StorageAdminAction::Mount, Some(_)) => Err(PocError::Integrity(
            "mount request cannot supply prior mount authority".to_owned(),
        )),
        (_, Some(binding)) => {
            validate_path_atom(
                "mount receipt operation id",
                binding.storage_operation_id.as_str(),
            )?;
            validate_sha256(
                "mount receipt attestation digest",
                &binding.attestation_sha256,
            )
        }
        (_, None) => Err(PocError::Integrity(
            "storage-admin lifecycle action is missing mount receipt authority".to_owned(),
        )),
    }
}

fn validate_mount_authority_evidence(
    selection: &StorageAdminSelection,
    process: &StorageAdminProcessEvidence,
    mount_plan: &StorageAdminMountPlanEvidence,
    outcome: StorageAdminOutcome,
    attestation: Option<&StorageAdminMountAttestation>,
    binding: Option<&StorageAdminMountReceiptBinding>,
) -> PocResult<()> {
    if outcome != StorageAdminOutcome::Succeeded {
        return Ok(());
    }
    match selection.request.action {
        StorageAdminAction::Mount => {
            let attestation = attestation.ok_or_else(|| {
                PocError::Integrity(
                    "successful mount is missing durable lower-binding attestation".to_owned(),
                )
            })?;
            validate_mount_attestation(attestation, selection, process, mount_plan)?;
            let digest = storage_admin_mount_attestation_sha256(attestation)?;
            let expected = StorageAdminMountReceiptBinding {
                storage_operation_id: selection.request.operation_id.clone(),
                attestation_sha256: digest,
            };
            require_equal(
                "mount receipt binding",
                binding.ok_or_else(|| {
                    PocError::Integrity(
                        "successful mount is missing its receipt binding".to_owned(),
                    )
                })?,
                &expected,
            )
        }
        _ => {
            if attestation.is_some() {
                return Err(PocError::Integrity(
                    "later lifecycle receipt cannot replace the mount attestation".to_owned(),
                ));
            }
            validate_mount_receipt_binding_for_action(selection.request.action, binding)
        }
    }
}

fn validate_mount_attestation(
    attestation: &StorageAdminMountAttestation,
    selection: &StorageAdminSelection,
    process: &StorageAdminProcessEvidence,
    mount_plan: &StorageAdminMountPlanEvidence,
) -> PocResult<()> {
    let scope = &selection.request.scope;
    require_equal(
        "mount attestation schema version",
        &attestation.schema_version,
        &SCHEMA_VERSION,
    )?;
    require_equal(
        "mount attestation run id",
        &attestation.run_id,
        &scope.run_id,
    )?;
    require_equal(
        "mount attestation sandbox id",
        &attestation.sandbox_id,
        &scope.sandbox_id,
    )?;
    require_equal(
        "mount attestation workspace session id",
        &attestation.workspace_session_id,
        &scope.workspace_session_id,
    )?;
    require_equal(
        "mount attestation session id",
        &attestation.session_id,
        &scope.session_id,
    )?;
    require_equal(
        "mount attestation allocation id",
        &attestation.allocation_id,
        &scope.allocation_id,
    )?;
    require_equal(
        "mount attestation lease id",
        &attestation.lease_id,
        &scope.lease_id,
    )?;
    require_equal(
        "mount attestation lease epoch",
        &attestation.lease_epoch,
        &scope.lease_epoch,
    )?;
    require_equal(
        "mount attestation namespace",
        &attestation.mount_namespace_id,
        &scope.mount_namespace_id,
    )?;
    require_equal(
        "mount attestation namespace inode",
        &attestation.mount_namespace_inode,
        &process.mount_namespace_inode,
    )?;
    require_equal(
        "mount attestation operation id",
        &attestation.storage_operation_id,
        &selection.request.operation_id,
    )?;
    require_equal(
        "mount attestation request digest",
        &attestation.request_sha256,
        &selection.request_sha256,
    )?;
    require_equal(
        "mount attestation profile",
        attestation.profile_id.as_str(),
        selection.profile_id(),
    )?;
    require_equal(
        "mount attestation effective capabilities",
        &attestation.effective_capabilities,
        &owned_strings(selection.profile().effective_capabilities()),
    )?;
    if attestation.lower_bindings_newest_first.len() != scope.lower_dirs_newest_first.len() {
        return Err(PocError::Integrity(
            "mount attestation lower stack length does not match trusted binding".to_owned(),
        ));
    }
    for (index, (binding, path)) in attestation
        .lower_bindings_newest_first
        .iter()
        .zip(&scope.lower_dirs_newest_first)
        .enumerate()
    {
        require_equal("mount attestation lower index", &binding.index, &index)?;
        require_equal(
            "mount attestation lower path proof",
            &binding.authorized_path_sha256,
            &storage_admin_authorized_path_sha256(path),
        )?;
        validate_sha256(
            "mount attestation lower path proof",
            &binding.authorized_path_sha256,
        )?;
        require_equal(
            "mount attestation opened lower identity",
            &binding.fd_identity,
            &binding.authorized_path_identity,
        )?;
        validate_path_identity("mount attestation lower", &binding.fd_identity)?;
    }
    let observed = mount_plan.mountinfo_after.target.as_ref().ok_or_else(|| {
        PocError::Integrity("mount attestation has no attached workspace target".to_owned())
    })?;
    let target = &attestation.target;
    require_equal(
        "mount attestation workspace target",
        &target.workspace_target,
        &scope.workspace_root,
    )?;
    require_equal(
        "mount attestation target namespace",
        &target.mount_namespace_id,
        &scope.mount_namespace_id,
    )?;
    require_equal(
        "mount attestation target namespace inode",
        &target.mount_namespace_inode,
        &process.mount_namespace_inode,
    )?;
    require_equal(
        "mount attestation mount id",
        &target.mount_id,
        &observed.mount_id,
    )?;
    require_equal(
        "mount attestation target mountinfo digest",
        &target.mountinfo_sha256,
        &mount_plan.mountinfo_after.sha256,
    )?;
    require_equal(
        "mount attestation filesystem",
        &target.filesystem_type,
        &observed.filesystem_type,
    )?;
    require_equal("mount attestation source", &target.source, &observed.source)?;
    require_equal(
        "mount attestation mount options",
        &target.mount_options,
        &observed.mount_options,
    )?;
    require_equal(
        "mount attestation super options",
        &target.super_options,
        &observed.super_options,
    )?;
    require_equal(
        "mount attestation expected upperdir",
        &target.expected_upperdir_sha256,
        &storage_admin_authorized_path_sha256(&mount_plan.upper_dir),
    )?;
    require_equal(
        "mount attestation observed upperdir",
        &target.observed_upperdir_sha256,
        &storage_admin_authorized_path_sha256(observed.upper_dir.as_deref().ok_or_else(|| {
            PocError::Integrity("mount attestation observed upperdir is missing".to_owned())
        })?),
    )?;
    require_equal(
        "mount attestation expected workdir",
        &target.expected_workdir_sha256,
        &storage_admin_authorized_path_sha256(&mount_plan.work_dir),
    )?;
    require_equal(
        "mount attestation observed workdir",
        &target.observed_workdir_sha256,
        &storage_admin_authorized_path_sha256(observed.work_dir.as_deref().ok_or_else(|| {
            PocError::Integrity("mount attestation observed workdir is missing".to_owned())
        })?),
    )?;
    validate_path_identity("mount attestation target", &target.target_identity)
}

fn validate_path_identity(label: &str, identity: &StorageAdminPathIdentity) -> PocResult<()> {
    if identity.mount_id == 0 || identity.inode == 0 {
        Err(PocError::Integrity(format!(
            "storage-admin {label} identity is incomplete"
        )))
    } else {
        Ok(())
    }
}

struct OperationPaths {
    directory: PathBuf,
    lock: PathBuf,
    attempt: PathBuf,
    receipt_validation_diagnostic: PathBuf,
    receipt: PathBuf,
}

fn operation_paths(request: &StorageAdminRequest) -> PocResult<OperationPaths> {
    validate_path_atom("operation id", request.operation_id.as_str())?;
    let root = request.scope.control_root.join(STORAGE_ADMIN_DIRECTORY);
    let directory = root.join(request.operation_id.as_str());
    Ok(OperationPaths {
        lock: root.join(LOCK_FILE),
        attempt: directory.join(ATTEMPT_FILE),
        receipt_validation_diagnostic: directory.join(RECEIPT_VALIDATION_DIAGNOSTIC_FILE),
        receipt: directory.join(RECEIPT_FILE),
        directory,
    })
}

fn prepare_operation_store(paths: &OperationPaths) -> PocResult<()> {
    let root = paths
        .lock
        .parent()
        .ok_or_else(|| PocError::Integrity("storage-admin lock has no parent".to_owned()))?;
    fs::create_dir_all(&paths.directory).map_err(|error| {
        PocError::io(
            "create storage-admin operation directory",
            &paths.directory,
            error,
        )
    })?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&paths.lock)
    {
        Ok(file) => crate::durable::sync_all(&file)
            .map_err(|error| PocError::io("fsync storage-admin lock", &paths.lock, error))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(PocError::io(
                "create storage-admin lock",
                &paths.lock,
                error,
            ));
        }
    }
    crate::durable::fsync_dir(root)?;
    crate::durable::fsync_dir(root.parent().ok_or_else(|| {
        PocError::Integrity("storage-admin root has no control-root parent".to_owned())
    })?)
}

fn validate_stored_attempt(
    attempt: &StorageAdminAttempt,
    selection: &StorageAdminSelection,
    invocation: &StorageAdminInvocation,
) -> PocResult<()> {
    require_equal(
        "stored attempt schema version",
        &attempt.schema_version,
        &SCHEMA_VERSION,
    )?;
    require_equal(
        "stored attempt interface version",
        attempt.interface_version.as_str(),
        INTERFACE_VERSION,
    )?;
    require_equal(
        "stored attempt operation id",
        &attempt.operation_id,
        &selection.request.operation_id,
    )?;
    require_equal(
        "stored attempt request digest",
        &attempt.request_sha256,
        &selection.request_sha256,
    )?;
    require_equal(
        "stored attempt request",
        &attempt.request,
        &selection.request,
    )?;
    require_equal(
        "stored attempt authorization",
        &attempt.authorization,
        &invocation.authorization,
    )?;
    require_equal(
        "stored attempt durability",
        &attempt.durability,
        &invocation.durability,
    )?;
    require_equal(
        "stored attempt trusted executable hash",
        &attempt.trusted_executable_sha256,
        &invocation.trusted_executable_sha256,
    )?;
    require_equal(
        "stored attempt workload cgroup",
        &attempt.workload_cgroup_procs,
        &invocation.workload_cgroup_procs,
    )?;
    require_equal(
        "stored attempt mount namespace holder pid",
        &attempt.mount_namespace_holder_pid,
        &invocation.mount_namespace_holder_pid,
    )?;
    require_equal(
        "stored attempt mount receipt binding",
        &attempt.mount_receipt_binding,
        &invocation.mount_receipt_binding,
    )?;
    if attempt.started_unix_ms == 0 {
        return Err(PocError::Integrity(
            "stored storage-admin attempt has a zero start timestamp".to_owned(),
        ));
    }
    Ok(())
}

fn validate_storage_admin_durability(
    action: StorageAdminAction,
    durability: StorageAdminDurability,
) -> PocResult<()> {
    if durability == StorageAdminDurability::SessionLifetime && action != StorageAdminAction::Mount
    {
        return Err(PocError::Integrity(
            "session-lifetime storage durability is valid only for Mount".to_owned(),
        ));
    }
    Ok(())
}

fn validate_stored_receipt(
    receipt: &StorageAdminReceipt,
    selection: &StorageAdminSelection,
    receipt_path: &Path,
) -> PocResult<()> {
    require_equal(
        "stored receipt schema version",
        &receipt.schema_version,
        &SCHEMA_VERSION,
    )?;
    require_equal(
        "stored receipt interface version",
        receipt.interface_version.as_str(),
        INTERFACE_VERSION,
    )?;
    require_equal(
        "stored receipt profile id",
        receipt.profile_id.as_str(),
        selection.profile_id(),
    )?;
    require_equal(
        "stored receipt operation id",
        &receipt.operation_id,
        &selection.request.operation_id,
    )?;
    require_equal(
        "stored receipt action",
        &receipt.action,
        &selection.request.action,
    )?;
    require_equal(
        "stored receipt request digest",
        &receipt.request_sha256,
        &selection.request_sha256,
    )?;
    require_equal(
        "stored receipt trusted executable",
        receipt.trusted_executable.as_path(),
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
    )?;
    let expected_capabilities = owned_strings(selection.profile().effective_capabilities());
    require_equal(
        "stored receipt effective capabilities",
        &receipt.effective_capabilities,
        &expected_capabilities,
    )?;
    let expected_syscalls = owned_strings(STORAGE_ADMIN_PRIVILEGED_SYSCALLS);
    require_equal(
        "stored receipt privileged syscalls",
        &receipt.allowed_privileged_syscalls,
        &expected_syscalls,
    )?;
    validate_receipt_authority_evidence(
        &receipt.process_evidence,
        &receipt.mount_plan_evidence,
        &receipt.scope,
        selection.profile(),
    )?;
    validate_mount_authority_evidence(
        selection,
        &receipt.process_evidence,
        &receipt.mount_plan_evidence,
        receipt.outcome,
        receipt.mount_attestation.as_ref(),
        receipt.mount_receipt_binding.as_ref(),
    )?;
    require_equal(
        "stored receipt scope",
        &receipt.scope,
        &selection.request.scope,
    )?;
    require_equal(
        "stored receipt path",
        receipt.receipt_path.as_path(),
        receipt_path,
    )?;
    if receipt.idempotent_replay {
        return Err(PocError::Integrity(
            "durable storage-admin receipt cannot be marked as a replay".to_owned(),
        ));
    }
    if receipt.started_unix_ms == 0 || receipt.completed_unix_ms < receipt.started_unix_ms {
        return Err(PocError::Integrity(
            "stored storage-admin receipt timestamps are invalid".to_owned(),
        ));
    }
    validate_execution(&StorageAdminExecution {
        outcome: receipt.outcome,
        cleanup_complete: receipt.cleanup_complete,
        failure: receipt.failure.clone(),
    })
}

fn validate_receipt_authority_evidence(
    process: &StorageAdminProcessEvidence,
    mount_plan: &StorageAdminMountPlanEvidence,
    scope: &StorageAdminScope,
    profile: StorageAdminCapabilityProfile,
) -> PocResult<()> {
    require_equal(
        "receipt executable identity",
        process.executable.as_path(),
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
    )?;
    require_equal(
        "receipt effective capability mask",
        &process.capabilities.effective,
        &profile.effective_capability_mask(),
    )?;
    require_equal(
        "receipt permitted capability mask",
        &process.capabilities.permitted,
        &profile.effective_capability_mask(),
    )?;
    require_equal(
        "receipt bounding capability mask",
        &process.capabilities.bounding,
        &profile.effective_capability_mask(),
    )?;
    require_equal(
        "receipt inheritable capability mask",
        &process.capabilities.inheritable,
        &0,
    )?;
    require_equal(
        "receipt ambient capability mask",
        &process.capabilities.ambient,
        &0,
    )?;
    require_equal(
        "receipt seccomp profile",
        process.seccomp.profile_id.as_str(),
        STORAGE_ADMIN_SECCOMP_PROFILE_ID,
    )?;
    require_equal(
        "receipt seccomp profile hash",
        process.seccomp.profile_sha256.as_str(),
        storage_admin_seccomp_profile_sha256().as_str(),
    )?;
    validate_sha256("receipt executable hash", &process.executable_sha256)?;
    require_equal("receipt seccomp mode", &process.seccomp.mode, &2)?;
    if process.seccomp.filter_count == 0 || !process.seccomp.no_new_privs {
        return Err(PocError::Integrity(
            "storage-admin receipt is missing active seccomp or NoNewPrivs evidence".to_owned(),
        ));
    }
    validate_workload_cgroup_procs(&process.workload_cgroup_procs)?;
    if process.workload_cgroup_member_pid == 0 {
        return Err(PocError::Integrity(
            "storage-admin receipt has an invalid cgroup member pid".to_owned(),
        ));
    }
    require_equal(
        "receipt process mount namespace",
        &process.mount_namespace_id,
        &scope.mount_namespace_id,
    )?;
    require_equal(
        "receipt mount-plan namespace",
        &mount_plan.mount_namespace_id,
        &scope.mount_namespace_id,
    )?;
    require_equal(
        "receipt mount-plan target",
        &mount_plan.target,
        &scope.workspace_root,
    )?;
    require_equal(
        "receipt mount-plan lower directories",
        &mount_plan.lower_dirs_newest_first,
        &scope.lower_dirs_newest_first,
    )?;
    require_equal(
        "receipt mount-plan upper directory",
        &mount_plan.upper_dir,
        &scope.allocation_root.join("upper"),
    )?;
    require_equal(
        "receipt mount-plan work directory",
        &mount_plan.work_dir,
        &scope.allocation_root.join("work"),
    )?;
    validate_mount_input_access_evidence(&mount_plan.input_access, mount_plan)?;
    validate_mount_table_evidence("before", &mount_plan.mountinfo_before, mount_plan)?;
    validate_mount_table_evidence("after", &mount_plan.mountinfo_after, mount_plan)
}

fn validate_mount_input_access_evidence(
    evidence: &StorageAdminMountInputAccessEvidence,
    plan: &StorageAdminMountPlanEvidence,
) -> PocResult<()> {
    let mut expected = plan
        .lower_dirs_newest_first
        .iter()
        .enumerate()
        .map(|(index, path)| (format!("lower_dir[{index}]"), path))
        .collect::<Vec<_>>();
    expected.extend([
        ("upper_dir".to_owned(), &plan.upper_dir),
        ("work_dir".to_owned(), &plan.work_dir),
        ("workspace_root".to_owned(), &plan.target),
    ]);
    if evidence.paths.len() != expected.len() {
        return Err(PocError::Integrity(
            "storage-admin input-access evidence does not cover the mount plan".to_owned(),
        ));
    }
    for (observed, (label, path)) in evidence.paths.iter().zip(expected) {
        require_equal("receipt input-access label", &observed.label, &label)?;
        require_equal("receipt input-access path", &observed.path, path)?;
        if observed.effective_access.len() != 2 {
            return Err(PocError::Integrity(
                "storage-admin input-access evidence has an unexpected check count".to_owned(),
            ));
        }
        if observed.metadata.is_some() == observed.metadata_error.is_some() {
            return Err(PocError::Integrity(
                "storage-admin input-access metadata evidence is ambiguous".to_owned(),
            ));
        }
        for (check, requested) in observed.effective_access.iter().zip([
            ["read", "search"].as_slice(),
            ["read", "write", "search"].as_slice(),
        ]) {
            let expected_requested = requested
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>();
            require_equal(
                "receipt input-access requested modes",
                &check.requested,
                &expected_requested,
            )?;
            if check.allowed == check.error.is_some() {
                return Err(PocError::Integrity(
                    "storage-admin input-access result is ambiguous".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_mount_table_evidence(
    phase: &str,
    table: &StorageAdminMountTableEvidence,
    plan: &StorageAdminMountPlanEvidence,
) -> PocResult<()> {
    validate_sha256(&format!("receipt mountinfo {phase} hash"), &table.sha256)?;
    require_equal(
        &format!("receipt mountinfo {phase} canonical target hash"),
        &table.sha256,
        &storage_admin_mountinfo_target_sha256(table.target.as_ref())?,
    )?;
    let Some(target) = &table.target else {
        return Ok(());
    };
    require_equal(
        "receipt observed mount target",
        &target.target,
        &plan.target,
    )?;
    require_equal(
        "receipt observed mount filesystem type",
        target.filesystem_type.as_str(),
        plan.filesystem_type.as_str(),
    )?;
    require_equal(
        "receipt observed mount source",
        target.source.as_str(),
        plan.source.as_str(),
    )?;
    if !target.mount_options.iter().any(|value| value == "nodev")
        || !target.mount_options.iter().any(|value| value == "nosuid")
    {
        return Err(PocError::Integrity(
            "storage-admin observed mount is missing fixed nodev/nosuid options".to_owned(),
        ));
    }
    require_equal(
        "receipt observed mount upper directory",
        &target.upper_dir,
        &Some(plan.upper_dir.clone()),
    )?;
    require_equal(
        "receipt observed mount work directory",
        &target.work_dir,
        &Some(plan.work_dir.clone()),
    )
}

fn owned_strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn validate_absolute_normalized_path(label: &str, path: &Path) -> PocResult<()> {
    if !path.is_absolute()
        || path == Path::new("/")
        || !path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(PocError::Integrity(format!(
            "storage-admin {label} must be a non-root normalized absolute path: {}",
            path.display()
        )));
    }
    #[cfg(target_os = "linux")]
    if path
        .as_os_str()
        .as_bytes()
        .iter()
        .any(|byte| matches!(byte, b'\0' | b',' | b':'))
    {
        return Err(PocError::Integrity(format!(
            "storage-admin {label} contains a forbidden mount-option byte: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_path_atom(label: &str, value: &str) -> PocResult<()> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 255
        || !bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(PocError::Integrity(format!(
            "storage-admin {label} is not a safe identifier"
        )));
    }
    Ok(())
}

fn validate_text(label: &str, value: &str) -> PocResult<()> {
    if value.is_empty()
        || value.len() > 512
        || value.chars().any(char::is_control)
        || value.contains('/')
    {
        return Err(PocError::Integrity(format!(
            "storage-admin {label} is invalid"
        )));
    }
    Ok(())
}

fn validate_mount_namespace_id(value: &str) -> PocResult<()> {
    parse_mount_namespace_inode(value).map(|_| ())
}

fn validate_workload_cgroup_procs(path: &Path) -> PocResult<()> {
    validate_absolute_normalized_path("workload cgroup.procs", path)?;
    if path.file_name().and_then(|name| name.to_str()) != Some("cgroup.procs") {
        return Err(PocError::Integrity(
            "storage-admin workload cgroup path must name cgroup.procs".to_owned(),
        ));
    }
    Ok(())
}

fn parse_mount_namespace_inode(value: &str) -> PocResult<u64> {
    let inode = value
        .strip_prefix("mnt:[")
        .and_then(|value| value.strip_suffix(']'))
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| {
            PocError::Integrity(
                "storage-admin mount namespace id is not a kernel namespace identity".to_owned(),
            )
        })?;
    let inode = inode.parse::<u64>().map_err(|error| {
        PocError::Integrity(format!(
            "storage-admin mount namespace id is invalid: {error}"
        ))
    })?;
    if inode == 0 {
        return Err(PocError::Integrity(
            "storage-admin mount namespace id must be non-zero".to_owned(),
        ));
    }
    Ok(inode)
}

#[cfg(target_os = "linux")]
fn parse_user_namespace_inode(value: &str) -> PocResult<u64> {
    let inode = value
        .strip_prefix("user:[")
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| {
            PocError::Integrity(
                "storage-admin user namespace id is not a kernel namespace identity".to_owned(),
            )
        })?;
    let inode = inode.parse::<u64>().map_err(|error| {
        PocError::Integrity(format!(
            "storage-admin user namespace id is invalid: {error}"
        ))
    })?;
    if inode == 0 {
        return Err(PocError::Integrity(
            "storage-admin user namespace id must be non-zero".to_owned(),
        ));
    }
    Ok(inode)
}

fn validate_mount_namespace_holder_pid(holder_pid: u32) -> PocResult<()> {
    if holder_pid == 0 || holder_pid > i32::MAX as u32 {
        return Err(PocError::Integrity(
            "storage-admin mount namespace holder pid is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn mount_namespace_path(holder_pid: u32) -> PocResult<PathBuf> {
    validate_mount_namespace_holder_pid(holder_pid)?;
    Ok(PathBuf::from(format!("/proc/{holder_pid}/ns/mnt")))
}

fn user_namespace_path(holder_pid: u32) -> PocResult<PathBuf> {
    validate_mount_namespace_holder_pid(holder_pid)?;
    Ok(PathBuf::from(format!("/proc/{holder_pid}/ns/user")))
}

pub fn validate_opened_mount_namespace(
    expected_namespace_id: &str,
    opened_namespace_id: &str,
    opened_inode: u64,
) -> PocResult<()> {
    let expected_inode = parse_mount_namespace_inode(expected_namespace_id)?;
    require_equal(
        "opened mount namespace",
        opened_namespace_id,
        expected_namespace_id,
    )?;
    require_equal(
        "opened mount namespace inode",
        &opened_inode,
        &expected_inode,
    )
}

#[cfg(target_os = "linux")]
fn validate_opened_user_namespace(opened_namespace_id: &str, opened_inode: u64) -> PocResult<()> {
    let expected_inode = parse_user_namespace_inode(opened_namespace_id)?;
    require_equal(
        "opened user namespace inode",
        &opened_inode,
        &expected_inode,
    )
}

pub fn storage_admin_process_evidence_from_status(
    executable: PathBuf,
    executable_sha256: String,
    status: &str,
    workload_cgroup_procs: PathBuf,
    workload_cgroup_member_pid: u32,
    mount_namespace_id: String,
    mount_namespace_inode: u64,
) -> PocResult<StorageAdminProcessEvidence> {
    validate_opened_mount_namespace(
        &mount_namespace_id,
        &mount_namespace_id,
        mount_namespace_inode,
    )?;
    validate_workload_cgroup_procs(&workload_cgroup_procs)?;
    if workload_cgroup_member_pid == 0 {
        return Err(PocError::Integrity(
            "storage-admin cgroup member pid is invalid".to_owned(),
        ));
    }
    let no_new_privs = match parse_status_u32(status, "NoNewPrivs")? {
        0 => false,
        1 => true,
        value => {
            return Err(PocError::Integrity(format!(
                "invalid NoNewPrivs value: {value}"
            )));
        }
    };
    Ok(StorageAdminProcessEvidence {
        executable,
        executable_sha256,
        capabilities: StorageAdminCapabilitySetEvidence {
            effective: parse_status_hex(status, "CapEff")?,
            permitted: parse_status_hex(status, "CapPrm")?,
            inheritable: parse_status_hex(status, "CapInh")?,
            bounding: parse_status_hex(status, "CapBnd")?,
            ambient: parse_status_hex(status, "CapAmb")?,
        },
        seccomp: StorageAdminSeccompEvidence {
            profile_id: STORAGE_ADMIN_SECCOMP_PROFILE_ID.to_owned(),
            profile_sha256: storage_admin_seccomp_profile_sha256(),
            mode: parse_status_u32(status, "Seccomp")?,
            filter_count: parse_status_u32(status, "Seccomp_filters")?,
            no_new_privs,
        },
        workload_cgroup_procs,
        workload_cgroup_member_pid,
        mount_namespace_id,
        mount_namespace_inode,
    })
}

pub fn storage_admin_mount_plan_evidence(
    scope: &StorageAdminScope,
) -> PocResult<StorageAdminMountPlanEvidence> {
    validate_scope(scope)?;
    Ok(StorageAdminMountPlanEvidence {
        mount_namespace_id: scope.mount_namespace_id.clone(),
        source: RAW_OVERLAY_MOUNTINFO_SOURCE.to_owned(),
        filesystem_type: "overlay".to_owned(),
        target: scope.workspace_root.clone(),
        flags: vec!["MS_NODEV".to_owned(), "MS_NOSUID".to_owned()],
        lower_dirs_newest_first: scope.lower_dirs_newest_first.clone(),
        upper_dir: scope.allocation_root.join("upper"),
        work_dir: scope.allocation_root.join("work"),
        input_access: capture_storage_admin_input_access(scope),
        mountinfo_before: StorageAdminMountTableEvidence {
            sha256: storage_admin_mountinfo_target_sha256(None)?,
            target: None,
        },
        mountinfo_after: StorageAdminMountTableEvidence {
            sha256: storage_admin_mountinfo_target_sha256(None)?,
            target: None,
        },
    })
}

#[cfg(target_os = "linux")]
fn capture_storage_admin_input_access(
    scope: &StorageAdminScope,
) -> StorageAdminMountInputAccessEvidence {
    let mut paths = scope
        .lower_dirs_newest_first
        .iter()
        .enumerate()
        .map(|(index, path)| capture_storage_admin_path_access(format!("lower_dir[{index}]"), path))
        .collect::<Vec<_>>();
    paths.extend([
        capture_storage_admin_path_access(
            "upper_dir".to_owned(),
            &scope.allocation_root.join("upper"),
        ),
        capture_storage_admin_path_access(
            "work_dir".to_owned(),
            &scope.allocation_root.join("work"),
        ),
        capture_storage_admin_path_access("workspace_root".to_owned(), &scope.workspace_root),
    ]);
    StorageAdminMountInputAccessEvidence { paths }
}

#[cfg(not(target_os = "linux"))]
fn capture_storage_admin_input_access(
    scope: &StorageAdminScope,
) -> StorageAdminMountInputAccessEvidence {
    let mut paths = scope
        .lower_dirs_newest_first
        .iter()
        .enumerate()
        .map(|(index, path)| {
            unsupported_storage_admin_path_access(format!("lower_dir[{index}]"), path)
        })
        .collect::<Vec<_>>();
    paths.extend([
        unsupported_storage_admin_path_access(
            "upper_dir".to_owned(),
            &scope.allocation_root.join("upper"),
        ),
        unsupported_storage_admin_path_access(
            "work_dir".to_owned(),
            &scope.allocation_root.join("work"),
        ),
        unsupported_storage_admin_path_access("workspace_root".to_owned(), &scope.workspace_root),
    ]);
    StorageAdminMountInputAccessEvidence { paths }
}

#[cfg(not(target_os = "linux"))]
fn unsupported_storage_admin_path_access(
    label: String,
    path: &Path,
) -> StorageAdminPathAccessEvidence {
    let unsupported = "effective credential evidence requires Linux".to_owned();
    StorageAdminPathAccessEvidence {
        label,
        path: path.to_path_buf(),
        metadata: None,
        metadata_error: Some(unsupported.clone()),
        effective_access: vec![
            StorageAdminEffectiveAccessCheck {
                requested: vec!["read".to_owned(), "search".to_owned()],
                allowed: false,
                error: Some(unsupported.clone()),
            },
            StorageAdminEffectiveAccessCheck {
                requested: vec!["read".to_owned(), "write".to_owned(), "search".to_owned()],
                allowed: false,
                error: Some(unsupported),
            },
        ],
    }
}

#[cfg(target_os = "linux")]
fn capture_storage_admin_path_access(label: String, path: &Path) -> StorageAdminPathAccessEvidence {
    let (metadata, metadata_error) = match fs::metadata(path) {
        Ok(metadata) => (
            Some(StorageAdminPathMetadataEvidence {
                is_directory: metadata.is_dir(),
                device: metadata.dev(),
                inode: metadata.ino(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                mode: metadata.mode(),
            }),
            None,
        ),
        Err(error) => (None, Some(error.to_string())),
    };
    StorageAdminPathAccessEvidence {
        label,
        path: path.to_path_buf(),
        metadata,
        metadata_error,
        effective_access: vec![
            capture_effective_access(path, libc::R_OK | libc::X_OK, ["read", "search"]),
            capture_effective_access(
                path,
                libc::R_OK | libc::W_OK | libc::X_OK,
                ["read", "write", "search"],
            ),
        ],
    }
}

#[cfg(target_os = "linux")]
fn capture_effective_access<const N: usize>(
    path: &Path,
    mode: libc::c_int,
    requested: [&str; N],
) -> StorageAdminEffectiveAccessCheck {
    let requested = requested.into_iter().map(str::to_owned).collect();
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return StorageAdminEffectiveAccessCheck {
            requested,
            allowed: false,
            error: Some("path contains a NUL byte".to_owned()),
        };
    };
    // SAFETY: the path is a NUL-terminated C string and faccessat only reads it.
    let result = unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), mode, libc::AT_EACCESS) };
    if result == 0 {
        StorageAdminEffectiveAccessCheck {
            requested,
            allowed: true,
            error: None,
        }
    } else {
        StorageAdminEffectiveAccessCheck {
            requested,
            allowed: false,
            error: Some(std::io::Error::last_os_error().to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
fn capture_storage_admin_mountinfo(
    scope: &StorageAdminScope,
) -> PocResult<StorageAdminMountTableEvidence> {
    capture_storage_admin_mountinfo_from_path(scope, Path::new("/proc/self/mountinfo"))
}

#[cfg(target_os = "linux")]
fn capture_storage_admin_mountinfo_from_path(
    scope: &StorageAdminScope,
    mountinfo_path: &Path,
) -> PocResult<StorageAdminMountTableEvidence> {
    const MAX_MOUNTINFO_BYTES: u64 = 16 * 1024 * 1024;
    let mut mountinfo = File::open(mountinfo_path)
        .map_err(|error| PocError::io("open storage-admin mount table", mountinfo_path, error))?;
    let mut bytes = Vec::new();
    mountinfo
        .by_ref()
        .take(MAX_MOUNTINFO_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| PocError::io("read storage-admin mount table", mountinfo_path, error))?;
    if bytes.len() as u64 > MAX_MOUNTINFO_BYTES {
        return Err(PocError::Integrity(
            "storage-admin mount table exceeds the bounded receipt budget".to_owned(),
        ));
    }
    let raw = std::str::from_utf8(&bytes).map_err(|error| {
        PocError::Integrity(format!("storage-admin mount table is not UTF-8: {error}"))
    })?;
    let target = raw.lines().find_map(|line| {
        let entry = parse_mountinfo_line(line)?;
        (entry.target == scope.workspace_root).then_some(entry)
    });
    Ok(StorageAdminMountTableEvidence {
        sha256: storage_admin_mountinfo_target_sha256(target.as_ref())?,
        target,
    })
}

#[cfg(not(target_os = "linux"))]
fn capture_storage_admin_mountinfo(
    _scope: &StorageAdminScope,
) -> PocResult<StorageAdminMountTableEvidence> {
    Err(PocError::Unsupported(
        "storage-admin mount-table evidence requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn parse_mountinfo_line(line: &str) -> Option<StorageAdminObservedMount> {
    let (left, right) = line.split_once(" - ")?;
    let left_fields: Vec<_> = left.split_ascii_whitespace().collect();
    let right_fields: Vec<_> = right.split_ascii_whitespace().collect();
    if left_fields.len() < 6 || right_fields.len() < 3 {
        return None;
    }
    let mount_id = left_fields[0].parse().ok()?;
    let parent_mount_id = left_fields[1].parse().ok()?;
    let root = PathBuf::from(unescape_mountinfo_path(left_fields[3])?);
    let target = PathBuf::from(unescape_mountinfo_path(left_fields[4])?);
    let mut mount_options = split_mount_options(left_fields[5]);
    let mut optional_fields: Vec<String> =
        left_fields[6..].iter().map(ToString::to_string).collect();
    let raw_super_options = split_mount_options(right_fields[2]);
    let upper_dir = mount_option_value(&raw_super_options, "upperdir")
        .and_then(unescape_mountinfo_path)
        .map(PathBuf::from);
    let work_dir = mount_option_value(&raw_super_options, "workdir")
        .and_then(unescape_mountinfo_path)
        .map(PathBuf::from);
    let mut super_options = raw_super_options
        .into_iter()
        .map(|option| {
            if option.starts_with("lowerdir=") {
                "lowerdir=<redacted>".to_owned()
            } else {
                option
            }
        })
        .collect::<Vec<_>>();
    mount_options.sort();
    optional_fields.sort();
    super_options.sort();
    Some(StorageAdminObservedMount {
        mount_id,
        parent_mount_id,
        root,
        source: right_fields[1].to_owned(),
        filesystem_type: right_fields[0].to_owned(),
        target,
        mount_options,
        optional_fields,
        super_options,
        upper_dir,
        work_dir,
    })
}

#[cfg(target_os = "linux")]
fn split_mount_options(value: &str) -> Vec<String> {
    value.split(',').map(str::to_owned).collect()
}

#[cfg(target_os = "linux")]
fn mount_option_value<'a>(options: &'a [String], key: &str) -> Option<&'a str> {
    options.iter().find_map(|option| {
        option
            .strip_prefix(key)
            .and_then(|value| value.strip_prefix('='))
    })
}

#[cfg(target_os = "linux")]
fn unescape_mountinfo_path(value: &str) -> Option<String> {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let octal = bytes.get(index + 1..index + 4)?;
            let text = std::str::from_utf8(octal).ok()?;
            let byte = u8::from_str_radix(text, 8).ok()?;
            output.push(byte);
            index += 4;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn require_equal<T: PartialEq + ?Sized>(label: &str, observed: &T, expected: &T) -> PocResult<()> {
    if observed == expected {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "storage-admin {label} does not match trusted binding"
        )))
    }
}

fn validate_wire_shape(value: &serde_json::Value) -> PocResult<()> {
    validate_object_keys(
        "invocation",
        value,
        &[
            "expected_request",
            "request",
            "authorization",
            "trusted_actor_id",
            "durability",
            "trusted_executable_sha256",
            "workload_cgroup_procs",
            "mount_namespace_holder_pid",
            "mount_receipt_binding",
        ],
    )?;
    let object = value.as_object().ok_or_else(|| {
        PocError::Integrity("storage-admin invocation must be an object".to_owned())
    })?;
    for key in ["expected_request", "request"] {
        let request = object.get(key).ok_or_else(|| {
            PocError::Integrity(format!("storage-admin invocation is missing {key}"))
        })?;
        validate_object_keys(
            key,
            request,
            &[
                "schema_version",
                "interface_version",
                "profile_id",
                "operation_id",
                "action",
                "scope",
            ],
        )?;
        let scope = request
            .get("scope")
            .ok_or_else(|| PocError::Integrity(format!("storage-admin {key} is missing scope")))?;
        validate_object_keys(
            "scope",
            scope,
            &[
                "run_id",
                "sandbox_id",
                "workspace_session_id",
                "session_id",
                "allocation_id",
                "lease_id",
                "lease_epoch",
                "mount_namespace_id",
                "payload_root",
                "control_root",
                "lower_dirs_newest_first",
                "allocation_root",
                "workspace_root",
            ],
        )?;
    }
    let authorization = object.get("authorization").ok_or_else(|| {
        PocError::Integrity("storage-admin invocation is missing authorization".to_owned())
    })?;
    validate_object_keys(
        "authorization",
        authorization,
        &[
            "authenticated",
            "actor_id",
            "operation_id",
            "run_id",
            "sandbox_id",
            "workspace_session_id",
            "session_id",
            "allocation_id",
            "lease_id",
            "lease_epoch",
            "mount_namespace_id",
        ],
    )
}

fn validate_holder_namespace_semantic_snapshot_wire_shape(
    value: &serde_json::Value,
) -> PocResult<()> {
    validate_object_keys(
        "holder-namespace semantic snapshot",
        value,
        &["format", "storage_admin", "semantic"],
    )?;
    let object = value.as_object().ok_or_else(|| {
        PocError::Integrity("holder-namespace semantic snapshot must be an object".to_owned())
    })?;
    let storage = object.get("storage_admin").ok_or_else(|| {
        PocError::Integrity(
            "holder-namespace semantic snapshot is missing storage_admin".to_owned(),
        )
    })?;
    validate_wire_shape(storage)?;
    let semantic = object.get("semantic").ok_or_else(|| {
        PocError::Integrity(
            "holder-namespace semantic snapshot is missing semantic request".to_owned(),
        )
    })?;
    validate_object_keys(
        "holder-namespace semantic request",
        semantic,
        &[
            "schema_version",
            "operation_id",
            "allocation_id",
            "sealed_tree",
            "spool_dir",
            "canonical_object_dir",
            "attribution",
        ],
    )?;
    let attribution = semantic.get("attribution").ok_or_else(|| {
        PocError::Integrity("holder-namespace semantic request is missing attribution".to_owned())
    })?;
    validate_object_keys(
        "holder-namespace semantic attribution",
        attribution,
        &["actor_id", "semantic_operation_id"],
    )
}

fn validate_holder_namespace_snapshot_request(
    selection: &StorageAdminSelection,
    semantic: &SemanticBuildRequest,
) -> PocResult<()> {
    let scope = &selection.request.scope;
    require_equal(
        "holder semantic schema version",
        &semantic.schema_version,
        &SCHEMA_VERSION,
    )?;
    require_equal(
        "holder semantic operation id",
        &semantic.operation_id,
        &selection.request.operation_id,
    )?;
    require_equal(
        "holder semantic allocation id",
        &semantic.allocation_id,
        &scope.allocation_id,
    )?;
    require_equal(
        "holder semantic sealed tree",
        &semantic.sealed_tree,
        &scope.workspace_root,
    )?;
    let operation_dir = scope
        .control_root
        .join("runtime-lifecycle")
        .join("operations")
        .join(selection.request.operation_id.as_str());
    require_equal(
        "holder semantic spool directory",
        &semantic.spool_dir,
        &operation_dir.join("initial-semantic-spool"),
    )?;
    require_equal(
        "holder semantic canonical object directory",
        &semantic.canonical_object_dir,
        &scope
            .control_root
            .join("runs")
            .join(scope.run_id.as_str())
            .join("canonical-objects"),
    )?;
    require_equal(
        "holder semantic attribution actor",
        semantic.attribution.actor_id.as_str(),
        "sandbox-runtime-publication",
    )?;
    require_equal(
        "holder semantic attribution operation",
        semantic.attribution.semantic_operation_id.as_str(),
        scope.run_id.as_str(),
    )
}

fn validate_object_keys(
    label: &str,
    value: &serde_json::Value,
    expected: &[&str],
) -> PocResult<()> {
    let object = value
        .as_object()
        .ok_or_else(|| PocError::Integrity(format!("storage-admin {label} must be an object")))?;
    let observed: BTreeSet<_> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<_> = expected.iter().copied().collect();
    if observed == expected {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "storage-admin {label} fields do not match the fixed schema"
        )))
    }
}

#[cfg(target_os = "linux")]
fn prepare_platform_process(
    invocation: &StorageAdminInvocation,
    profile: StorageAdminCapabilityProfile,
) -> PocResult<StorageAdminProcessEvidence> {
    enter_bound_user_and_mount_namespaces(
        invocation.mount_namespace_holder_pid,
        &invocation.request.scope.mount_namespace_id,
    )?;
    narrow_process_capabilities(profile)?;
    set_no_new_privileges()?;
    install_storage_admin_seccomp_profile()?;
    verify_process_identity(&invocation.workload_cgroup_procs, profile)
}

#[cfg(not(target_os = "linux"))]
fn prepare_platform_process(
    _invocation: &StorageAdminInvocation,
    _profile: StorageAdminCapabilityProfile,
) -> PocResult<StorageAdminProcessEvidence> {
    Err(PocError::Unsupported(
        "mpla-storage-admin-v1 execution requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn narrow_process_capabilities(profile: StorageAdminCapabilityProfile) -> PocResult<()> {
    // SAFETY: prctl is called with fixed integer arguments and no borrowed memory.
    let ambient_result = unsafe {
        libc::prctl(
            PR_CAP_AMBIENT,
            PR_CAP_AMBIENT_CLEAR_ALL,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if ambient_result != 0 {
        return Err(PocError::Integrity(format!(
            "failed to clear storage-admin ambient capabilities: {}",
            std::io::Error::last_os_error()
        )));
    }

    // CAP_SETPCAP is retained only long enough to make the helper's bounding
    // set irreversible. It is absent before the helper enters the bound mount
    // namespace and before any storage syscall can run.
    let mut bootstrap_capabilities = profile.capability_numbers().to_vec();
    bootstrap_capabilities.push(CAP_SETPCAP_NUMBER);
    set_capability_masks(&bootstrap_capabilities)?;
    for capability in 0..=MAX_CAPABILITY {
        if !profile.capability_numbers().contains(&capability) {
            drop_bounding_capability(capability)?;
        }
    }
    set_capability_masks(profile.capability_numbers())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn set_capability_masks(capabilities: &[u32]) -> PocResult<()> {
    let header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapabilityData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; CAPABILITY_WORDS];
    for capability in capabilities {
        let word = (*capability / 32) as usize;
        let bit = 1_u32 << (*capability % 32);
        data[word].effective |= bit;
        data[word].permitted |= bit;
    }
    // SAFETY: capset reads the fixed header and two-word capability array for this process.
    let result = unsafe { libc::syscall(libc::SYS_capset, &header, data.as_mut_ptr()) };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "failed to narrow storage-admin capability masks: {}",
            std::io::Error::last_os_error()
        )))
    }
}

#[cfg(target_os = "linux")]
fn drop_bounding_capability(capability: u32) -> PocResult<()> {
    // SAFETY: prctl is called with fixed integer arguments and no borrowed memory.
    let result = unsafe { libc::prctl(PR_CAPBSET_DROP, capability as libc::c_ulong, 0, 0, 0) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EINVAL) {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "failed to drop storage-admin bounding capability {capability}: {error}"
        )))
    }
}

#[cfg(target_os = "linux")]
fn set_no_new_privileges() -> PocResult<()> {
    // SAFETY: prctl is called with fixed integer arguments and no borrowed memory.
    let result = unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "failed to set storage-admin NoNewPrivs: {}",
            std::io::Error::last_os_error()
        )))
    }
}

/// Install the helper's immutable post-bootstrap syscall policy. The helper
/// has no reason to replace itself or create descendants after its fixed stdin
/// payload is decoded; denying those syscalls makes the authority single-use.
#[cfg(target_os = "linux")]
fn install_storage_admin_seccomp_profile() -> PocResult<()> {
    let mut program = vec![
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_ARCH_OFFSET),
        bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, storage_admin_audit_arch(), 1, 0),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, SECCOMP_DATA_NR_OFFSET),
        bpf_jump(BPF_JMP | BPF_JGE | BPF_K, X32_SYSCALL_BIT, 0, 1),
        bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
    ];
    for number in storage_admin_denied_syscalls() {
        program.push(bpf_jump(BPF_JMP | BPF_JEQ | BPF_K, number, 0, 1));
        program.push(bpf_stmt(
            BPF_RET | BPF_K,
            SECCOMP_RET_ERRNO | libc::EPERM as u32,
        ));
    }
    program.push(bpf_stmt(BPF_RET | BPF_K, SECCOMP_RET_ALLOW));
    let fprog = SockFprog {
        len: program.len().try_into().map_err(|_| {
            PocError::Integrity("storage-admin seccomp profile is too large".to_owned())
        })?,
        filter: program.as_ptr(),
    };
    // SAFETY: seccomp reads the immutable BPF program while this stack frame is live.
    let result = unsafe {
        libc::syscall(
            SYS_SECCOMP,
            SECCOMP_SET_MODE_FILTER,
            0,
            &fprog as *const SockFprog,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "install storage-admin seccomp profile: {}",
            std::io::Error::last_os_error()
        )))
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const fn storage_admin_audit_arch() -> u32 {
    0xc000_003e
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const fn storage_admin_audit_arch() -> u32 {
    0xc000_00b7
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const fn storage_admin_denied_syscalls() -> [u32; 6] {
    [56, 435, 59, 322, 57, 58]
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const fn storage_admin_denied_syscalls() -> [u32; 4] {
    [220, 435, 221, 281]
}

#[cfg(target_os = "linux")]
const fn bpf_stmt(code: u16, k: u32) -> SockFilter {
    SockFilter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

#[cfg(target_os = "linux")]
const fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter { code, jt, jf, k }
}

#[cfg(target_os = "linux")]
pub fn capture_storage_admin_process_evidence(
    workload_cgroup_procs: &Path,
) -> PocResult<StorageAdminProcessEvidence> {
    validate_workload_cgroup_procs(workload_cgroup_procs)?;
    let workload_cgroup_member_pid = std::process::id();
    verify_process_cgroup_membership(workload_cgroup_member_pid, workload_cgroup_procs)?;
    let executable = fs::read_link("/proc/self/exe").map_err(|error| {
        PocError::io(
            "read storage-admin executable identity",
            "/proc/self/exe",
            error,
        )
    })?;
    let executable_sha256 = sha256_file(&executable)?;
    let status = fs::read_to_string("/proc/self/status").map_err(|error| {
        PocError::io(
            "read storage-admin process status",
            "/proc/self/status",
            error,
        )
    })?;
    let namespace_path = Path::new("/proc/self/ns/mnt");
    let mount_namespace_id = fs::read_link(namespace_path)
        .map_err(|error| PocError::io("read storage-admin mount namespace", namespace_path, error))?
        .to_string_lossy()
        .into_owned();
    let mount_namespace_inode = fs::metadata(namespace_path)
        .map_err(|error| PocError::io("stat storage-admin mount namespace", namespace_path, error))?
        .ino();
    storage_admin_process_evidence_from_status(
        executable,
        executable_sha256,
        &status,
        workload_cgroup_procs.to_path_buf(),
        workload_cgroup_member_pid,
        mount_namespace_id,
        mount_namespace_inode,
    )
}

/// Verify one process against its exact unified cgroup-v2 membership.
///
/// Reading `/proc/<pid>/cgroup` is the kernel's race-safe identity check for
/// one process. It avoids scanning every member in `cgroup.procs`, whose cost
/// grows with unrelated processes in the destination cgroup.
#[cfg(target_os = "linux")]
pub fn verify_process_cgroup_membership(pid: u32, expected_cgroup_procs: &Path) -> PocResult<()> {
    validate_workload_cgroup_procs(expected_cgroup_procs)?;
    if pid == 0 {
        return Err(PocError::Integrity(
            "storage-admin cgroup member pid must be non-zero".to_owned(),
        ));
    }
    let cgroup_dir = expected_cgroup_procs.parent().ok_or_else(|| {
        PocError::Integrity("storage-admin cgroup.procs path has no parent".to_owned())
    })?;
    let expected_relative = cgroup_dir.strip_prefix("/sys/fs/cgroup").map_err(|_| {
        PocError::Integrity(format!(
            "storage-admin cgroup.procs path is outside /sys/fs/cgroup: {}",
            expected_cgroup_procs.display()
        ))
    })?;
    let expected_membership = if expected_relative.as_os_str().is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", expected_relative.display())
    };
    let membership_path = PathBuf::from(format!("/proc/{pid}/cgroup"));
    let membership = fs::read_to_string(&membership_path).map_err(|error| {
        PocError::io(
            "read storage-admin process cgroup membership",
            &membership_path,
            error,
        )
    })?;
    let observed_membership = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
        .ok_or_else(|| {
            PocError::Integrity(
                "storage-admin process has no unified cgroup-v2 membership".to_owned(),
            )
        })?;
    require_equal(
        "process cgroup membership",
        observed_membership,
        expected_membership.as_str(),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn verify_process_cgroup_membership(_pid: u32, _expected_cgroup_procs: &Path) -> PocResult<()> {
    Err(PocError::Unsupported(
        "storage-admin cgroup verification requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn sha256_file(path: &Path) -> PocResult<String> {
    let mut file = File::open(path)
        .map_err(|error| PocError::io("open storage-admin executable for hashing", path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            PocError::io("read storage-admin executable for hashing", path, error)
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(&hasher.finalize()))
}

#[cfg(not(target_os = "linux"))]
pub fn capture_storage_admin_process_evidence(
    _workload_cgroup_procs: &Path,
) -> PocResult<StorageAdminProcessEvidence> {
    Err(PocError::Unsupported(
        "storage-admin process evidence requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn verify_process_identity(
    workload_cgroup_procs: &Path,
    profile: StorageAdminCapabilityProfile,
) -> PocResult<StorageAdminProcessEvidence> {
    let evidence = capture_storage_admin_process_evidence(workload_cgroup_procs)?;
    require_equal(
        "executable identity",
        evidence.executable.as_path(),
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
    )?;
    require_equal(
        "effective capability mask",
        &evidence.capabilities.effective,
        &profile.effective_capability_mask(),
    )?;
    require_equal(
        "permitted capability mask",
        &evidence.capabilities.permitted,
        &profile.effective_capability_mask(),
    )?;
    require_equal(
        "bounding capability mask",
        &evidence.capabilities.bounding,
        &profile.effective_capability_mask(),
    )?;
    require_equal(
        "inheritable capability mask",
        &evidence.capabilities.inheritable,
        &0,
    )?;
    require_equal(
        "ambient capability mask",
        &evidence.capabilities.ambient,
        &0,
    )?;
    require_equal("seccomp mode", &evidence.seccomp.mode, &2)?;
    require_equal(
        "seccomp profile",
        evidence.seccomp.profile_id.as_str(),
        STORAGE_ADMIN_SECCOMP_PROFILE_ID,
    )?;
    require_equal(
        "seccomp profile hash",
        evidence.seccomp.profile_sha256.as_str(),
        storage_admin_seccomp_profile_sha256().as_str(),
    )?;
    if evidence.seccomp.filter_count == 0 {
        return Err(PocError::Integrity(
            "storage-admin seccomp filter count must be non-zero".to_owned(),
        ));
    }
    if !evidence.seccomp.no_new_privs {
        return Err(PocError::Integrity(
            "storage-admin NoNewPrivs is not enabled".to_owned(),
        ));
    }
    Ok(evidence)
}

#[cfg(target_os = "linux")]
fn enter_bound_user_and_mount_namespaces(
    holder_pid: u32,
    expected_namespace_id: &str,
) -> PocResult<()> {
    // A mount namespace is owned by a user namespace.  Open and bind both
    // namespace descriptors from the server-validated holder before changing
    // this process so a helper cannot be redirected by later path changes.
    let user_namespace_path = user_namespace_path(holder_pid)?;
    let user_namespace_file = File::open(&user_namespace_path).map_err(|error| {
        PocError::io(
            "open bound storage-admin user namespace",
            &user_namespace_path,
            error,
        )
    })?;
    let opened_user_fd_path =
        PathBuf::from(format!("/proc/self/fd/{}", user_namespace_file.as_raw_fd()));
    let opened_user_namespace = fs::read_link(&opened_user_fd_path).map_err(|error| {
        PocError::io(
            "read opened storage-admin user namespace identity",
            &opened_user_fd_path,
            error,
        )
    })?;
    let opened_user_namespace_id = opened_user_namespace.to_string_lossy().into_owned();
    let opened_user_inode = user_namespace_file
        .metadata()
        .map_err(|error| {
            PocError::io(
                "stat opened storage-admin user namespace",
                &user_namespace_path,
                error,
            )
        })?
        .ino();
    validate_opened_user_namespace(&opened_user_namespace_id, opened_user_inode)?;

    let namespace_path = mount_namespace_path(holder_pid)?;
    let namespace_file = File::open(&namespace_path).map_err(|error| {
        PocError::io(
            "open bound storage-admin mount namespace",
            &namespace_path,
            error,
        )
    })?;
    let opened_fd_path = PathBuf::from(format!("/proc/self/fd/{}", namespace_file.as_raw_fd()));
    let opened_namespace = fs::read_link(&opened_fd_path).map_err(|error| {
        PocError::io(
            "read opened storage-admin mount namespace identity",
            &opened_fd_path,
            error,
        )
    })?;
    let opened_inode = namespace_file
        .metadata()
        .map_err(|error| {
            PocError::io(
                "stat opened storage-admin mount namespace",
                &namespace_path,
                error,
            )
        })?
        .ino();
    validate_opened_mount_namespace(
        expected_namespace_id,
        opened_namespace.to_string_lossy().as_ref(),
        opened_inode,
    )?;

    // SAFETY: setns receives an owned namespace fd from the validated holder
    // and the fixed user-namespace type.  Joining it first is required before
    // the holder's mount namespace can be entered.
    let setns_user_result =
        unsafe { libc::setns(user_namespace_file.as_raw_fd(), libc::CLONE_NEWUSER) };
    if setns_user_result != 0 {
        return Err(PocError::Integrity(format!(
            "failed to enter bound storage-admin user namespace: {}",
            std::io::Error::last_os_error()
        )));
    }
    let current_user_namespace_path = Path::new("/proc/self/ns/user");
    let current_user_namespace = fs::read_link(current_user_namespace_path).map_err(|error| {
        PocError::io(
            "read entered storage-admin user namespace",
            current_user_namespace_path,
            error,
        )
    })?;
    require_equal(
        "entered user namespace",
        current_user_namespace.to_string_lossy().as_ref(),
        opened_user_namespace_id.as_str(),
    )?;

    // SAFETY: setns receives an owned namespace fd and the fixed mount-namespace type.
    let setns_result = unsafe { libc::setns(namespace_file.as_raw_fd(), libc::CLONE_NEWNS) };
    if setns_result != 0 {
        return Err(PocError::Integrity(format!(
            "failed to enter bound storage-admin mount namespace: {}",
            std::io::Error::last_os_error()
        )));
    }

    let current_namespace_path = Path::new("/proc/self/ns/mnt");
    let current_namespace_id = fs::read_link(current_namespace_path)
        .map_err(|error| {
            PocError::io(
                "read entered storage-admin mount namespace",
                current_namespace_path,
                error,
            )
        })?
        .to_string_lossy()
        .into_owned();
    require_equal(
        "entered mount namespace",
        current_namespace_id.as_str(),
        expected_namespace_id,
    )?;
    let expected_inode = parse_mount_namespace_inode(expected_namespace_id)?;
    let current_namespace_inode = fs::metadata(current_namespace_path)
        .map_err(|error| {
            PocError::io(
                "stat entered storage-admin mount namespace",
                current_namespace_path,
                error,
            )
        })?
        .ino();
    require_equal(
        "entered mount namespace inode",
        &current_namespace_inode,
        &expected_inode,
    )
}

fn parse_status_hex(status: &str, field: &str) -> PocResult<u64> {
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|line| line.strip_prefix(':'))
        .map(str::trim)
        .ok_or_else(|| PocError::Integrity(format!("process status is missing {field}")))?;
    u64::from_str_radix(value, 16)
        .map_err(|error| PocError::Integrity(format!("invalid {field} value: {error}")))
}

fn parse_status_u32(status: &str, field: &str) -> PocResult<u32> {
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|line| line.strip_prefix(':'))
        .map(str::trim)
        .ok_or_else(|| PocError::Integrity(format!("process status is missing {field}")))?;
    value
        .parse::<u32>()
        .map_err(|error| PocError::Integrity(format!("invalid {field} value: {error}")))
}

fn load_mount_receipt_attestation(
    scope: &StorageAdminScope,
    profile: StorageAdminCapabilityProfile,
    binding: &StorageAdminMountReceiptBinding,
) -> PocResult<StorageAdminMountAttestation> {
    validate_mount_receipt_binding_for_action(StorageAdminAction::Quiesce, Some(binding))?;
    let request = StorageAdminRequest {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        profile_id: profile.profile_id().to_owned(),
        operation_id: binding.storage_operation_id.clone(),
        action: StorageAdminAction::Mount,
        scope: scope.clone(),
    };
    let selection = StorageAdminSelection {
        request_sha256: request_sha256(&request)?,
        request,
        profile,
    };
    let paths = operation_paths(&selection.request)?;
    if !paths.attempt.exists() {
        return Err(PocError::Integrity(
            "mount authority receipt is missing its immutable attempt".to_owned(),
        ));
    }
    let receipt: StorageAdminReceipt = read_json(&paths.receipt)?;
    validate_stored_receipt(&receipt, &selection, &paths.receipt)?;
    require_equal(
        "mount authority receipt outcome",
        &receipt.outcome,
        &StorageAdminOutcome::Succeeded,
    )?;
    let attestation = receipt.mount_attestation.ok_or_else(|| {
        PocError::Integrity("mount authority receipt is missing its attestation".to_owned())
    })?;
    require_equal(
        "mount authority attestation digest",
        &storage_admin_mount_attestation_sha256(&attestation)?,
        &binding.attestation_sha256,
    )?;
    Ok(attestation)
}

#[cfg(target_os = "linux")]
pub fn validate_storage_admin_destroy_authority(
    scope: &StorageAdminScope,
    profile: StorageAdminCapabilityProfile,
    binding: &StorageAdminMountReceiptBinding,
    cleanup_operation_id: &OperationId,
    mount_namespace_holder_pid: u32,
) -> PocResult<()> {
    validate_mount_namespace_holder_pid(mount_namespace_holder_pid)?;
    let attestation = load_mount_receipt_attestation(scope, profile, binding)?;
    validate_attestation_scope(scope, &attestation)?;

    let cleanup_request = StorageAdminRequest {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        profile_id: profile.profile_id().to_owned(),
        operation_id: cleanup_operation_id.clone(),
        action: StorageAdminAction::Cleanup,
        scope: scope.clone(),
    };
    let cleanup_selection = StorageAdminSelection {
        request_sha256: request_sha256(&cleanup_request)?,
        request: cleanup_request,
        profile,
    };
    let cleanup_paths = operation_paths(&cleanup_selection.request)?;
    validate_publication_sequence_cleanup_attempt(&cleanup_paths, &cleanup_selection)?;
    let cleanup_receipt: StorageAdminReceipt = read_json(&cleanup_paths.receipt)?;
    validate_stored_receipt(&cleanup_receipt, &cleanup_selection, &cleanup_paths.receipt)?;
    require_equal(
        "destroy authority cleanup outcome",
        &cleanup_receipt.outcome,
        &StorageAdminOutcome::Succeeded,
    )?;
    require_equal(
        "destroy authority cleanup completion",
        &cleanup_receipt.cleanup_complete,
        &true,
    )?;
    require_equal(
        "destroy authority mount receipt binding",
        cleanup_receipt
            .mount_receipt_binding
            .as_ref()
            .ok_or_else(|| {
                PocError::Integrity(
                    "destroy authority cleanup receipt lost mount authority".to_owned(),
                )
            })?,
        binding,
    )?;

    let namespace_path = mount_namespace_path(mount_namespace_holder_pid)?;
    let namespace_file = File::open(&namespace_path).map_err(|error| {
        PocError::io(
            "open destroy-authority mount namespace",
            &namespace_path,
            error,
        )
    })?;
    let opened_fd_path = PathBuf::from(format!("/proc/self/fd/{}", namespace_file.as_raw_fd()));
    let opened_namespace = fs::read_link(&opened_fd_path).map_err(|error| {
        PocError::io(
            "read destroy-authority mount namespace identity",
            &opened_fd_path,
            error,
        )
    })?;
    let opened_inode = namespace_file
        .metadata()
        .map_err(|error| {
            PocError::io(
                "stat destroy-authority mount namespace",
                &namespace_path,
                error,
            )
        })?
        .ino();
    validate_opened_mount_namespace(
        &scope.mount_namespace_id,
        opened_namespace.to_string_lossy().as_ref(),
        opened_inode,
    )?;
    require_equal(
        "destroy-authority attested namespace inode",
        &attestation.mount_namespace_inode,
        &opened_inode,
    )?;

    let holder_mountinfo = PathBuf::from(format!("/proc/{mount_namespace_holder_pid}/mountinfo"));
    let observation = capture_storage_admin_mountinfo_from_path(scope, &holder_mountinfo)?;
    if observation.target.is_some() {
        return Err(PocError::Integrity(
            "destroy authority found a mount at the attested workspace target".to_owned(),
        ));
    }
    require_equal(
        "destroy authority target-absence digest",
        &observation.sha256,
        &storage_admin_mountinfo_target_sha256(None)?,
    )?;

    let current_namespace = fs::read_link(&namespace_path).map_err(|error| {
        PocError::io(
            "reread destroy-authority mount namespace",
            &namespace_path,
            error,
        )
    })?;
    require_equal(
        "destroy-authority stable holder namespace",
        current_namespace.to_string_lossy().as_ref(),
        opened_namespace.to_string_lossy().as_ref(),
    )
}

fn validate_destroy_authority_attempt(
    attempt: &StorageAdminAttempt,
    expected_action: StorageAdminAction,
    expected_scope: &StorageAdminScope,
) -> PocResult<()> {
    validate_request(&attempt.request)?;
    require_equal(
        "destroy authority sequence durability",
        &attempt.durability,
        &StorageAdminDurability::ExactObjectGraph,
    )?;
    require_equal(
        "destroy authority sequence action",
        &attempt.request.action,
        &expected_action,
    )?;
    require_equal(
        "destroy authority sequence scope",
        &attempt.request.scope,
        expected_scope,
    )?;
    require_equal(
        "destroy authority sequence request digest",
        &attempt.request_sha256,
        &request_sha256(&attempt.request)?,
    )?;
    validate_authorization(
        &attempt.request,
        &attempt.authorization,
        &attempt.authorization.actor_id,
    )?;
    validate_sha256(
        "destroy authority sequence trusted executable hash",
        &attempt.trusted_executable_sha256,
    )?;
    validate_workload_cgroup_procs(&attempt.workload_cgroup_procs)?;
    validate_mount_namespace_holder_pid(attempt.mount_namespace_holder_pid)?;
    validate_mount_receipt_binding_for_action(
        attempt.request.action,
        attempt.mount_receipt_binding.as_ref(),
    )?;
    if attempt.started_unix_ms == 0 {
        return Err(PocError::Integrity(
            "destroy authority sequence attempt has a zero start timestamp".to_owned(),
        ));
    }
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn validate_publication_sequence_cleanup_attempt(
    cleanup_paths: &OperationPaths,
    cleanup_selection: &StorageAdminSelection,
) -> PocResult<()> {
    let root = cleanup_paths
        .lock
        .parent()
        .ok_or_else(|| PocError::Integrity("storage-admin lock has no parent".to_owned()))?;
    let attempts_path =
        publication_sequence_attempts_path(root, &cleanup_selection.request.operation_id)?;
    if attempts_path.exists() {
        let attempts: PublicationSequenceAttempts = read_json(&attempts_path)?;
        require_equal(
            "destroy authority sequence attempt schema version",
            &attempts.schema_version,
            &SCHEMA_VERSION,
        )?;
        require_equal(
            "destroy authority sequence attempt interface version",
            attempts.interface_version.as_str(),
            INTERFACE_VERSION,
        )?;
        require_equal(
            "destroy authority sequence attempt count",
            &attempts.attempts.len(),
            &3_usize,
        )?;
        let expected_actions = [
            StorageAdminAction::Quiesce,
            StorageAdminAction::StrictUnmount,
            StorageAdminAction::Cleanup,
        ];
        for (attempt, expected_action) in attempts.attempts.iter().zip(expected_actions) {
            validate_destroy_authority_attempt(
                attempt,
                expected_action,
                &cleanup_selection.request.scope,
            )?;
        }
        let cleanup_attempt = &attempts.attempts[2];
        require_equal(
            "destroy authority cleanup attempt operation id",
            &cleanup_attempt.operation_id,
            &cleanup_selection.request.operation_id,
        )?;
        return require_equal(
            "destroy authority cleanup attempt request",
            &cleanup_attempt.request,
            &cleanup_selection.request,
        );
    }

    if !cleanup_paths.attempt.exists() {
        return Err(PocError::Integrity(
            "destroy authority cleanup receipt is missing its immutable sequence or legacy attempt"
                .to_owned(),
        ));
    }
    let cleanup_attempt: StorageAdminAttempt = read_json(&cleanup_paths.attempt)?;
    validate_destroy_authority_attempt(
        &cleanup_attempt,
        StorageAdminAction::Cleanup,
        &cleanup_selection.request.scope,
    )?;
    require_equal(
        "destroy authority cleanup attempt operation id",
        &cleanup_attempt.operation_id,
        &cleanup_selection.request.operation_id,
    )?;
    require_equal(
        "destroy authority cleanup attempt request",
        &cleanup_attempt.request,
        &cleanup_selection.request,
    )
}

#[cfg(not(target_os = "linux"))]
pub fn validate_storage_admin_destroy_authority(
    _scope: &StorageAdminScope,
    _profile: StorageAdminCapabilityProfile,
    _binding: &StorageAdminMountReceiptBinding,
    _cleanup_operation_id: &OperationId,
    _mount_namespace_holder_pid: u32,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "storage-admin destroy authority requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn execute_platform_action(
    action: StorageAdminAction,
    scope: &StorageAdminScope,
    mounted_by_this_process: &mut Option<PathBuf>,
    workspace_prequiesced: bool,
) -> PocResult<()> {
    match action {
        StorageAdminAction::Mount => Err(PocError::Integrity(
            "mount must use the attested overlay attachment path".to_owned(),
        )),
        StorageAdminAction::Quiesce => syncfs_path(&scope.workspace_root),
        StorageAdminAction::StrictUnmount => {
            if !workspace_prequiesced {
                syncfs_path(&scope.workspace_root)?;
            }
            strict_unmount_path(&scope.workspace_root)
        }
        StorageAdminAction::Cleanup => cleanup_platform_state(scope, mounted_by_this_process),
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(not(target_os = "linux"))]
fn execute_platform_action(
    _action: StorageAdminAction,
    _scope: &StorageAdminScope,
    _mounted_by_this_process: &mut Option<PathBuf>,
    _workspace_prequiesced: bool,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "storage-admin lifecycle syscalls require Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn mount_overlay_with_attestation(
    scope: &StorageAdminScope,
    selection: &StorageAdminSelection,
    process: &StorageAdminProcessEvidence,
    mount_plan: &StorageAdminMountPlanEvidence,
    mounted_by_this_process: &mut Option<PathBuf>,
) -> PocResult<(StorageAdminMountAttestation, StorageAdminMountTableEvidence)> {
    let upper_dir = scope.allocation_root.join("upper");
    let work_dir = scope.allocation_root.join("work");
    require_directory("allocation upper directory", &upper_dir)?;
    require_directory("allocation work directory", &work_dir)?;
    for lower_dir in &scope.lower_dirs_newest_first {
        require_directory("lower directory", lower_dir)?;
    }
    fs::create_dir_all(&scope.workspace_root).map_err(|error| {
        PocError::io(
            "create storage-admin workspace root",
            &scope.workspace_root,
            error,
        )
    })?;
    if mount_plan.mountinfo_before.target.is_some() {
        return Err(PocError::Integrity(
            "storage-admin workspace target was already mounted before attach".to_owned(),
        ));
    }
    let (mount, inspection) = mount_kernel_overlay_with_lower_inspection(
        &scope.workspace_root,
        &OverlayHandle {
            upperdir: upper_dir,
            workdir: work_dir,
            layer_paths: scope.lower_dirs_newest_first.clone(),
        },
        |lower_bindings| {
            capture_mount_attestation(scope, selection, process, mount_plan, lower_bindings)
        },
    )?;
    let (attestation, observation) = match inspection {
        Ok(value) => value,
        Err(error) => {
            std::mem::forget(mount);
            return match strict_unmount_path(&scope.workspace_root) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(PocError::RecoveryRequired(format!(
                    "post-attach attestation failed: {error}; strict cleanup failed: {cleanup_error}"
                ))),
            };
        }
    };
    std::mem::forget(mount);
    *mounted_by_this_process = Some(scope.workspace_root.clone());
    Ok((attestation, observation))
}

#[cfg(not(target_os = "linux"))]
fn mount_overlay_with_attestation(
    _scope: &StorageAdminScope,
    _selection: &StorageAdminSelection,
    _process: &StorageAdminProcessEvidence,
    _mount_plan: &StorageAdminMountPlanEvidence,
    _mounted_by_this_process: &mut Option<PathBuf>,
) -> PocResult<(StorageAdminMountAttestation, StorageAdminMountTableEvidence)> {
    Err(PocError::Unsupported(
        "storage-admin mount attestation requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn capture_mount_attestation(
    scope: &StorageAdminScope,
    selection: &StorageAdminSelection,
    process: &StorageAdminProcessEvidence,
    mount_plan: &StorageAdminMountPlanEvidence,
    opened_lowers: &[OpenedLowerBinding],
) -> PocResult<(StorageAdminMountAttestation, StorageAdminMountTableEvidence)> {
    if opened_lowers.len() != scope.lower_dirs_newest_first.len() {
        return Err(PocError::Integrity(
            "opened lower stack length does not match authorized scope".to_owned(),
        ));
    }
    let mut lower_bindings = Vec::with_capacity(opened_lowers.len());
    for (index, (opened, authorized_path)) in opened_lowers
        .iter()
        .zip(&scope.lower_dirs_newest_first)
        .enumerate()
    {
        require_equal("opened lower order", &opened.index, &index)?;
        require_equal(
            "opened lower authorized path",
            &opened.authorized_path,
            authorized_path,
        )?;
        let fd_identity = storage_path_identity(&opened.fd_identity);
        let authorized_path_identity = storage_path_identity(&opened.authorized_path_identity);
        require_equal(
            "opened lower physical authorization proof",
            &fd_identity,
            &authorized_path_identity,
        )?;
        lower_bindings.push(StorageAdminLowerBinding {
            index,
            authorized_path_sha256: storage_admin_authorized_path_sha256(authorized_path),
            fd_identity,
            authorized_path_identity,
        });
    }
    let observation = capture_storage_admin_mountinfo(scope)?;
    validate_mount_table_evidence("attached", &observation, mount_plan)?;
    let observed = observation.target.as_ref().ok_or_else(|| {
        PocError::Integrity("attached workspace mount is missing from mountinfo".to_owned())
    })?;
    let target_identity = capture_path_identity(&scope.workspace_root)?;
    require_equal(
        "attached workspace mount id",
        &target_identity.mount_id,
        &observed.mount_id,
    )?;
    let observed_upper = observed.upper_dir.as_deref().ok_or_else(|| {
        PocError::Integrity("attached workspace mount is missing upperdir".to_owned())
    })?;
    let observed_work = observed.work_dir.as_deref().ok_or_else(|| {
        PocError::Integrity("attached workspace mount is missing workdir".to_owned())
    })?;
    let attestation = StorageAdminMountAttestation {
        schema_version: SCHEMA_VERSION,
        run_id: scope.run_id.clone(),
        sandbox_id: scope.sandbox_id.clone(),
        workspace_session_id: scope.workspace_session_id.clone(),
        session_id: scope.session_id.clone(),
        allocation_id: scope.allocation_id.clone(),
        lease_id: scope.lease_id.clone(),
        lease_epoch: scope.lease_epoch,
        mount_namespace_id: scope.mount_namespace_id.clone(),
        mount_namespace_inode: process.mount_namespace_inode,
        storage_operation_id: selection.request.operation_id.clone(),
        request_sha256: selection.request_sha256.clone(),
        lower_bindings_newest_first: lower_bindings,
        target: StorageAdminTargetBinding {
            workspace_target: scope.workspace_root.clone(),
            mount_namespace_id: scope.mount_namespace_id.clone(),
            mount_namespace_inode: process.mount_namespace_inode,
            mount_id: observed.mount_id,
            mountinfo_sha256: observation.sha256.clone(),
            target_identity,
            filesystem_type: observed.filesystem_type.clone(),
            source: observed.source.clone(),
            mount_options: observed.mount_options.clone(),
            super_options: observed.super_options.clone(),
            expected_upperdir_sha256: storage_admin_authorized_path_sha256(&mount_plan.upper_dir),
            observed_upperdir_sha256: storage_admin_authorized_path_sha256(observed_upper),
            expected_workdir_sha256: storage_admin_authorized_path_sha256(&mount_plan.work_dir),
            observed_workdir_sha256: storage_admin_authorized_path_sha256(observed_work),
        },
        profile_id: selection.profile_id().to_owned(),
        effective_capabilities: owned_strings(selection.profile().effective_capabilities()),
    };
    validate_mount_attestation(
        &attestation,
        selection,
        process,
        &StorageAdminMountPlanEvidence {
            mountinfo_after: observation.clone(),
            ..mount_plan.clone()
        },
    )?;
    Ok((attestation, observation))
}

#[cfg(target_os = "linux")]
fn storage_path_identity(identity: &OpenedPathIdentity) -> StorageAdminPathIdentity {
    StorageAdminPathIdentity {
        mount_id: identity.mount_id,
        device_major: identity.device_major,
        device_minor: identity.device_minor,
        inode: identity.inode,
    }
}

#[cfg(target_os = "linux")]
fn capture_path_identity(path: &Path) -> PocResult<StorageAdminPathIdentity> {
    let stat = statx(
        rustix::fs::CWD,
        path,
        AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
        StatxFlags::BASIC_STATS | StatxFlags::MNT_ID,
    )
    .map_err(|error| PocError::io("statx storage-admin path identity", path, error.into()))?;
    if stat.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        return Err(PocError::Integrity(
            "storage-admin path identity does not expose a mount id".to_owned(),
        ));
    }
    Ok(StorageAdminPathIdentity {
        mount_id: stat.stx_mnt_id,
        device_major: stat.stx_dev_major,
        device_minor: stat.stx_dev_minor,
        inode: stat.stx_ino,
    })
}

#[cfg(target_os = "linux")]
fn validate_target_before_action(
    action: StorageAdminAction,
    scope: &StorageAdminScope,
    attestation: &StorageAdminMountAttestation,
) -> PocResult<()> {
    validate_attestation_scope(scope, attestation)?;
    match action {
        StorageAdminAction::Quiesce | StorageAdminAction::StrictUnmount => {
            validate_current_attested_target(scope, attestation)
        }
        StorageAdminAction::Cleanup => require_target_absent(scope),
        StorageAdminAction::Mount => Err(PocError::Integrity(
            "mount cannot consume prior mount authority".to_owned(),
        )),
    }
}

#[cfg(not(target_os = "linux"))]
fn validate_target_before_action(
    _action: StorageAdminAction,
    _scope: &StorageAdminScope,
    _attestation: &StorageAdminMountAttestation,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "storage-admin target validation requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn validate_target_after_action(
    action: StorageAdminAction,
    scope: &StorageAdminScope,
) -> PocResult<()> {
    match action {
        StorageAdminAction::Quiesce => Ok(()),
        StorageAdminAction::StrictUnmount | StorageAdminAction::Cleanup => {
            require_target_absent(scope)
        }
        StorageAdminAction::Mount => Ok(()),
    }
}

#[cfg(not(target_os = "linux"))]
fn validate_target_after_action(
    _action: StorageAdminAction,
    _scope: &StorageAdminScope,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "storage-admin target validation requires Linux".to_owned(),
    ))
}

fn validate_attestation_scope(
    scope: &StorageAdminScope,
    attestation: &StorageAdminMountAttestation,
) -> PocResult<()> {
    require_equal("attested run id", &attestation.run_id, &scope.run_id)?;
    require_equal(
        "attested sandbox id",
        &attestation.sandbox_id,
        &scope.sandbox_id,
    )?;
    require_equal(
        "attested workspace session id",
        &attestation.workspace_session_id,
        &scope.workspace_session_id,
    )?;
    require_equal(
        "attested session id",
        &attestation.session_id,
        &scope.session_id,
    )?;
    require_equal(
        "attested allocation id",
        &attestation.allocation_id,
        &scope.allocation_id,
    )?;
    require_equal("attested lease id", &attestation.lease_id, &scope.lease_id)?;
    require_equal(
        "attested lease epoch",
        &attestation.lease_epoch,
        &scope.lease_epoch,
    )?;
    require_equal(
        "attested mount namespace",
        &attestation.mount_namespace_id,
        &scope.mount_namespace_id,
    )?;
    require_equal(
        "attested workspace target",
        &attestation.target.workspace_target,
        &scope.workspace_root,
    )
}

#[cfg(target_os = "linux")]
fn validate_current_attested_target(
    scope: &StorageAdminScope,
    attestation: &StorageAdminMountAttestation,
) -> PocResult<()> {
    let observation = capture_storage_admin_mountinfo(scope)?;
    let observed = observation.target.as_ref().ok_or_else(|| {
        PocError::Integrity("attested workspace target is no longer mounted".to_owned())
    })?;
    let expected = &attestation.target;
    require_equal("attested mount id", &observed.mount_id, &expected.mount_id)?;
    require_equal(
        "attested mountinfo digest",
        &observation.sha256,
        &expected.mountinfo_sha256,
    )?;
    require_equal(
        "attested filesystem",
        &observed.filesystem_type,
        &expected.filesystem_type,
    )?;
    require_equal("attested source", &observed.source, &expected.source)?;
    require_equal(
        "attested mount options",
        &observed.mount_options,
        &expected.mount_options,
    )?;
    require_equal(
        "attested super options",
        &observed.super_options,
        &expected.super_options,
    )?;
    require_equal(
        "attested upperdir",
        &storage_admin_authorized_path_sha256(observed.upper_dir.as_deref().ok_or_else(|| {
            PocError::Integrity("current attested target is missing upperdir".to_owned())
        })?),
        &expected.observed_upperdir_sha256,
    )?;
    require_equal(
        "attested workdir",
        &storage_admin_authorized_path_sha256(observed.work_dir.as_deref().ok_or_else(|| {
            PocError::Integrity("current attested target is missing workdir".to_owned())
        })?),
        &expected.observed_workdir_sha256,
    )?;
    require_equal(
        "attested target identity",
        &capture_path_identity(&scope.workspace_root)?,
        &expected.target_identity,
    )
}

#[cfg(not(target_os = "linux"))]
fn validate_current_attested_target(
    _scope: &StorageAdminScope,
    _attestation: &StorageAdminMountAttestation,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "storage-admin target validation requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn require_target_absent(scope: &StorageAdminScope) -> PocResult<()> {
    if capture_storage_admin_mountinfo(scope)?.target.is_none() {
        Ok(())
    } else {
        Err(PocError::Integrity(
            "workspace target is mounted where attested absence is required".to_owned(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn require_target_absent(_scope: &StorageAdminScope) -> PocResult<()> {
    Err(PocError::Unsupported(
        "storage-admin target validation requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn require_directory(label: &'static str, path: &Path) -> PocResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| PocError::io(label, path, error))?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "storage-admin {label} is not a real directory: {}",
            path.display()
        )))
    }
}

#[cfg(target_os = "linux")]
fn syncfs_path(path: &Path) -> PocResult<()> {
    let file = fs::File::open(path)
        .map_err(|error| PocError::io("open storage-admin syncfs target", path, error))?;
    let result = unsafe { libc::syncfs(file.as_raw_fd()) };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::io(
            "syncfs storage-admin workspace",
            path,
            std::io::Error::last_os_error(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn syncfs_path(_path: &Path) -> PocResult<()> {
    Err(PocError::Unsupported(
        "storage-admin syncfs requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn strict_unmount_path(path: &Path) -> PocResult<()> {
    let path_c_string = path_c_string(path)?;
    let result = unsafe { libc::umount2(path_c_string.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::io(
            "strictly unmount storage-admin workspace",
            path,
            std::io::Error::last_os_error(),
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn strict_unmount_path(_path: &Path) -> PocResult<()> {
    Err(PocError::Unsupported(
        "strict storage-admin unmount requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn cleanup_platform_state(
    scope: &StorageAdminScope,
    mounted_by_this_process: &mut Option<PathBuf>,
) -> PocResult<()> {
    let unmount = strict_unmount_path(&scope.workspace_root);
    if let Err(PocError::Io { source, .. }) = &unmount {
        if !matches!(
            source.raw_os_error(),
            Some(libc::EINVAL) | Some(libc::ENOENT)
        ) {
            return unmount;
        }
    } else {
        unmount?;
    }
    *mounted_by_this_process = None;
    match fs::remove_dir(&scope.workspace_root) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(PocError::io(
            "remove storage-admin workspace temporary",
            &scope.workspace_root,
            error,
        )),
    }
}

#[cfg(not(target_os = "linux"))]
fn cleanup_platform_state(
    _scope: &StorageAdminScope,
    mounted_by_this_process: &mut Option<PathBuf>,
) -> PocResult<()> {
    *mounted_by_this_process = None;
    Ok(())
}

#[cfg(target_os = "linux")]
fn path_c_string(path: &Path) -> PocResult<std::ffi::CString> {
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|error| {
        PocError::Integrity(format!(
            "storage-admin path contains NUL at {}: {error}",
            path.display()
        ))
    })
}
