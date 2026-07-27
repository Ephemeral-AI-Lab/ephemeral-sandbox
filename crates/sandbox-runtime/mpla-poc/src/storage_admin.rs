use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::fs::File;
use std::fs::{self, OpenOptions};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{read_json, write_immutable_json, FileLock};
use crate::{
    unix_time_ms, PocError, PocResult, StorageAdminAction, StorageAdminAuthorization,
    StorageAdminOutcome, StorageAdminReceipt, StorageAdminRequest, StorageAdminScope,
    INTERFACE_VERSION, SCHEMA_VERSION, STORAGE_ADMIN_EFFECTIVE_CAPABILITIES,
    STORAGE_ADMIN_PRIVILEGED_SYSCALLS, STORAGE_ADMIN_PROFILE_ID, STORAGE_ADMIN_TRUSTED_EXECUTABLE,
};

const STORAGE_ADMIN_DIRECTORY: &str = "storage-admin";
const ATTEMPT_FILE: &str = "ATTEMPT.json";
const RECEIPT_FILE: &str = "RECEIPT.json";
const LOCK_FILE: &str = "LOCK";
const MAX_INVOCATION_BYTES: usize = 1024 * 1024;
const CAP_SYS_ADMIN_BIT: u64 = 1 << 21;
pub const STORAGE_ADMIN_SECCOMP_PROFILE_ID: &str = "mpla-storage-admin-v1-seccomp-v1";
#[cfg(target_os = "linux")]
const CAP_SYS_ADMIN_NUMBER: u32 = 21;
#[cfg(target_os = "linux")]
const CAPABILITY_WORDS: usize = 2;
#[cfg(target_os = "linux")]
const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
#[cfg(target_os = "linux")]
const PR_CAP_AMBIENT: libc::c_int = 47;
#[cfg(target_os = "linux")]
const PR_CAP_AMBIENT_CLEAR_ALL: libc::c_ulong = 4;
#[cfg(target_os = "linux")]
const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageAdminInvocation {
    pub expected_request: StorageAdminRequest,
    pub request: StorageAdminRequest,
    pub authorization: StorageAdminAuthorization,
    pub trusted_actor_id: String,
    pub mount_namespace_holder_pid: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageAdminSelection {
    request: StorageAdminRequest,
    request_sha256: String,
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
        STORAGE_ADMIN_PROFILE_ID
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
    NarrowCapabilityMasks,
    SetNoNewPrivileges,
    VerifyExecutableAndCapabilityIdentity,
    OpenAndValidateBoundMountNamespace,
    EnterBoundMountNamespace,
    VerifyEnteredMountNamespace,
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
    pub mode: u32,
    pub filter_count: u32,
    pub no_new_privs: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageAdminProcessEvidence {
    pub executable: PathBuf,
    pub capabilities: StorageAdminCapabilitySetEvidence,
    pub seccomp: StorageAdminSeccompEvidence,
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
            StorageAdminPreparationStep::NarrowCapabilityMasks,
            StorageAdminPreparationStep::SetNoNewPrivileges,
            StorageAdminPreparationStep::VerifyExecutableAndCapabilityIdentity,
            StorageAdminPreparationStep::OpenAndValidateBoundMountNamespace,
            StorageAdminPreparationStep::EnterBoundMountNamespace,
            StorageAdminPreparationStep::VerifyEnteredMountNamespace,
        ]
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
    mount_namespace_holder_pid: u32,
    started_unix_ms: u64,
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
}

impl StorageAdminLifecycle for PlatformStorageLifecycle {
    fn execute(
        &mut self,
        action: StorageAdminAction,
        scope: &StorageAdminScope,
    ) -> StorageAdminExecution {
        match execute_platform_action(action, scope, &mut self.mounted_by_this_process) {
            Ok(()) => StorageAdminExecution::succeeded(),
            Err(error) => {
                let cleanup = cleanup_platform_state(scope, &mut self.mounted_by_this_process);
                let cleanup_complete = cleanup.is_ok();
                let failure = match cleanup {
                    Ok(()) => error.to_string(),
                    Err(cleanup_error) => format!("{error}; cleanup failed: {cleanup_error}"),
                };
                StorageAdminExecution::failed(failure, cleanup_complete)
            }
        }
    }

    fn recover_incomplete(
        &mut self,
        action: StorageAdminAction,
        scope: &StorageAdminScope,
    ) -> StorageAdminExecution {
        let cleanup = match action {
            StorageAdminAction::Mount
            | StorageAdminAction::StrictUnmount
            | StorageAdminAction::Cleanup => {
                cleanup_platform_state(scope, &mut self.mounted_by_this_process)
            }
            StorageAdminAction::Quiesce => Ok(()),
        };
        match cleanup {
            Ok(()) => StorageAdminExecution::failed(
                format!("recovered incomplete {action:?} operation"),
                true,
            ),
            Err(error) => StorageAdminExecution::failed(
                format!("incomplete {action:?} recovery failed: {error}"),
                false,
            ),
        }
    }

    fn receipt_committed(&mut self, _action: StorageAdminAction, _scope: &StorageAdminScope) {
        self.mounted_by_this_process = None;
    }

    fn cleanup_after_receipt_failure(
        &mut self,
        _action: StorageAdminAction,
        scope: &StorageAdminScope,
    ) -> PocResult<()> {
        cleanup_platform_state(scope, &mut self.mounted_by_this_process)
    }
}

impl Drop for PlatformStorageLifecycle {
    fn drop(&mut self) {
        if let Some(workspace_root) = self.mounted_by_this_process.take() {
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
    validate_mount_namespace_holder_pid(invocation.mount_namespace_holder_pid)?;
    Ok(invocation)
}

pub fn authorize_storage_admin(
    expected: &StorageAdminRequest,
    request: &StorageAdminRequest,
    authorization: &StorageAdminAuthorization,
    trusted_actor_id: &str,
) -> PocResult<StorageAdminSelection> {
    validate_request(expected)?;
    validate_exact_request(expected, request)?;
    validate_authorization(expected, authorization, trusted_actor_id)?;
    Ok(StorageAdminSelection {
        request: request.clone(),
        request_sha256: request_sha256(request)?,
    })
}

pub fn run_storage_admin<L: StorageAdminLifecycle>(
    invocation: &StorageAdminInvocation,
    lifecycle: &mut L,
) -> PocResult<StorageAdminReceipt> {
    validate_mount_namespace_holder_pid(invocation.mount_namespace_holder_pid)?;
    let selection = authorize_storage_admin(
        &invocation.expected_request,
        &invocation.request,
        &invocation.authorization,
        &invocation.trusted_actor_id,
    )?;
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
                mount_namespace_holder_pid: invocation.mount_namespace_holder_pid,
                started_unix_ms,
            },
        )?;
        let execution = lifecycle.execute(selection.request.action, &selection.request.scope);
        return commit_execution(
            &selection,
            lifecycle,
            execution,
            started_unix_ms,
            &paths.receipt,
        );
    };

    let execution =
        lifecycle.recover_incomplete(selection.request.action, &selection.request.scope);
    commit_execution(
        &selection,
        lifecycle,
        execution,
        started_unix_ms,
        &paths.receipt,
    )
}

pub fn run_platform_invocation(
    invocation: &StorageAdminInvocation,
) -> PocResult<StorageAdminReceipt> {
    authorize_storage_admin(
        &invocation.expected_request,
        &invocation.request,
        &invocation.authorization,
        &invocation.trusted_actor_id,
    )?;
    validate_mount_namespace_holder_pid(invocation.mount_namespace_holder_pid)?;
    prepare_platform_process(invocation)?;
    run_storage_admin(invocation, &mut PlatformStorageLifecycle::default())
}

fn commit_execution<L: StorageAdminLifecycle>(
    selection: &StorageAdminSelection,
    lifecycle: &mut L,
    execution: StorageAdminExecution,
    started_unix_ms: u64,
    receipt_path: &Path,
) -> PocResult<StorageAdminReceipt> {
    validate_execution(&execution)?;
    let completed_unix_ms = unix_time_ms()?.max(started_unix_ms);
    let receipt = StorageAdminReceipt {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        profile_id: STORAGE_ADMIN_PROFILE_ID.to_owned(),
        operation_id: selection.request.operation_id.clone(),
        action: selection.request.action,
        request_sha256: selection.request_sha256.clone(),
        trusted_executable: PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
        effective_capabilities: STORAGE_ADMIN_EFFECTIVE_CAPABILITIES
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect(),
        allowed_privileged_syscalls: STORAGE_ADMIN_PRIVILEGED_SYSCALLS
            .iter()
            .map(|syscall| (*syscall).to_owned())
            .collect(),
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
    lifecycle.receipt_committed(selection.request.action, &selection.request.scope);
    Ok(receipt)
}

fn validate_request(request: &StorageAdminRequest) -> PocResult<()> {
    require_equal("schema version", &request.schema_version, &SCHEMA_VERSION)?;
    require_equal(
        "interface version",
        request.interface_version.as_str(),
        INTERFACE_VERSION,
    )?;
    require_equal(
        "profile id",
        request.profile_id.as_str(),
        STORAGE_ADMIN_PROFILE_ID,
    )?;
    validate_path_atom("operation id", request.operation_id.as_str())?;
    validate_scope(&request.scope)
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

fn hex_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

struct OperationPaths {
    directory: PathBuf,
    lock: PathBuf,
    attempt: PathBuf,
    receipt: PathBuf,
}

fn operation_paths(request: &StorageAdminRequest) -> PocResult<OperationPaths> {
    validate_path_atom("operation id", request.operation_id.as_str())?;
    let root = request.scope.control_root.join(STORAGE_ADMIN_DIRECTORY);
    let directory = root.join(request.operation_id.as_str());
    Ok(OperationPaths {
        lock: root.join(LOCK_FILE),
        attempt: directory.join(ATTEMPT_FILE),
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
        Ok(file) => file
            .sync_all()
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
    crate::durable::fsync_dir(root)
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
        "stored attempt mount namespace holder pid",
        &attempt.mount_namespace_holder_pid,
        &invocation.mount_namespace_holder_pid,
    )?;
    if attempt.started_unix_ms == 0 {
        return Err(PocError::Integrity(
            "stored storage-admin attempt has a zero start timestamp".to_owned(),
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
        STORAGE_ADMIN_PROFILE_ID,
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
    let expected_capabilities = owned_strings(STORAGE_ADMIN_EFFECTIVE_CAPABILITIES);
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

pub fn storage_admin_process_evidence_from_status(
    executable: PathBuf,
    status: &str,
    mount_namespace_id: String,
    mount_namespace_inode: u64,
) -> PocResult<StorageAdminProcessEvidence> {
    validate_opened_mount_namespace(
        &mount_namespace_id,
        &mount_namespace_id,
        mount_namespace_inode,
    )?;
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
        capabilities: StorageAdminCapabilitySetEvidence {
            effective: parse_status_hex(status, "CapEff")?,
            permitted: parse_status_hex(status, "CapPrm")?,
            inheritable: parse_status_hex(status, "CapInh")?,
            bounding: parse_status_hex(status, "CapBnd")?,
            ambient: parse_status_hex(status, "CapAmb")?,
        },
        seccomp: StorageAdminSeccompEvidence {
            profile_id: STORAGE_ADMIN_SECCOMP_PROFILE_ID.to_owned(),
            mode: parse_status_u32(status, "Seccomp")?,
            filter_count: parse_status_u32(status, "Seccomp_filters")?,
            no_new_privs,
        },
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
        source: "overlay".to_owned(),
        filesystem_type: "overlay".to_owned(),
        target: scope.workspace_root.clone(),
        flags: vec!["MS_NODEV".to_owned(), "MS_NOSUID".to_owned()],
        lower_dirs_newest_first: scope.lower_dirs_newest_first.clone(),
        upper_dir: scope.allocation_root.join("upper"),
        work_dir: scope.allocation_root.join("work"),
    })
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
            "mount_namespace_holder_pid",
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
fn prepare_platform_process(invocation: &StorageAdminInvocation) -> PocResult<()> {
    narrow_process_capabilities()?;
    set_no_new_privileges()?;
    verify_process_identity()?;
    enter_bound_mount_namespace(
        invocation.mount_namespace_holder_pid,
        &invocation.request.scope.mount_namespace_id,
    )
}

#[cfg(not(target_os = "linux"))]
fn prepare_platform_process(_invocation: &StorageAdminInvocation) -> PocResult<()> {
    Err(PocError::Unsupported(
        "mpla-storage-admin-v1 execution requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn narrow_process_capabilities() -> PocResult<()> {
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

    let header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapabilityData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; CAPABILITY_WORDS];
    let word = (CAP_SYS_ADMIN_NUMBER / 32) as usize;
    let bit = 1_u32 << (CAP_SYS_ADMIN_NUMBER % 32);
    data[word].effective = bit;
    data[word].permitted = bit;

    // SAFETY: capset reads the fixed header and two-word capability array for this process.
    let result = unsafe { libc::syscall(libc::SYS_capset, &header, data.as_mut_ptr()) };
    if result != 0 {
        return Err(PocError::Integrity(format!(
            "failed to narrow storage-admin capability masks: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
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

#[cfg(target_os = "linux")]
pub fn capture_storage_admin_process_evidence() -> PocResult<StorageAdminProcessEvidence> {
    let executable = fs::read_link("/proc/self/exe").map_err(|error| {
        PocError::io(
            "read storage-admin executable identity",
            "/proc/self/exe",
            error,
        )
    })?;
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
        &status,
        mount_namespace_id,
        mount_namespace_inode,
    )
}

#[cfg(not(target_os = "linux"))]
pub fn capture_storage_admin_process_evidence() -> PocResult<StorageAdminProcessEvidence> {
    Err(PocError::Unsupported(
        "storage-admin process evidence requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn verify_process_identity() -> PocResult<()> {
    let evidence = capture_storage_admin_process_evidence()?;
    require_equal(
        "executable identity",
        evidence.executable.as_path(),
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
    )?;
    require_equal(
        "effective capability mask",
        &evidence.capabilities.effective,
        &CAP_SYS_ADMIN_BIT,
    )?;
    require_equal(
        "permitted capability mask",
        &evidence.capabilities.permitted,
        &CAP_SYS_ADMIN_BIT,
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
    Ok(())
}

#[cfg(target_os = "linux")]
fn enter_bound_mount_namespace(holder_pid: u32, expected_namespace_id: &str) -> PocResult<()> {
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

    // SAFETY: setns receives an owned namespace fd and the fixed mount-namespace type.
    let setns_result = unsafe { libc::setns(namespace_file.as_raw_fd(), libc::CLONE_NEWNS) };
    if setns_result != 0 {
        return Err(PocError::Integrity(format!(
            "failed to enter bound storage-admin mount namespace: {}",
            std::io::Error::last_os_error()
        )));
    }

    let current_evidence = capture_storage_admin_process_evidence()?;
    require_equal(
        "entered mount namespace",
        current_evidence.mount_namespace_id.as_str(),
        expected_namespace_id,
    )?;
    let expected_inode = parse_mount_namespace_inode(expected_namespace_id)?;
    require_equal(
        "entered mount namespace inode",
        &current_evidence.mount_namespace_inode,
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

#[cfg(target_os = "linux")]
fn execute_platform_action(
    action: StorageAdminAction,
    scope: &StorageAdminScope,
    mounted_by_this_process: &mut Option<PathBuf>,
) -> PocResult<()> {
    match action {
        StorageAdminAction::Mount => mount_overlay(scope, mounted_by_this_process),
        StorageAdminAction::Quiesce => syncfs_path(&scope.workspace_root),
        StorageAdminAction::StrictUnmount => {
            syncfs_path(&scope.workspace_root)?;
            strict_unmount_path(&scope.workspace_root)
        }
        StorageAdminAction::Cleanup => cleanup_platform_state(scope, mounted_by_this_process),
    }
}

#[cfg(not(target_os = "linux"))]
fn execute_platform_action(
    _action: StorageAdminAction,
    _scope: &StorageAdminScope,
    _mounted_by_this_process: &mut Option<PathBuf>,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "storage-admin lifecycle syscalls require Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn mount_overlay(
    scope: &StorageAdminScope,
    mounted_by_this_process: &mut Option<PathBuf>,
) -> PocResult<()> {
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
    let mut lower = Vec::new();
    for (index, path) in scope.lower_dirs_newest_first.iter().enumerate() {
        if index > 0 {
            lower.push(b':');
        }
        lower.extend_from_slice(path.as_os_str().as_bytes());
    }
    let mut options = Vec::new();
    options.extend_from_slice(b"lowerdir=");
    options.extend_from_slice(&lower);
    options.extend_from_slice(b",upperdir=");
    options.extend_from_slice(upper_dir.as_os_str().as_bytes());
    options.extend_from_slice(b",workdir=");
    options.extend_from_slice(work_dir.as_os_str().as_bytes());
    let source = std::ffi::CString::new("overlay")
        .map_err(|error| PocError::Integrity(format!("invalid overlay source: {error}")))?;
    let filesystem = std::ffi::CString::new("overlay")
        .map_err(|error| PocError::Integrity(format!("invalid overlay filesystem: {error}")))?;
    let target = path_c_string(&scope.workspace_root)?;
    let options = std::ffi::CString::new(options)
        .map_err(|error| PocError::Integrity(format!("invalid overlay mount options: {error}")))?;
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            filesystem.as_ptr(),
            libc::MS_NODEV | libc::MS_NOSUID,
            options.as_ptr().cast(),
        )
    };
    if result != 0 {
        return Err(PocError::io(
            "mount storage-admin OverlayFS workspace",
            &scope.workspace_root,
            std::io::Error::last_os_error(),
        ));
    }
    *mounted_by_this_process = Some(scope.workspace_root.clone());
    Ok(())
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
