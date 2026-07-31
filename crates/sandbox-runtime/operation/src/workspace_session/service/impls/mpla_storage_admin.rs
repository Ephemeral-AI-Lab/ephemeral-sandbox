use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::OnceLock;
use std::time::Instant;

#[cfg(target_os = "linux")]
use sandbox_runtime_mpla_poc::atomic_cgroup_process::{
    spawn_storage_admin_helper_into_cgroup, spawn_storage_admin_publication_helper_into_cgroup,
    AtomicCgroupChild,
};
use sandbox_runtime_mpla_poc::lease::validate_active_storage_admin_lease;
use sandbox_runtime_mpla_poc::storage_admin::{
    verify_process_cgroup_membership, HolderNamespaceSemanticSnapshotInvocation,
    HolderNamespaceSemanticSnapshotReceipt, PlatformPublicationUnmountResult,
    StorageAdminCapabilityProfile, StorageAdminInvocation,
};
use sandbox_runtime_mpla_poc::{
    OperationId, SemanticBuildRequest, StorageAdminAction, StorageAdminAuthorization,
    StorageAdminDurability, StorageAdminReceipt, StorageAdminRequest,
    STORAGE_ADMIN_TRUSTED_EXECUTABLE,
};
use sha2::{Digest, Sha256};

use crate::workspace_crate::WorkspaceSessionId;
use crate::workspace_session::{
    FinalizationState, MplaWorkspaceBinding, WorkspaceSessionError, WorkspaceSessionService,
};

pub(crate) struct MplaPublicationStorageSequenceResult<R> {
    pub(crate) quiesce: StorageAdminReceipt,
    pub(crate) strict_unmount: StorageAdminReceipt,
    pub(crate) checkpoint: R,
    pub(crate) helper_to_unmount_elapsed_ns: u64,
    pub(crate) stable_callback_elapsed_ns: u64,
    pub(crate) helper_cleanup_elapsed_ns: u64,
    pub(crate) helper_input_encode_elapsed_ns: u64,
    pub(crate) helper_launch_elapsed_ns: u64,
    pub(crate) helper_cgroup_placement_elapsed_ns: u64,
    pub(crate) helper_request_write_elapsed_ns: u64,
    pub(crate) helper_response_wait_elapsed_ns: u64,
    pub(crate) helper_unmount_response_decode_elapsed_ns: u64,
    pub(crate) helper_cgroup_release_elapsed_ns: u64,
    pub(crate) helper_input_decode_elapsed_ns: u64,
    pub(crate) helper_validation_elapsed_ns: u64,
    pub(crate) helper_process_preparation_elapsed_ns: u64,
    pub(crate) quiesce_lifecycle_elapsed_ns: u64,
    pub(crate) quiesce_operation_elapsed_ns: u64,
    pub(crate) strict_unmount_lifecycle_elapsed_ns: u64,
    pub(crate) strict_unmount_operation_elapsed_ns: u64,
}

struct FixedPublicationStorageSequenceResult<R> {
    receipts: Vec<StorageAdminReceipt>,
    checkpoint: R,
    helper_to_unmount_elapsed_ns: u64,
    stable_callback_elapsed_ns: u64,
    helper_cleanup_elapsed_ns: u64,
    helper_input_encode_elapsed_ns: u64,
    helper_launch_elapsed_ns: u64,
    helper_cgroup_placement_elapsed_ns: u64,
    helper_request_write_elapsed_ns: u64,
    helper_response_wait_elapsed_ns: u64,
    helper_unmount_response_decode_elapsed_ns: u64,
    helper_cgroup_release_elapsed_ns: u64,
    helper_input_decode_elapsed_ns: u64,
    helper_validation_elapsed_ns: u64,
    helper_process_preparation_elapsed_ns: u64,
    quiesce_lifecycle_elapsed_ns: u64,
    quiesce_operation_elapsed_ns: u64,
    strict_unmount_lifecycle_elapsed_ns: u64,
    strict_unmount_operation_elapsed_ns: u64,
}

struct FixedPublicationHelper {
    pid: u32,
    handle: FixedPublicationHelperHandle,
    stdin: Option<Box<dyn Write + Send>>,
    stdout: Option<Box<dyn Read + Send>>,
    stderr: Option<Box<dyn Read + Send>>,
}

enum FixedPublicationHelperHandle {
    Standard(Child),
    #[cfg(target_os = "linux")]
    AtomicCgroup(AtomicCgroupChild),
}

impl FixedPublicationHelper {
    fn from_standard(mut child: Child) -> Result<Self, String> {
        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "storage-admin publication helper stdin is unavailable".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "storage-admin publication helper stdout is unavailable".to_owned())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "storage-admin publication helper stderr is unavailable".to_owned())?;
        Ok(Self {
            pid,
            handle: FixedPublicationHelperHandle::Standard(child),
            stdin: Some(Box::new(stdin)),
            stdout: Some(Box::new(stdout)),
            stderr: Some(Box::new(stderr)),
        })
    }

    fn id(&self) -> u32 {
        self.pid
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        self.stdin.take()
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stdout.take()
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stderr.take()
    }

    fn kill(&mut self) -> io::Result<()> {
        match &mut self.handle {
            FixedPublicationHelperHandle::Standard(child) => child.kill(),
            #[cfg(target_os = "linux")]
            FixedPublicationHelperHandle::AtomicCgroup(child) => child.kill(),
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        match &mut self.handle {
            FixedPublicationHelperHandle::Standard(child) => child.wait(),
            #[cfg(target_os = "linux")]
            FixedPublicationHelperHandle::AtomicCgroup(child) => child.wait(),
        }
    }

    fn kill_and_wait(&mut self) {
        let _ = self.kill();
        let _ = self.wait();
    }
}

impl Drop for FixedPublicationHelper {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}

struct SpawnedPublicationHelper {
    process: FixedPublicationHelper,
    launch_elapsed_ns: u64,
    placement_elapsed_ns: u64,
}

impl WorkspaceSessionService {
    pub(crate) fn execute_mpla_storage_admin(
        &self,
        routed_operation_id: &OperationId,
        sandbox_id: &str,
        submitted: &StorageAdminRequest,
    ) -> Result<StorageAdminReceipt, WorkspaceSessionError> {
        self.execute_mpla_storage_admin_with_durability(
            routed_operation_id,
            sandbox_id,
            submitted,
            StorageAdminDurability::ExactObjectGraph,
        )
    }

    fn execute_mpla_storage_admin_with_durability(
        &self,
        routed_operation_id: &OperationId,
        sandbox_id: &str,
        submitted: &StorageAdminRequest,
        durability: StorageAdminDurability,
    ) -> Result<StorageAdminReceipt, WorkspaceSessionError> {
        if submitted.operation_id != *routed_operation_id {
            return Err(mpla_error(
                &submitted.scope.workspace_session_id,
                "storage-admin operation_id must equal the routed request_id",
            ));
        }
        if submitted.scope.sandbox_id != sandbox_id {
            return Err(mpla_error(
                &submitted.scope.workspace_session_id,
                "storage-admin sandbox id does not match the routed sandbox",
            ));
        }
        let workspace_session_id = WorkspaceSessionId(submitted.scope.workspace_session_id.clone());
        let selected_profile = self.mpla_storage_admin_profile()?;
        self.with_gated_mpla_storage_action(
            &workspace_session_id,
            submitted.action,
            |handler, binding| {
                let receipt = bind_and_run_storage_admin(
                    routed_operation_id,
                    sandbox_id,
                    submitted,
                    handler,
                    binding,
                    selected_profile,
                    durability,
                )?;
                Ok((receipt.clone(), receipt))
            },
        )
    }

    pub(crate) fn mount_mpla_workspace_session(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        sandbox_id: &str,
        operation_id: &OperationId,
    ) -> Result<StorageAdminReceipt, WorkspaceSessionError> {
        let profile = self.mpla_storage_admin_profile()?;
        let scope = self.mpla_storage_scope(workspace_session_id, sandbox_id)?;
        let mount_operation_id = live_mount_operation_id(
            operation_id,
            workspace_session_id,
            &scope.mount_namespace_id,
        );
        self.execute_mpla_storage_admin_with_durability(
            &mount_operation_id,
            sandbox_id,
            &StorageAdminRequest {
                schema_version: sandbox_runtime_mpla_poc::SCHEMA_VERSION,
                interface_version: sandbox_runtime_mpla_poc::INTERFACE_VERSION.to_owned(),
                profile_id: profile.profile_id().to_owned(),
                operation_id: mount_operation_id.clone(),
                action: StorageAdminAction::Mount,
                scope,
            },
            StorageAdminDurability::SessionLifetime,
        )
    }

    /// Execute the fixed publication storage transaction in one privileged
    /// helper process. Each receipt is then applied through the normal
    /// admission transition path, in order, so batching cannot make lifecycle
    /// state advance without independently validated action evidence.
    pub(crate) fn execute_mpla_publication_storage_sequence_under_gate<R>(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        sandbox_id: &str,
        operation_ids: [OperationId; 3],
        after_unmount: impl FnOnce() -> Result<R, WorkspaceSessionError>,
    ) -> Result<MplaPublicationStorageSequenceResult<R>, WorkspaceSessionError> {
        const ACTIONS: [StorageAdminAction; 3] = [
            StorageAdminAction::Quiesce,
            StorageAdminAction::StrictUnmount,
            StorageAdminAction::Cleanup,
        ];
        let selected_profile = self.mpla_storage_admin_profile()?;
        let (handler, binding) = self.mpla_storage_action_context_under_gate(
            workspace_session_id,
            StorageAdminAction::Quiesce,
            FinalizationState::Finalizing,
        )?;
        let scope = binding.mount_scope.clone().ok_or_else(|| {
            mpla_error(
                &workspace_session_id.0,
                "publication storage sequence requires durable mount authority",
            )
        })?;
        if scope.sandbox_id != sandbox_id || scope.workspace_session_id != workspace_session_id.0 {
            return Err(mpla_error(
                &workspace_session_id.0,
                "stored MPLA mount authority does not match the routed sandbox session",
            ));
        }

        let requests = ACTIONS
            .into_iter()
            .zip(operation_ids)
            .map(|(action, operation_id)| StorageAdminRequest {
                schema_version: sandbox_runtime_mpla_poc::SCHEMA_VERSION,
                interface_version: sandbox_runtime_mpla_poc::INTERFACE_VERSION.to_owned(),
                profile_id: selected_profile.profile_id().to_owned(),
                operation_id,
                action,
                scope: scope.clone(),
            })
            .collect::<Vec<_>>();
        let invocations = requests
            .iter()
            .map(|request| {
                bind_storage_admin_invocation(
                    &request.operation_id,
                    sandbox_id,
                    request,
                    &handler,
                    &binding,
                    selected_profile,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|reason| mpla_error(&workspace_session_id.0, reason))?;
        let FixedPublicationStorageSequenceResult {
            mut receipts,
            checkpoint,
            helper_to_unmount_elapsed_ns,
            stable_callback_elapsed_ns,
            helper_cleanup_elapsed_ns,
            helper_input_encode_elapsed_ns,
            helper_launch_elapsed_ns,
            helper_cgroup_placement_elapsed_ns,
            helper_request_write_elapsed_ns,
            helper_response_wait_elapsed_ns,
            helper_unmount_response_decode_elapsed_ns,
            helper_cgroup_release_elapsed_ns,
            helper_input_decode_elapsed_ns,
            helper_validation_elapsed_ns,
            helper_process_preparation_elapsed_ns,
            quiesce_lifecycle_elapsed_ns,
            quiesce_operation_elapsed_ns,
            strict_unmount_lifecycle_elapsed_ns,
            strict_unmount_operation_elapsed_ns,
        } = run_fixed_storage_admin_publication_sequence(&invocations, |unmount_receipts| {
            if unmount_receipts.len() != 2 {
                return Err(
                    "storage-admin publication sequence stopped before strict unmount".to_owned(),
                );
            }
            for (index, receipt) in unmount_receipts.iter().cloned().enumerate() {
                let request = &requests[index];
                validate_storage_admin_receipt(request, &receipt)?;
                self.with_mpla_storage_action_under_gate(
                    workspace_session_id,
                    request.action,
                    FinalizationState::Finalizing,
                    |_, _| Ok(((), receipt)),
                )
                .map_err(|error| error.to_string())?;
            }
            after_unmount().map_err(|error| error.to_string())
        })
        .map_err(|reason| mpla_error(&workspace_session_id.0, reason))?;
        if receipts.len() != ACTIONS.len() {
            return Err(mpla_error(
                &workspace_session_id.0,
                "storage-admin publication sequence stopped before cleanup",
            ));
        }
        let cleanup = receipts.pop().expect("three receipts were checked");
        validate_storage_admin_receipt(&requests[2], &cleanup)
            .map_err(|reason| mpla_error(&workspace_session_id.0, reason))?;
        self.with_mpla_storage_action_under_gate(
            workspace_session_id,
            StorageAdminAction::Cleanup,
            FinalizationState::Finalizing,
            |_, _| Ok(((), cleanup)),
        )?;
        Ok(MplaPublicationStorageSequenceResult {
            quiesce: receipts[0].clone(),
            strict_unmount: receipts[1].clone(),
            checkpoint,
            helper_to_unmount_elapsed_ns,
            stable_callback_elapsed_ns,
            helper_cleanup_elapsed_ns,
            helper_input_encode_elapsed_ns,
            helper_launch_elapsed_ns,
            helper_cgroup_placement_elapsed_ns,
            helper_request_write_elapsed_ns,
            helper_response_wait_elapsed_ns,
            helper_unmount_response_decode_elapsed_ns,
            helper_cgroup_release_elapsed_ns,
            helper_input_decode_elapsed_ns,
            helper_validation_elapsed_ns,
            helper_process_preparation_elapsed_ns,
            quiesce_lifecycle_elapsed_ns,
            quiesce_operation_elapsed_ns,
            strict_unmount_lifecycle_elapsed_ns,
            strict_unmount_operation_elapsed_ns,
        })
    }
}

fn bind_and_run_storage_admin(
    routed_operation_id: &OperationId,
    sandbox_id: &str,
    submitted: &StorageAdminRequest,
    handler: &super::super::model::WorkspaceSessionHandler,
    binding: &MplaWorkspaceBinding,
    selected_profile: StorageAdminCapabilityProfile,
    durability: StorageAdminDurability,
) -> Result<StorageAdminReceipt, String> {
    let invocation = bind_storage_admin_invocation_with_durability(
        routed_operation_id,
        sandbox_id,
        submitted,
        handler,
        binding,
        selected_profile,
        durability,
    )?;
    let receipt = run_fixed_storage_admin(&invocation)?;
    validate_storage_admin_receipt(submitted, &receipt)?;
    Ok(receipt)
}

pub(super) fn bind_storage_admin_invocation(
    routed_operation_id: &OperationId,
    sandbox_id: &str,
    submitted: &StorageAdminRequest,
    handler: &super::super::model::WorkspaceSessionHandler,
    binding: &MplaWorkspaceBinding,
    selected_profile: StorageAdminCapabilityProfile,
) -> Result<StorageAdminInvocation, String> {
    bind_storage_admin_invocation_with_durability(
        routed_operation_id,
        sandbox_id,
        submitted,
        handler,
        binding,
        selected_profile,
        StorageAdminDurability::ExactObjectGraph,
    )
}

fn bind_storage_admin_invocation_with_durability(
    routed_operation_id: &OperationId,
    sandbox_id: &str,
    submitted: &StorageAdminRequest,
    handler: &super::super::model::WorkspaceSessionHandler,
    binding: &MplaWorkspaceBinding,
    selected_profile: StorageAdminCapabilityProfile,
    durability: StorageAdminDurability,
) -> Result<StorageAdminInvocation, String> {
    if durability == StorageAdminDurability::SessionLifetime
        && submitted.action != StorageAdminAction::Mount
    {
        return Err("session-lifetime storage durability is valid only for Mount".to_owned());
    }
    if submitted.operation_id != *routed_operation_id {
        return Err("storage-admin operation_id must equal the routed request_id".to_owned());
    }
    if submitted.scope.sandbox_id != sandbox_id {
        return Err("storage-admin sandbox id does not match the routed sandbox".to_owned());
    }
    if submitted.scope.workspace_session_id != handler.workspace_session_id.0 {
        return Err("storage-admin workspace session is not the live gated session".to_owned());
    }
    if submitted.scope.workspace_root != handler.handle.workspace_root {
        return Err(
            "storage-admin workspace root is not the live session workspace root".to_owned(),
        );
    }
    if binding.storage_admin_profile != selected_profile {
        return Err(
            "storage-admin session profile does not match the current daemon policy".to_owned(),
        );
    }
    require_daemon_selected_storage_admin_profile(&submitted.profile_id, selected_profile)?;
    let holder_pid = u32::try_from(handler.handle.holder_pid)
        .map_err(|_| "storage-admin live holder pid is invalid".to_owned())?;
    if holder_pid == 0 {
        return Err("storage-admin live holder pid is invalid".to_owned());
    }
    let mount_namespace_id = fs::read_link(format!("/proc/{holder_pid}/ns/mnt"))
        .map_err(|error| format!("read live holder mount namespace: {error}"))?
        .to_string_lossy()
        .into_owned();
    if submitted.scope.mount_namespace_id != mount_namespace_id {
        return Err("storage-admin mount namespace does not match the live holder".to_owned());
    }
    require_scoped_mpla_paths(submitted, handler, binding)?;
    validate_active_storage_admin_lease(
        &submitted.scope.allocation_root,
        &submitted.scope.allocation_id,
        &submitted.scope.session_id,
        &submitted.scope.lease_id,
        submitted.scope.lease_epoch,
    )
    .map_err(|error| format!("validate live MPLA lease binding: {error}"))?;
    let expected_request = StorageAdminRequest {
        schema_version: submitted.schema_version,
        interface_version: submitted.interface_version.clone(),
        profile_id: selected_profile.profile_id().to_owned(),
        operation_id: routed_operation_id.clone(),
        action: submitted.action,
        scope: submitted.scope.clone(),
    };
    let authorization = StorageAdminAuthorization {
        authenticated: true,
        actor_id: "sandbox-runtime-storage-admin".to_owned(),
        operation_id: expected_request.operation_id.clone(),
        run_id: expected_request.scope.run_id.clone(),
        sandbox_id: sandbox_id.to_owned(),
        workspace_session_id: handler.workspace_session_id.0.clone(),
        session_id: expected_request.scope.session_id.clone(),
        allocation_id: expected_request.scope.allocation_id.clone(),
        lease_id: expected_request.scope.lease_id.clone(),
        lease_epoch: expected_request.scope.lease_epoch,
        mount_namespace_id,
    };
    let workload_cgroup_procs = handler
        .cgroup_path
        .as_ref()
        .map(|path| path.join("cgroup.procs"))
        .ok_or_else(|| "storage-admin requires a live workload cgroup".to_owned())?;
    Ok(StorageAdminInvocation {
        expected_request: expected_request.clone(),
        request: expected_request,
        authorization,
        trusted_actor_id: "sandbox-runtime-storage-admin".to_owned(),
        durability,
        trusted_executable_sha256: trusted_storage_admin_executable_sha256()?,
        workload_cgroup_procs,
        mount_namespace_holder_pid: holder_pid,
        mount_receipt_binding: binding.mount_receipt_binding.clone(),
    })
}

fn live_mount_operation_id(
    activation_operation_id: &OperationId,
    workspace_session_id: &WorkspaceSessionId,
    mount_namespace_id: &str,
) -> OperationId {
    let mut hasher = Sha256::new();
    hasher.update(b"mpla-live-mount-operation-v1\0");
    hasher.update(activation_operation_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(workspace_session_id.0.as_bytes());
    hasher.update(b"\0");
    hasher.update(mount_namespace_id.as_bytes());
    OperationId::from_string(format!("mpla-live-mount-{:x}", hasher.finalize()))
}

/// Run the one fixed semantic snapshot helper mode. The request is bound from
/// the live gated session before the helper is started; the helper itself
/// revalidates the complete storage authority after entering the holder mount
/// namespace.
pub(super) fn run_fixed_holder_namespace_semantic_snapshot(
    storage_admin: StorageAdminInvocation,
    semantic: SemanticBuildRequest,
) -> Result<HolderNamespaceSemanticSnapshotReceipt, String> {
    const MAX_INVOCATION_BYTES: usize = 1024 * 1024;
    const MAX_RECEIPT_BYTES: usize = 1024 * 1024;
    let request = HolderNamespaceSemanticSnapshotInvocation {
        format: sandbox_runtime_mpla_poc::storage_admin::HOLDER_NAMESPACE_SEMANTIC_SNAPSHOT_FORMAT
            .to_owned(),
        storage_admin,
        semantic,
    };
    let input = serde_json::to_vec(&request)
        .map_err(|error| format!("encode holder-namespace semantic snapshot: {error}"))?;
    if input.len() > MAX_INVOCATION_BYTES {
        return Err("holder-namespace semantic snapshot exceeds one mebibyte".to_owned());
    }
    let cgroup_procs = &request.storage_admin.workload_cgroup_procs;
    let mut child = Command::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
        .arg("--holder-namespace-semantic-snapshot")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn holder-namespace semantic helper: {error}"))?;
    let helper_pid = child.id();
    if let Err(error) = fs::write(cgroup_procs, helper_pid.to_string()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "place holder-namespace semantic helper in {}: {error}",
            cgroup_procs.display()
        ));
    }
    if let Err(error) = verify_process_cgroup_membership(helper_pid, cgroup_procs) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "verify holder-namespace semantic helper cgroup placement: {error}"
        ));
    }
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "holder-namespace semantic helper stdin is unavailable".to_owned())?;
    stdin
        .write_all(&input)
        .map_err(|error| format!("write holder-namespace semantic snapshot: {error}"))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait for holder-namespace semantic helper: {error}"))?;
    if output.stdout.len() > MAX_RECEIPT_BYTES || output.stderr.len() > MAX_RECEIPT_BYTES {
        return Err(
            "holder-namespace semantic helper exceeded its bounded response budget".to_owned(),
        );
    }
    if !output.status.success() {
        return Err(format!(
            "holder-namespace semantic helper failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("decode holder-namespace semantic receipt: {error}"))
}

fn validate_storage_admin_receipt(
    submitted: &StorageAdminRequest,
    receipt: &StorageAdminReceipt,
) -> Result<(), String> {
    if receipt.scope != submitted.scope
        || receipt.action != submitted.action
        || receipt.operation_id != submitted.operation_id
    {
        return Err("storage-admin receipt does not match the bound request".to_owned());
    }
    Ok(())
}

fn require_daemon_selected_storage_admin_profile(
    submitted_profile_id: &str,
    selected_profile: StorageAdminCapabilityProfile,
) -> Result<(), String> {
    if submitted_profile_id == selected_profile.profile_id() {
        Ok(())
    } else {
        Err(
            "storage-admin profile does not match the daemon-selected capability profile"
                .to_owned(),
        )
    }
}

fn trusted_storage_admin_executable_sha256() -> Result<String, String> {
    static TRUSTED_EXECUTABLE_SHA256: OnceLock<Result<String, String>> = OnceLock::new();
    TRUSTED_EXECUTABLE_SHA256
        .get_or_init(hash_trusted_storage_admin_executable)
        .clone()
}

fn hash_trusted_storage_admin_executable() -> Result<String, String> {
    let mut executable = fs::File::open(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
        .map_err(|error| format!("open fixed storage-admin helper for hashing: {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = executable
            .read(&mut buffer)
            .map_err(|error| format!("read fixed storage-admin helper for hashing: {error}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let mut hash = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        write!(&mut hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(hash)
}

fn require_scoped_mpla_paths(
    submitted: &StorageAdminRequest,
    handler: &super::super::model::WorkspaceSessionHandler,
    binding: &MplaWorkspaceBinding,
) -> Result<(), String> {
    let entry = handler
        .handle
        .entry()
        .map_err(|error| format!("read live workspace launch material: {error}"))?;
    let workspace_root = &handler.handle.workspace_root;
    if entry.workspace_root != *workspace_root
        || submitted.scope.workspace_root != *workspace_root
        || submitted.scope.workspace_root != binding.prepared.workspace_root()
    {
        return Err("storage-admin workspace target is not server-owned".to_owned());
    }
    let allocation_root = &binding.allocation.allocation_root;
    if entry.upperdir != binding.allocation.upper_dir
        || entry.workdir != binding.allocation.work_dir
    {
        return Err("live workspace allocation layout is not MPLA-owned".to_owned());
    }
    if allocation_root.file_name().and_then(|part| part.to_str())
        != Some(submitted.scope.allocation_id.as_str())
        || submitted.scope.allocation_root != *allocation_root
        || submitted.scope.allocation_id != binding.allocation.descriptor.allocation_id
    {
        return Err("storage-admin allocation root is not the live MPLA allocation".to_owned());
    }
    let allocations_root = allocation_root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "live MPLA allocation has no payload allocations root".to_owned())?;
    if allocations_root.file_name().and_then(|part| part.to_str()) != Some("allocations") {
        return Err("live MPLA allocation is outside the payload allocations root".to_owned());
    }
    let payload_root = allocations_root
        .parent()
        .ok_or_else(|| "live MPLA allocations root has no payload root".to_owned())?;
    if submitted.scope.payload_root != payload_root
        || submitted.scope.payload_root != binding.payload_root
    {
        return Err(
            "storage-admin payload root is not derived from the live allocation".to_owned(),
        );
    }
    let session_dir = workspace_root
        .parent()
        .ok_or_else(|| "live workspace target has no MPLA session directory".to_owned())?;
    if workspace_root.file_name().and_then(|part| part.to_str()) != Some("mount")
        || session_dir.file_name().and_then(|part| part.to_str())
            != Some(submitted.scope.session_id.as_str())
    {
        return Err("storage-admin workspace target is not the live MPLA session mount".to_owned());
    }
    let sessions_root = session_dir
        .parent()
        .ok_or_else(|| "live MPLA session has no sessions root".to_owned())?;
    if sessions_root.file_name().and_then(|part| part.to_str()) != Some("sessions") {
        return Err("live MPLA session is outside the control sessions root".to_owned());
    }
    let control_root = sessions_root
        .parent()
        .ok_or_else(|| "live MPLA sessions root has no control root".to_owned())?;
    if submitted.scope.control_root != control_root
        || submitted.scope.control_root != binding.control_root
    {
        return Err("storage-admin control root is not derived from the live session".to_owned());
    }
    if submitted.scope.lower_dirs_newest_first != binding.lower_dirs_newest_first
        || entry.layer_paths != binding.lower_dirs_newest_first
    {
        return Err(
            "storage-admin lower directories do not match the selected MPLA projection".to_owned(),
        );
    }
    if submitted.scope.run_id != binding.run_id
        || submitted.scope.session_id != binding.lease.session_id
        || submitted.scope.lease_id != binding.lease_operation_id.as_str()
        || submitted.scope.lease_epoch != binding.lease.lease_epoch
    {
        return Err(
            "storage-admin request is not bound to the server-owned MPLA run and lease".to_owned(),
        );
    }
    Ok(())
}

fn run_fixed_storage_admin(
    invocation: &StorageAdminInvocation,
) -> Result<StorageAdminReceipt, String> {
    const MAX_INVOCATION_BYTES: usize = 1024 * 1024;
    const MAX_RECEIPT_BYTES: usize = 1024 * 1024;
    let input = serde_json::to_vec(invocation)
        .map_err(|error| format!("encode bound storage-admin invocation: {error}"))?;
    if input.len() > MAX_INVOCATION_BYTES {
        return Err("bound storage-admin invocation exceeds one mebibyte".to_owned());
    }
    let cgroup_procs = &invocation.workload_cgroup_procs;
    let output = run_fixed_storage_admin_helper(&input, cgroup_procs)?;
    if output.stdout.len() > MAX_RECEIPT_BYTES || output.stderr.len() > MAX_RECEIPT_BYTES {
        return Err("storage-admin helper exceeded its bounded response budget".to_owned());
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "fixed storage-admin helper failed: {}",
            stderr.trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("decode fixed storage-admin receipt: {error}"))
}

fn run_fixed_storage_admin_helper(input: &[u8], cgroup_procs: &Path) -> Result<Output, String> {
    #[cfg(target_os = "linux")]
    {
        match spawn_storage_admin_helper_into_cgroup(cgroup_procs) {
            Ok(mut child) => {
                let helper_pid = child.id();
                if let Err(error) = verify_process_cgroup_membership(helper_pid, cgroup_procs) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "verify atomically placed storage-admin helper: {error}"
                    ));
                }
                let mut stdin = child
                    .take_stdin()
                    .ok_or_else(|| "storage-admin helper stdin is unavailable".to_owned())?;
                stdin
                    .write_all(input)
                    .map_err(|error| format!("write bound storage-admin invocation: {error}"))?;
                drop(stdin);
                return child
                    .wait_with_output()
                    .map_err(|error| format!("wait for storage-admin helper: {error}"));
            }
            Err(error) => {
                static FALLBACK_REPORTED: OnceLock<()> = OnceLock::new();
                if FALLBACK_REPORTED.set(()).is_ok() {
                    eprintln!(
                        "MPLA storage-admin helper clone3 cgroup placement unavailable; \
                         using verified cgroup.procs fallback: {error}"
                    );
                }
            }
        }
    }

    let mut child = Command::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
        .args(std::iter::empty::<&str>())
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn fixed storage-admin helper: {error}"))?;
    let helper_pid = child.id();
    if let Err(error) = fs::write(cgroup_procs, helper_pid.to_string()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "place storage-admin helper in {}: {error}",
            cgroup_procs.display()
        ));
    }
    if let Err(error) = verify_process_cgroup_membership(helper_pid, cgroup_procs) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("verify storage-admin cgroup placement: {error}"));
    }
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "storage-admin helper stdin is unavailable".to_owned())?;
    stdin
        .write_all(input)
        .map_err(|error| format!("write bound storage-admin invocation: {error}"))?;
    drop(stdin);
    child
        .wait_with_output()
        .map_err(|error| format!("wait for storage-admin helper: {error}"))
}

fn spawn_publication_helper(
    workload_cgroup_procs: &Path,
) -> Result<SpawnedPublicationHelper, String> {
    #[cfg(target_os = "linux")]
    {
        let launch_started = Instant::now();
        match spawn_storage_admin_publication_helper_into_cgroup(workload_cgroup_procs) {
            Ok(mut atomic_child) => {
                let pid = atomic_child.id();
                let stdin = atomic_child.take_stdin().ok_or_else(|| {
                    "atomic storage-admin publication helper stdin is unavailable".to_owned()
                })?;
                let stdout = atomic_child.take_stdout().ok_or_else(|| {
                    "atomic storage-admin publication helper stdout is unavailable".to_owned()
                })?;
                let stderr = atomic_child.take_stderr().ok_or_else(|| {
                    "atomic storage-admin publication helper stderr is unavailable".to_owned()
                })?;
                let mut process = FixedPublicationHelper {
                    pid,
                    handle: FixedPublicationHelperHandle::AtomicCgroup(atomic_child),
                    stdin: Some(Box::new(stdin)),
                    stdout: Some(Box::new(stdout)),
                    stderr: Some(Box::new(stderr)),
                };
                let launch_elapsed_ns = elapsed_ns(launch_started);
                let placement_started = Instant::now();
                if let Err(error) =
                    verify_process_cgroup_membership(process.id(), workload_cgroup_procs)
                {
                    process.kill_and_wait();
                    return Err(format!(
                        "verify atomically placed storage-admin publication helper: {error}"
                    ));
                }
                return Ok(SpawnedPublicationHelper {
                    process,
                    launch_elapsed_ns,
                    placement_elapsed_ns: elapsed_ns(placement_started),
                });
            }
            Err(error) => {
                static FALLBACK_REPORTED: OnceLock<()> = OnceLock::new();
                if FALLBACK_REPORTED.set(()).is_ok() {
                    eprintln!(
                        "MPLA publication helper clone3 cgroup placement unavailable; \
                         using verified cgroup.procs fallback: {error}"
                    );
                }
            }
        }
    }

    let launch_started = Instant::now();
    let child = Command::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
        .arg("--publication-sequence")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn fixed storage-admin publication helper: {error}"))?;
    let mut process = FixedPublicationHelper::from_standard(child)?;
    let launch_elapsed_ns = elapsed_ns(launch_started);
    let placement_started = Instant::now();
    if let Err(error) = fs::write(workload_cgroup_procs, process.id().to_string()) {
        process.kill_and_wait();
        return Err(format!(
            "place storage-admin publication helper in {}: {error}",
            workload_cgroup_procs.display()
        ));
    }
    if let Err(error) = verify_process_cgroup_membership(process.id(), workload_cgroup_procs) {
        process.kill_and_wait();
        return Err(format!(
            "verify storage-admin publication cgroup placement: {error}"
        ));
    }
    Ok(SpawnedPublicationHelper {
        process,
        launch_elapsed_ns,
        placement_elapsed_ns: elapsed_ns(placement_started),
    })
}

fn run_fixed_storage_admin_publication_sequence<R>(
    invocations: &[StorageAdminInvocation],
    before_cleanup: impl FnOnce(&[StorageAdminReceipt]) -> Result<R, String>,
) -> Result<FixedPublicationStorageSequenceResult<R>, String> {
    const MAX_INVOCATION_BYTES: usize = 3 * 1024 * 1024;
    const MAX_RECEIPT_BYTES: usize = 3 * 1024 * 1024;
    let sequence_started = Instant::now();
    let input_encode_started = Instant::now();
    let mut input = serde_json::to_vec(invocations)
        .map_err(|error| format!("encode bound storage-admin publication sequence: {error}"))?;
    input.push(b'\n');
    if input.len() > MAX_INVOCATION_BYTES {
        return Err("bound storage-admin publication sequence exceeds three mebibytes".to_owned());
    }
    let helper_input_encode_elapsed_ns = elapsed_ns(input_encode_started);
    let first = invocations
        .first()
        .ok_or_else(|| "storage-admin publication sequence is empty".to_owned())?;
    let cgroup_procs = &first.workload_cgroup_procs;
    let spawned = spawn_publication_helper(cgroup_procs)?;
    let helper_launch_elapsed_ns = spawned.launch_elapsed_ns;
    let helper_cgroup_placement_elapsed_ns = spawned.placement_elapsed_ns;
    let mut child = spawned.process;
    let mut stdin = child
        .take_stdin()
        .ok_or_else(|| "storage-admin publication helper stdin is unavailable".to_owned())?;
    let helper_request_write_started = Instant::now();
    stdin
        .write_all(&input)
        .map_err(|error| format!("write bound storage-admin publication sequence: {error}"))?;
    stdin
        .flush()
        .map_err(|error| format!("flush bound storage-admin publication sequence: {error}"))?;
    let helper_request_write_elapsed_ns = elapsed_ns(helper_request_write_started);
    let stdout = child
        .take_stdout()
        .ok_or_else(|| "storage-admin publication helper stdout is unavailable".to_owned())?;
    let mut stdout = BufReader::new(stdout);
    let mut unmount_line = Vec::new();
    let helper_response_wait_started = Instant::now();
    stdout
        .by_ref()
        .take((MAX_RECEIPT_BYTES + 1) as u64)
        .read_until(b'\n', &mut unmount_line)
        .map_err(|error| format!("read storage-admin unmount receipts: {error}"))?;
    let helper_response_wait_elapsed_ns = elapsed_ns(helper_response_wait_started);
    if unmount_line.len() > MAX_RECEIPT_BYTES {
        child.kill_and_wait();
        return Err("storage-admin unmount receipts exceeded the response budget".to_owned());
    }
    let helper_unmount_response_decode_started = Instant::now();
    let unmount_result: PlatformPublicationUnmountResult = serde_json::from_slice(&unmount_line)
        .map_err(|error| format!("decode fixed storage-admin unmount receipts: {error}"))?;
    let helper_unmount_response_decode_elapsed_ns =
        elapsed_ns(helper_unmount_response_decode_started);
    let helper_input_decode_elapsed_ns = unmount_result.input_decode_elapsed_ns;
    let helper_validation_elapsed_ns = unmount_result.validation_elapsed_ns;
    let helper_process_preparation_elapsed_ns = unmount_result.process_preparation_elapsed_ns;
    let quiesce_lifecycle_elapsed_ns = unmount_result.quiesce_lifecycle_elapsed_ns;
    let quiesce_operation_elapsed_ns = unmount_result.quiesce_operation_elapsed_ns;
    let strict_unmount_lifecycle_elapsed_ns = unmount_result.strict_unmount_lifecycle_elapsed_ns;
    let strict_unmount_operation_elapsed_ns = unmount_result.strict_unmount_operation_elapsed_ns;
    let mut receipts = unmount_result.receipts;
    let helper_to_unmount_elapsed_ns = elapsed_ns(sequence_started);
    // Strict unmount has completed.  Before any stable allocation observation,
    // prove the workload leaf contains exactly the helper that was atomically
    // placed there.  This excludes an untrusted writer during the overlap
    // below; the second, empty-cgroup proof is retained after helper exit.
    require_only_cgroup_member(cgroup_procs, child.id())?;

    // Cleanup only removes the already-unmounted workspace target; it never
    // touches the allocation upper tree being inventoried.  Let it run in the
    // already-attested helper while the host performs the independent stable
    // inventory.  Both results, the helper's successful exit, and the final
    // empty-cgroup proof are required before this function returns a
    // checkpoint that can be published.
    let helper_cleanup_started = Instant::now();
    stdin
        .write_all(b"continue-cleanup\n")
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("continue storage-admin publication cleanup: {error}"))?;
    drop(stdin);
    let stable_callback_started = Instant::now();
    let (checkpoint_result, cleanup_result) = std::thread::scope(|scope| {
        let cleanup_task = scope.spawn(move || -> Result<(StorageAdminReceipt, u64), String> {
            let mut cleanup_line = Vec::new();
            stdout
                .by_ref()
                .take((MAX_RECEIPT_BYTES + 1) as u64)
                .read_until(b'\n', &mut cleanup_line)
                .map_err(|error| format!("read storage-admin cleanup receipt: {error}"))?;
            if cleanup_line.len() > MAX_RECEIPT_BYTES {
                child.kill_and_wait();
                return Err("storage-admin cleanup receipt exceeded the response budget".to_owned());
            }
            let cleanup: StorageAdminReceipt = serde_json::from_slice(&cleanup_line)
                .map_err(|error| format!("decode fixed storage-admin cleanup receipt: {error}"))?;
            let status = child
                .wait()
                .map_err(|error| format!("wait for storage-admin publication helper: {error}"))?;
            let mut stderr = Vec::new();
            if let Some(mut child_stderr) = child.take_stderr() {
                child_stderr
                    .by_ref()
                    .take((MAX_RECEIPT_BYTES + 1) as u64)
                    .read_to_end(&mut stderr)
                    .map_err(|error| {
                        format!("read storage-admin publication helper stderr: {error}")
                    })?;
            }
            if stderr.len() > MAX_RECEIPT_BYTES {
                return Err(
                    "storage-admin publication helper exceeded its stderr budget".to_owned(),
                );
            }
            if !status.success() {
                return Err(format!(
                    "fixed storage-admin publication helper failed: {}",
                    String::from_utf8_lossy(&stderr).trim()
                ));
            }
            Ok((cleanup, elapsed_ns(helper_cleanup_started)))
        });
        let checkpoint_result = before_cleanup(&receipts);
        let cleanup_result = cleanup_task
            .join()
            .map_err(|_| "storage-admin publication cleanup monitor panicked".to_owned())
            .and_then(|result| result);
        (checkpoint_result, cleanup_result)
    });
    let (cleanup, helper_cleanup_elapsed_ns) = cleanup_result?;
    require_empty_cgroup(cgroup_procs)?;
    let helper_cgroup_release_elapsed_ns = helper_cleanup_elapsed_ns;
    let checkpoint = checkpoint_result?;
    let stable_callback_elapsed_ns = elapsed_ns(stable_callback_started);
    receipts.push(cleanup);
    Ok(FixedPublicationStorageSequenceResult {
        receipts,
        checkpoint,
        helper_to_unmount_elapsed_ns,
        stable_callback_elapsed_ns,
        helper_cleanup_elapsed_ns,
        helper_input_encode_elapsed_ns,
        helper_launch_elapsed_ns,
        helper_cgroup_placement_elapsed_ns,
        helper_request_write_elapsed_ns,
        helper_response_wait_elapsed_ns,
        helper_unmount_response_decode_elapsed_ns,
        helper_cgroup_release_elapsed_ns,
        helper_input_decode_elapsed_ns,
        helper_validation_elapsed_ns,
        helper_process_preparation_elapsed_ns,
        quiesce_lifecycle_elapsed_ns,
        quiesce_operation_elapsed_ns,
        strict_unmount_lifecycle_elapsed_ns,
        strict_unmount_operation_elapsed_ns,
    })
}

fn mpla_error(workspace_session_id: &str, reason: impl Into<String>) -> WorkspaceSessionError {
    WorkspaceSessionError::MplaLifecycle {
        workspace_session_id: WorkspaceSessionId(workspace_session_id.to_owned()),
        reason: reason.into(),
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn require_only_cgroup_member(cgroup_procs: &Path, expected_pid: u32) -> Result<(), String> {
    let members = fs::read_to_string(cgroup_procs)
        .map_err(|error| format!("read {}: {error}", cgroup_procs.display()))?;
    require_only_cgroup_member_contents(&members, expected_pid).map_err(|reason| {
        format!(
            "workload cgroup {} is not exclusively owned by trusted helper {expected_pid}: {reason}",
            cgroup_procs.display()
        )
    })
}

fn require_only_cgroup_member_contents(members: &str, expected_pid: u32) -> Result<(), String> {
    let pids = parse_cgroup_member_pids(members)?;
    match pids.as_slice() {
        [actual_pid] if *actual_pid == expected_pid => Ok(()),
        [] => Err("cgroup is empty".to_owned()),
        [actual_pid] => Err(format!("cgroup contains pid {actual_pid}")),
        _ => Err(format!("cgroup contains pids {pids:?}")),
    }
}

fn require_empty_cgroup(cgroup_procs: &Path) -> Result<(), String> {
    let members = fs::read_to_string(cgroup_procs)
        .map_err(|error| format!("read {}: {error}", cgroup_procs.display()))?;
    let pids = parse_cgroup_member_pids(&members)?;
    if pids.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "workload cgroup {} is populated by {pids:?}",
            cgroup_procs.display()
        ))
    }
}

fn parse_cgroup_member_pids(members: &str) -> Result<Vec<u32>, String> {
    let mut pids = Vec::new();
    for member in members
        .lines()
        .map(str::trim)
        .filter(|member| !member.is_empty())
    {
        let pid = member
            .parse::<u32>()
            .map_err(|error| format!("invalid cgroup member {member:?}: {error}"))?;
        if pid == 0 {
            return Err("cgroup contains invalid pid 0".to_owned());
        }
        pids.push(pid);
    }
    Ok(pids)
}

#[cfg(test)]
mod tests {
    use super::{live_mount_operation_id, require_only_cgroup_member_contents};
    use crate::workspace_crate::WorkspaceSessionId;
    use sandbox_runtime_mpla_poc::OperationId;

    #[test]
    fn stable_inventory_overlap_requires_the_trusted_helper_to_be_the_only_member() {
        assert!(require_only_cgroup_member_contents("4242\n", 4242).is_ok());
        assert!(require_only_cgroup_member_contents("4242\n9999\n", 4242).is_err());
        assert!(require_only_cgroup_member_contents("9999\n", 4242).is_err());
        assert!(require_only_cgroup_member_contents("not-a-pid\n", 4242).is_err());
    }

    #[test]
    fn live_mount_operation_identity_is_stable_only_for_one_holder_namespace() {
        let activation = OperationId::from_string("activation-operation");
        let workspace = WorkspaceSessionId("workspace-session".to_owned());
        let first = live_mount_operation_id(&activation, &workspace, "mnt:[100]");
        assert_eq!(
            first,
            live_mount_operation_id(&activation, &workspace, "mnt:[100]")
        );
        assert_ne!(
            first,
            live_mount_operation_id(&activation, &workspace, "mnt:[101]")
        );
        assert_ne!(
            first,
            live_mount_operation_id(
                &activation,
                &WorkspaceSessionId("other-workspace".to_owned()),
                "mnt:[100]",
            )
        );
    }
}
