use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fault::{FaultInjector, FaultPoint};
#[cfg(target_os = "linux")]
use crate::overlay_adapter::mount_permanent_overlay_anchored;
use crate::overlay_adapter::{
    freeze_attested_mount_read_only_anchored, require_attested_mount_absent_anchored,
    strict_unmount_attested_frozen_anchored, validate_attested_mount_for_cleanup_anchored,
    validated_attested_cgroup_path, AttestedMountCleanupState, OverlayMountAttestation,
    PermanentOverlayMount, UnmountedOverlay,
};
use crate::process_tree::{
    anchored_workspace_audit_identity, audit_terminal_workspace_anchored,
    terminate_terminal_workspace_references_anchored, AnchoredWorkspaceAuditIdentity,
    AttestedCgroupMembership, CommandReceipt, ManagedProcessTree, ProcessAudit,
};
use crate::quiesce::{
    self, ReceiptHitSealInput, ReceiptSealedAllocation, SealedAllocation, SealingRecord,
};
use crate::recovery::reach_real_operation;
use crate::{
    durable, lease, unix_time_ms, AdoptionReceipt, AllocationHandle, MutableLease,
    NamedFaultInjector, NamedFaultPoint, OperationId, OwnerTransitionRequest, PocError, PocResult,
    SessionId, SessionPhase, StableAllocationReceipt, TerminalLeaseFenceWitness, WriterCapability,
    SCHEMA_VERSION,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionRecord {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub allocation_id: crate::AllocationId,
    pub lease_epoch: u64,
    pub owner_epoch: u64,
    pub phase: SessionPhase,
    pub workspace_root: PathBuf,
    pub updated_unix_ms: u64,
}

/// Immutable authority produced by restart-only terminal seal recovery.  The
/// embedded pair is canonical; `STABLE.json` and `QUIESCENCE.json` are exact
/// projections which can be repaired after a crash without changing this
/// receipt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSealRecoveryDisposition {
    /// The process died before the final durable Sealing record existed.  The
    /// workspace is terminally dismantled, but no new sealed payload is
    /// ratified.
    Old,
    /// The final durable Sealing record existed, so recovery completed the
    /// stable/quiescent sealed-payload boundary.
    CompleteNew,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSealCleanupWitness {
    pub killed_or_signaled_pids: Vec<i32>,
    pub pre_unmount_audit: ProcessAudit,
    pub post_unmount_audit: ProcessAudit,
    pub unmounted: UnmountedOverlay,
}

/// Secret-free durable input for restart-only seal recovery.  This tuple is
/// sufficient to authenticate and fence the dead session, but cannot recreate
/// either of its writer/deleter capabilities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSealRecoveryRequest {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub prior_operation_id: OperationId,
    pub session_id: SessionId,
    pub allocation_id: crate::AllocationId,
    pub lease_epoch: u64,
    pub owner_epoch: u64,
}

impl SessionSealRecoveryRequest {
    #[must_use]
    pub fn from_lease(
        lease: &MutableLease,
        prior_operation_id: OperationId,
        operation_id: OperationId,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            operation_id,
            prior_operation_id,
            session_id: lease.session_id.clone(),
            allocation_id: lease.allocation_id.clone(),
            lease_epoch: lease.lease_epoch,
            owner_epoch: lease.owner_epoch,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSealRecoveryReceipt {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub prior_operation_id: OperationId,
    pub session_id: SessionId,
    pub allocation_id: crate::AllocationId,
    pub lease_epoch: u64,
    pub owner_epoch: u64,
    pub lease_fence: TerminalLeaseFenceWitness,
    pub disposition: SessionSealRecoveryDisposition,
    pub terminal_phase: SessionPhase,
    pub sealing: Option<SealingRecord>,
    pub cleanup: SessionSealCleanupWitness,
    pub stable: Option<crate::StableAllocationReceipt>,
    pub quiescence: Option<crate::QuiescenceReceipt>,
    pub completed_unix_ms: u64,
}

const MOUNT_ATTESTATION_FILE: &str = "MOUNT.json";
const RECOVERY_RECEIPT_FILE: &str = "SEAL-RECOVERY.json";
const TERMINAL_AUDIT_BUDGET: Duration = Duration::from_secs(1);

/// Durable MPLA session control state prepared by the public runtime before
/// the storage-admin helper mounts the allocation into a holder namespace.
///
/// This deliberately contains no mount or process-tree authority.  The
/// caller may pass its exact `workspace_root` to the typed storage-admin
/// request, but only the helper is allowed to make it a mountpoint.
#[derive(Clone, Debug)]
pub struct PreparedExternalSession {
    session_dir: PathBuf,
    workspace_root: PathBuf,
    #[cfg(target_os = "linux")]
    allocation_authority: Arc<AnchoredAllocationAuthority>,
}

impl PartialEq for PreparedExternalSession {
    fn eq(&self, other: &Self) -> bool {
        self.session_dir == other.session_dir && self.workspace_root == other.workspace_root
    }
}

impl Eq for PreparedExternalSession {}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct AnchoredPreparedSession {
    session_dir: PathBuf,
    workspace_root: PathBuf,
    session: OwnedFd,
    workspace: OwnedFd,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AllocationDirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct AnchoredAllocationAuthority {
    root: OwnedFd,
    upper: OwnedFd,
    work: OwnedFd,
    owner: OwnedFd,
    root_identity: AllocationDirectoryIdentity,
    upper_identity: AllocationDirectoryIdentity,
    work_identity: AllocationDirectoryIdentity,
    owner_identity: AllocationDirectoryIdentity,
}

#[cfg(target_os = "linux")]
impl AnchoredAllocationAuthority {
    fn pin_for_session(
        allocation: &AllocationHandle,
        expected_upper: &OwnedFd,
        expected_work: &OwnedFd,
    ) -> PocResult<Self> {
        let authority = Self::pin(allocation)?;
        if allocation_directory_identity(
            expected_upper,
            "stat supplied allocation upper",
            &allocation.upper_dir,
        )? != authority.upper_identity
            || allocation_directory_identity(
                expected_work,
                "stat supplied allocation work",
                &allocation.work_dir,
            )? != authority.work_identity
        {
            return Err(PocError::RecoveryRequired(
                "anchored session allocation differs from its supplied upper/work authority"
                    .to_owned(),
            ));
        }
        authority.revalidate(allocation)?;
        Ok(authority)
    }

    fn pin_for_recovery(
        allocation: &AllocationHandle,
        attestation: &OverlayMountAttestation,
    ) -> PocResult<Self> {
        let authority = Self::pin(allocation)?;
        if authority.root_identity.device != attestation.allocation_root_device
            || authority.root_identity.inode != attestation.allocation_root_inode
            || authority.upper_identity.device != attestation.allocation_upper_device
            || authority.upper_identity.inode != attestation.allocation_upper_inode
            || authority.work_identity.device != attestation.allocation_work_device
            || authority.work_identity.inode != attestation.allocation_work_inode
            || authority.owner_identity.device != attestation.allocation_owner_device
            || authority.owner_identity.inode != attestation.allocation_owner_inode
        {
            return Err(PocError::RecoveryRequired(
                "recovery allocation root/upper/work/owner differ from their durable mount identity"
                    .to_owned(),
            ));
        }
        authority.revalidate(allocation)?;
        Ok(authority)
    }

    fn pin(allocation: &AllocationHandle) -> PocResult<Self> {
        require_canonical_allocation_paths(allocation)?;
        let root = open_directory_no_symlink("allocation root", &allocation.allocation_root)?;
        let upper = open_child_directory_no_symlink(
            "allocation upper",
            &root,
            std::ffi::OsStr::new("upper"),
        )?;
        let work = open_child_directory_no_symlink(
            "allocation work",
            &root,
            std::ffi::OsStr::new("work"),
        )?;
        let owner = open_child_directory_no_symlink(
            "allocation owner",
            &root,
            std::ffi::OsStr::new("owner"),
        )?;
        let authority = Self {
            root_identity: allocation_directory_identity(
                &root,
                "stat pinned allocation root",
                &allocation.allocation_root,
            )?,
            upper_identity: allocation_directory_identity(
                &upper,
                "stat pinned allocation upper",
                &allocation.upper_dir,
            )?,
            work_identity: allocation_directory_identity(
                &work,
                "stat pinned allocation work",
                &allocation.work_dir,
            )?,
            owner_identity: allocation_directory_identity(
                &owner,
                "stat pinned allocation owner",
                &allocation.owner_dir,
            )?,
            root,
            upper,
            work,
            owner,
        };
        authority.revalidate(allocation)?;
        Ok(authority)
    }

    pub(crate) fn revalidate(&self, allocation: &AllocationHandle) -> PocResult<()> {
        require_canonical_allocation_paths(allocation)?;
        let named_root = open_directory_no_symlink("allocation root", &allocation.allocation_root)?;
        let named_upper = open_child_directory_no_symlink(
            "allocation upper",
            &self.root,
            std::ffi::OsStr::new("upper"),
        )?;
        let named_work = open_child_directory_no_symlink(
            "allocation work",
            &self.root,
            std::ffi::OsStr::new("work"),
        )?;
        let named_owner = open_child_directory_no_symlink(
            "allocation owner",
            &self.root,
            std::ffi::OsStr::new("owner"),
        )?;
        let exact = allocation_directory_identity(
            &self.root,
            "restat pinned allocation root",
            &allocation.allocation_root,
        )? == self.root_identity
            && allocation_directory_identity(
                &self.upper,
                "restat pinned allocation upper",
                &allocation.upper_dir,
            )? == self.upper_identity
            && allocation_directory_identity(
                &self.work,
                "restat pinned allocation work",
                &allocation.work_dir,
            )? == self.work_identity
            && allocation_directory_identity(
                &self.owner,
                "restat pinned allocation owner",
                &allocation.owner_dir,
            )? == self.owner_identity
            && allocation_directory_identity(
                &named_root,
                "stat named allocation root",
                &allocation.allocation_root,
            )? == self.root_identity
            && allocation_directory_identity(
                &named_upper,
                "stat named allocation upper",
                &allocation.upper_dir,
            )? == self.upper_identity
            && allocation_directory_identity(
                &named_work,
                "stat named allocation work",
                &allocation.work_dir,
            )? == self.work_identity
            && allocation_directory_identity(
                &named_owner,
                "stat named allocation owner",
                &allocation.owner_dir,
            )? == self.owner_identity;
        let descriptor: crate::AllocationDescriptor =
            durable::read_json(&self.root_path().join("ALLOCATION.json"))?;
        if !exact || descriptor != allocation.descriptor {
            return Err(PocError::RecoveryRequired(
                "allocation root/upper/work/owner binding changed after it was pinned".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn anchored_handle(&self, allocation: &AllocationHandle) -> AllocationHandle {
        let root = self.root_path();
        AllocationHandle {
            descriptor: allocation.descriptor.clone(),
            upper_dir: root.join("upper"),
            work_dir: root.join("work"),
            owner_dir: root.join("owner"),
            allocation_root: root,
        }
    }

    pub(crate) fn upper(&self) -> &OwnedFd {
        &self.upper
    }

    pub(crate) fn root(&self) -> &OwnedFd {
        &self.root
    }

    pub(crate) fn owner(&self) -> &OwnedFd {
        &self.owner
    }

    pub(crate) fn root_path(&self) -> PathBuf {
        allocation_descriptor_path(&self.root)
    }

    pub(crate) fn upper_path(&self) -> PathBuf {
        allocation_descriptor_path(&self.upper)
    }

    pub(crate) fn owner_path(&self) -> PathBuf {
        allocation_descriptor_path(&self.owner)
    }

    fn compare_and_adopt_after_intent(
        &self,
        allocation: &AllocationHandle,
        stable: &StableAllocationReceipt,
        request: &OwnerTransitionRequest,
        after_durable_intent: impl FnOnce() -> PocResult<()>,
    ) -> PocResult<AdoptionReceipt> {
        crate::owner::compare_and_adopt_anchored_after_intent(
            &self.root,
            &self.owner,
            stable,
            request,
            after_durable_intent,
            || self.revalidate(allocation),
        )
    }

    fn stale_capabilities_rejected(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<(bool, bool)> {
        self.revalidate(allocation)?;
        let root = self.root_path();
        let rejected = crate::owner::with_pinned_owner_directory(&root, &self.owner, || {
            Ok((
                lease::validate_writer(&root, &lease.writer).is_err(),
                lease::validate_deleter(&root, &lease.deleter).is_err(),
            ))
        })?;
        self.revalidate(allocation)?;
        Ok(rejected)
    }
}

impl PreparedExternalSession {
    #[must_use]
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn begin_sealing(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
        operation_id: &OperationId,
        faults: &mut FaultInjector,
    ) -> PocResult<SealingRecord> {
        require_single_component("Sealing operation", operation_id.as_str())?;
        let _seal_recovery_lock =
            lock_original_seal_against_recovery(allocation, &self.session_dir, operation_id)?;
        let record = self.validate_binding(allocation, lease)?;
        faults.hit(FaultPoint::BeforeSealing, false)?;
        let sealing_path = quiesce::sealing_record_path(&self.session_dir);
        if path_entry_exists(&sealing_path)? {
            let sealing: SealingRecord = read_recovery_json(&sealing_path)?;
            validate_external_sealing_record(&sealing, operation_id, lease)?;
            if !matches!(
                record.phase,
                SessionPhase::Open
                    | SessionPhase::Closing
                    | SessionPhase::Sealing
                    | SessionPhase::RecoveryRequired
                    | SessionPhase::PublicationCommitted
            ) {
                return Err(PocError::Integrity(format!(
                    "external session {} cannot resume Sealing from {:?}",
                    lease.session_id, record.phase
                )));
            }
            return Ok(sealing);
        }
        if !matches!(record.phase, SessionPhase::Open | SessionPhase::Closing) {
            return Err(PocError::Integrity(format!(
                "external session {} cannot begin Sealing from {:?}",
                lease.session_id, record.phase
            )));
        }
        // Admission is already closed in memory by the public service. The
        // durable Sealing record below is the ratified terminal boundary, so
        // an intermediate durable Closing rewrite adds no recovery state.
        let mut named_faults = NamedFaultInjector::default().with_physical_context(
            operation_id.as_str(),
            [self.session_dir.join("SESSION.json"), sealing_path.clone()],
        );
        let sealing = match quiesce::persist_sealing(
            &self.session_dir,
            operation_id,
            lease,
            &allocation.upper_dir,
            &mut named_faults,
        ) {
            Ok(sealing) => sealing,
            Err(error) => {
                let temporary_path =
                    pre_ratification_temporary_path(&self.session_dir, operation_id);
                let boundary_entry = path_entry_exists(&sealing_path).and_then(|final_exists| {
                    if final_exists {
                        Ok(true)
                    } else {
                        path_entry_exists(&temporary_path)
                    }
                });
                match boundary_entry {
                    Ok(false) => return Err(error),
                    boundary_state => {
                        return Err(PocError::RecoveryRequired(format!(
                            "Sealing publication left a final/temporary entry or could not be audited: {error}; entry audit: {boundary_state:?}"
                        )));
                    }
                }
            }
        };
        faults.hit(FaultPoint::AfterSealingDurable, true)?;
        Ok(sealing)
    }

    /// Return whether the session has crossed its durable publication boundary.
    ///
    /// `SEALING.json` is the ratified boundary for an external session.  The
    /// original `SESSION.json` phase may therefore remain `Open`; callers that
    /// could otherwise delete an unpublished allocation must consult this
    /// record and fail closed if its scope is malformed.
    pub fn has_ratified_sealing(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<bool> {
        self.validate_binding(allocation, lease)?;
        let sealing_path = quiesce::sealing_record_path(&self.session_dir);
        if !path_entry_exists(&sealing_path)? {
            return Ok(false);
        }
        let sealing: SealingRecord = read_recovery_json(&sealing_path)?;
        validate_external_sealing_scope(&sealing, lease)?;
        Ok(true)
    }

    pub fn mark_publication_committed(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<()> {
        let record = self.validate_binding(allocation, lease)?;
        if record.phase == SessionPhase::PublicationCommitted {
            return Ok(());
        }
        if !matches!(
            record.phase,
            SessionPhase::Open
                | SessionPhase::Closing
                | SessionPhase::Sealing
                | SessionPhase::RecoveryRequired
        ) {
            return Err(PocError::Integrity(format!(
                "external session {} cannot commit publication from {:?}",
                lease.session_id, record.phase
            )));
        }
        if !self.has_ratified_sealing(allocation, lease)? {
            return Err(PocError::Integrity(
                "external session cannot commit publication before durable Sealing".to_owned(),
            ));
        }
        persist_session_record(
            &self.session_dir,
            lease,
            SessionPhase::PublicationCommitted,
            &self.workspace_root,
        )
    }

    pub fn mark_recovery_required(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<()> {
        let record = self.validate_binding(allocation, lease)?;
        if record.phase == SessionPhase::RecoveryRequired {
            return Ok(());
        }
        if !matches!(
            record.phase,
            SessionPhase::Open | SessionPhase::Closing | SessionPhase::Sealing
        ) {
            return Err(PocError::Integrity(
                "pre-Sealing external session cannot be marked terminal recovery".to_owned(),
            ));
        }
        if !self.has_ratified_sealing(allocation, lease)? {
            return Err(PocError::Integrity(
                "pre-Sealing external session cannot be marked terminal recovery".to_owned(),
            ));
        }
        persist_session_record(
            &self.session_dir,
            lease,
            SessionPhase::RecoveryRequired,
            &self.workspace_root,
        )
    }

    pub(crate) fn validate_stationary_binding(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
        operation_id: &OperationId,
    ) -> PocResult<SessionRecord> {
        let record = self.validate_binding(allocation, lease)?;
        if !matches!(
            record.phase,
            SessionPhase::Open
                | SessionPhase::Closing
                | SessionPhase::Sealing
                | SessionPhase::RecoveryRequired
                | SessionPhase::PublicationCommitted
        ) {
            return Err(PocError::Integrity(format!(
                "external session {} is not terminally sealed: {:?}",
                lease.session_id, record.phase
            )));
        }
        let sealing: SealingRecord =
            durable::read_json(&quiesce::sealing_record_path(&self.session_dir))?;
        validate_external_sealing_record(&sealing, operation_id, lease)?;
        Ok(record)
    }

    pub(crate) fn compare_and_adopt_after_intent(
        &self,
        allocation: &AllocationHandle,
        stable: &StableAllocationReceipt,
        request: &OwnerTransitionRequest,
        after_durable_intent: impl FnOnce() -> PocResult<()>,
    ) -> PocResult<AdoptionReceipt> {
        #[cfg(target_os = "linux")]
        return self.allocation_authority.compare_and_adopt_after_intent(
            allocation,
            stable,
            request,
            after_durable_intent,
        );
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (allocation, stable, request, after_durable_intent);
            Err(PocError::Unsupported(
                "descriptor-anchored external adoption requires Linux".to_owned(),
            ))
        }
    }

    pub(crate) fn stale_capabilities_rejected(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<(bool, bool)> {
        #[cfg(target_os = "linux")]
        return self
            .allocation_authority
            .stale_capabilities_rejected(allocation, lease);
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (allocation, lease);
            Err(PocError::Unsupported(
                "descriptor-anchored external capability fencing requires Linux".to_owned(),
            ))
        }
    }

    fn validate_binding(
        &self,
        allocation: &AllocationHandle,
        lease: &MutableLease,
    ) -> PocResult<SessionRecord> {
        #[cfg(target_os = "linux")]
        self.allocation_authority.revalidate(allocation)?;
        if allocation.descriptor.allocation_id != lease.allocation_id {
            return Err(PocError::Integrity(
                "external session lease allocation does not match allocation handle".to_owned(),
            ));
        }
        let expected_session_dir = self
            .session_dir
            .parent()
            .and_then(Path::parent)
            .map(|control_root| {
                control_root
                    .join("sessions")
                    .join(lease.session_id.as_str())
            })
            .ok_or_else(|| {
                PocError::Integrity("external session directory has no control root".to_owned())
            })?;
        if self.session_dir != expected_session_dir
            || self.workspace_root != self.session_dir.join("mount")
        {
            return Err(PocError::Integrity(
                "external session paths do not match the lease identity".to_owned(),
            ));
        }
        let record: SessionRecord = durable::read_json(&self.session_dir.join("SESSION.json"))?;
        if record.schema_version != SCHEMA_VERSION
            || record.session_id != lease.session_id
            || record.allocation_id != lease.allocation_id
            || record.lease_epoch != lease.lease_epoch
            || record.owner_epoch != lease.owner_epoch
            || record.workspace_root != self.workspace_root
        {
            return Err(PocError::Integrity(
                "durable external session record differs from its allocation and lease".to_owned(),
            ));
        }
        #[cfg(target_os = "linux")]
        self.allocation_authority.revalidate(allocation)?;
        Ok(record)
    }
}

fn validate_external_sealing_record(
    sealing: &SealingRecord,
    operation_id: &OperationId,
    lease: &MutableLease,
) -> PocResult<()> {
    validate_external_sealing_scope(sealing, lease)?;
    if sealing.operation_id != *operation_id {
        return Err(PocError::RecoveryRequired(
            "durable external Sealing operation does not match the requested operation".to_owned(),
        ));
    }
    Ok(())
}

fn lock_original_seal_against_recovery(
    allocation: &AllocationHandle,
    session_dir: &Path,
    operation_id: &OperationId,
) -> PocResult<durable::FileLock> {
    let lock =
        durable::FileLock::exclusive(&crate::owner::owner_lock_path(&allocation.allocation_root))?;
    let recovery_path = session_dir.join(RECOVERY_RECEIPT_FILE);
    if path_entry_exists(&recovery_path)? {
        return Err(PocError::RecoveryRequired(format!(
            "operation {} cannot publish Sealing after terminal recovery",
            operation_id.as_str()
        )));
    }
    Ok(lock)
}

#[cfg(target_os = "linux")]
fn lock_original_seal_against_recovery_anchored(
    allocation: &AllocationHandle,
    authority: &AnchoredAllocationAuthority,
    session_dir: &Path,
    operation_id: &OperationId,
) -> PocResult<durable::FileLock> {
    authority.revalidate(allocation)?;
    let lock = durable::FileLock::exclusive(&authority.owner_path().join("LOCK"))?;
    let recovery_path = session_dir.join(RECOVERY_RECEIPT_FILE);
    if path_entry_exists(&recovery_path)? {
        return Err(PocError::RecoveryRequired(format!(
            "operation {} cannot publish Sealing after terminal recovery",
            operation_id.as_str()
        )));
    }
    authority.revalidate(allocation)?;
    Ok(lock)
}

fn validate_external_sealing_scope(sealing: &SealingRecord, lease: &MutableLease) -> PocResult<()> {
    if sealing.schema_version != SCHEMA_VERSION
        || sealing.session_id != lease.session_id
        || sealing.allocation_id != lease.allocation_id
        || sealing.lease_epoch != lease.lease_epoch
        || sealing.owner_epoch != lease.owner_epoch
    {
        return Err(PocError::RecoveryRequired(
            "durable external Sealing scope does not match the requested lease tuple".to_owned(),
        ));
    }
    Ok(())
}

fn validate_recovery_request(
    request: &SessionSealRecoveryRequest,
    allocation: &AllocationHandle,
) -> PocResult<()> {
    if request.schema_version != SCHEMA_VERSION
        || request.allocation_id != allocation.descriptor.allocation_id
    {
        return Err(PocError::RecoveryRequired(
            "seal-recovery request differs from the permanent allocation".to_owned(),
        ));
    }
    require_single_component("session", request.session_id.as_str())?;
    require_single_component("seal-recovery operation", request.operation_id.as_str())?;
    require_single_component("prior lease operation", request.prior_operation_id.as_str())
}

fn validate_recovery_sealing_record(
    sealing: &SealingRecord,
    request: &SessionSealRecoveryRequest,
) -> PocResult<()> {
    if sealing.schema_version == SCHEMA_VERSION
        && sealing.operation_id == request.operation_id
        && sealing.session_id == request.session_id
        && sealing.allocation_id == request.allocation_id
        && sealing.lease_epoch == request.lease_epoch
        && sealing.owner_epoch == request.owner_epoch
    {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "durable Sealing scope does not match the secret-free recovery tuple".to_owned(),
        ))
    }
}

/// Create the durable control-plane state for a lease-backed MPLA session
/// without mounting the allocation or admitting a workload.
pub fn prepare_external_session(
    control_root: &Path,
    allocation: &AllocationHandle,
    lease: &MutableLease,
) -> PocResult<PreparedExternalSession> {
    require_single_component("session", lease.session_id.as_str())?;
    if allocation.descriptor.allocation_id != lease.allocation_id {
        return Err(PocError::Integrity(
            "session lease allocation does not match allocation handle".to_owned(),
        ));
    }
    #[cfg(target_os = "linux")]
    let allocation_authority = Arc::new(AnchoredAllocationAuthority::pin(allocation)?);
    let activation_root = control_root.join("activations");
    std::fs::create_dir_all(control_root)
        .map_err(|error| PocError::io("create session control root", control_root, error))?;
    match std::fs::create_dir(&activation_root) {
        Ok(()) => durable::fsync_dir(control_root)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !activation_root.is_dir() {
                return Err(PocError::Integrity(format!(
                    "activation control root is not a directory: {}",
                    activation_root.display()
                )));
            }
        }
        Err(error) => {
            return Err(PocError::io(
                "create activation control root",
                &activation_root,
                error,
            ));
        }
    }
    let session_dir = control_root
        .join("sessions")
        .join(lease.session_id.as_str());
    let workspace_root = session_dir.join("mount");
    let record_path = session_dir.join("SESSION.json");
    if path_entry_exists(&record_path)? {
        let existing: SessionRecord = read_recovery_json(&record_path)?;
        validate_session_record(&existing, lease, &workspace_root)?;
        return Err(PocError::RecoveryRequired(format!(
            "existing session {:?} cannot be reopened without restart recovery",
            existing.phase
        )));
    } else {
        std::fs::create_dir_all(&workspace_root).map_err(|error| {
            PocError::io("create session mount directory", &workspace_root, error)
        })?;
        #[cfg(unix)]
        std::fs::set_permissions(&session_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| PocError::io("make session directory private", &session_dir, error))?;
        durable::fsync_dir(&session_dir)?;
        durable::fsync_dir(
            session_dir
                .parent()
                .ok_or_else(|| PocError::Integrity("session directory has no parent".to_owned()))?,
        )?;
        durable::fsync_dir(control_root)?;
        persist_session_record(&session_dir, lease, SessionPhase::Open, &workspace_root)?;
    }
    #[cfg(target_os = "linux")]
    allocation_authority.revalidate(allocation)?;
    Ok(PreparedExternalSession {
        session_dir,
        workspace_root,
        #[cfg(target_os = "linux")]
        allocation_authority,
    })
}

#[cfg(target_os = "linux")]
fn prepare_external_session_anchored(
    control_root_label: &Path,
    control_root: &OwnedFd,
    allocation: &AllocationHandle,
    lease: &MutableLease,
) -> PocResult<AnchoredPreparedSession> {
    require_single_component("session", lease.session_id.as_str())?;
    if allocation.descriptor.allocation_id != lease.allocation_id {
        return Err(PocError::Integrity(
            "session lease allocation does not match allocation handle".to_owned(),
        ));
    }
    let _ = ensure_anchored_child_directory(
        control_root,
        std::ffi::OsStr::new("activations"),
        &control_root_label.join("activations"),
    )?;
    let (sessions, _) = ensure_anchored_child_directory(
        control_root,
        std::ffi::OsStr::new("sessions"),
        &control_root_label.join("sessions"),
    )?;
    let session_dir = control_root_label
        .join("sessions")
        .join(lease.session_id.as_str());
    let workspace_root = session_dir.join("mount");
    let session_name = std::ffi::OsStr::new(lease.session_id.as_str());
    let (session, session_created) =
        ensure_anchored_child_directory(&sessions, session_name, &session_dir)?;
    if !session_created {
        let record_path = session_dir.join("SESSION.json");
        if path_entry_exists_at(&session, std::ffi::OsStr::new("SESSION.json"), &record_path)? {
            let existing: SessionRecord = read_recovery_json_at(
                &session,
                std::ffi::OsStr::new("SESSION.json"),
                &record_path,
            )?;
            validate_session_record(&existing, lease, &workspace_root)?;
            return Err(PocError::RecoveryRequired(format!(
                "existing session {:?} cannot be reopened without restart recovery",
                existing.phase
            )));
        }
        return Err(PocError::RecoveryRequired(format!(
            "anchored session directory exists without SESSION.json: {}",
            session_dir.display()
        )));
    }
    let (workspace, workspace_created) =
        ensure_anchored_child_directory(&session, std::ffi::OsStr::new("mount"), &workspace_root)?;
    if !workspace_created {
        return Err(PocError::RecoveryRequired(format!(
            "new anchored session collided with an existing mount directory: {}",
            workspace_root.display()
        )));
    }
    let record = SessionRecord {
        schema_version: SCHEMA_VERSION,
        session_id: lease.session_id.clone(),
        allocation_id: lease.allocation_id.clone(),
        lease_epoch: lease.lease_epoch,
        owner_epoch: lease.owner_epoch,
        phase: SessionPhase::Open,
        workspace_root: workspace_root.clone(),
        updated_unix_ms: unix_time_ms()?,
    };
    write_recovery_immutable_json(&session, &session_dir.join("SESSION.json"), &record)?;
    rustix::fs::fsync(&sessions).map_err(|error| {
        PocError::io(
            "fsync anchored sessions directory",
            control_root_label.join("sessions"),
            std::io::Error::from(error),
        )
    })?;
    rustix::fs::fsync(control_root).map_err(|error| {
        PocError::io(
            "fsync anchored session control root",
            control_root_label,
            std::io::Error::from(error),
        )
    })?;
    Ok(AnchoredPreparedSession {
        session_dir,
        workspace_root,
        session,
        workspace,
    })
}

#[cfg(target_os = "linux")]
fn ensure_anchored_child_directory(
    parent: &OwnedFd,
    child: &std::ffi::OsStr,
    display_path: &Path,
) -> PocResult<(OwnedFd, bool)> {
    require_single_component_os("anchored session directory name", child)?;
    let created = match rustix::fs::mkdirat(
        parent,
        child,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
    ) {
        Ok(()) => {
            rustix::fs::fsync(parent).map_err(|error| {
                PocError::io(
                    "fsync anchored session parent",
                    display_path.parent().unwrap_or(display_path),
                    std::io::Error::from(error),
                )
            })?;
            true
        }
        Err(rustix::io::Errno::EXIST) => false,
        Err(error) => {
            return Err(PocError::io(
                "create anchored session directory",
                display_path,
                std::io::Error::from(error),
            ));
        }
    };
    let directory = open_child_directory_no_symlink("anchored session directory", parent, child)?;
    Ok((directory, created))
}

/// Resolve a dead session without recreating execution authority.  Absence of
/// the final durable Sealing record resolves to `Old`; its presence rolls the
/// same operation forward to `CompleteNew`.  This API never mounts, constructs
/// `MplaSession`, creates a process runner, or returns a writer/deleter
/// capability.
#[cfg(not(target_os = "linux"))]
pub fn recover_session_seal(
    _control_root: &Path,
    _allocation: &AllocationHandle,
    _request: &SessionSealRecoveryRequest,
) -> PocResult<SessionSealRecoveryReceipt> {
    Err(PocError::Unsupported(
        "terminal seal recovery requires Linux allocation descriptors".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
pub fn recover_session_seal(
    control_root: &Path,
    allocation: &AllocationHandle,
    request: &SessionSealRecoveryRequest,
) -> PocResult<SessionSealRecoveryReceipt> {
    validate_recovery_request(request, allocation)?;
    let operation_id = &request.operation_id;
    let session_dir = control_root
        .join("sessions")
        .join(request.session_id.as_str());
    let workspace_root = session_dir.join("mount");
    let session_anchor =
        open_directory_no_symlink("seal-recovery session directory", &session_dir)?;
    let mut workspace_anchor = Some(open_child_directory_no_symlink(
        "seal-recovery workspace",
        &session_anchor,
        std::ffi::OsStr::new("mount"),
    )?);
    let record_path = session_dir.join("SESSION.json");
    let record: SessionRecord = read_recovery_json_at(
        &session_anchor,
        std::ffi::OsStr::new("SESSION.json"),
        &record_path,
    )?;
    validate_recovery_session_record(&record, request, &workspace_root)?;
    let sealing_path = quiesce::sealing_record_path(&session_dir);
    let sealing_name = std::ffi::OsStr::new("SEALING.json");
    if !matches!(
        record.phase,
        SessionPhase::Open
            | SessionPhase::Closing
            | SessionPhase::Sealing
            | SessionPhase::RecoveryRequired
            | SessionPhase::PublicationCommitted
    ) {
        return Err(PocError::RecoveryRequired(format!(
            "ratified session cannot recover from {:?}",
            record.phase
        )));
    }
    let mut sealing = if path_entry_exists_at(&session_anchor, sealing_name, &sealing_path)? {
        let sealing: SealingRecord =
            read_recovery_json_at(&session_anchor, sealing_name, &sealing_path)?;
        validate_recovery_sealing_record(&sealing, request)?;
        Some(sealing)
    } else {
        None
    };

    let attestation_path = session_dir.join(MOUNT_ATTESTATION_FILE);
    let attestation: OverlayMountAttestation = read_recovery_json_at(
        &session_anchor,
        std::ffi::OsStr::new(MOUNT_ATTESTATION_FILE),
        &attestation_path,
    )?;
    validate_mount_attestation_scope(&attestation, allocation, request, &workspace_root)?;
    let allocation_authority =
        AnchoredAllocationAuthority::pin_for_recovery(allocation, &attestation)?;
    let anchored_allocation = allocation_authority.anchored_handle(allocation);
    let _owner_lock =
        durable::FileLock::exclusive(&allocation_authority.owner_path().join("LOCK"))?;
    let cgroup_membership = validated_attested_cgroup_path(&attestation)?;

    let recovery_path = session_dir.join(RECOVERY_RECEIPT_FILE);
    let recovery_name = std::ffi::OsStr::new(RECOVERY_RECEIPT_FILE);
    if path_entry_exists_at(&session_anchor, recovery_name, &recovery_path)? {
        let receipt: SessionSealRecoveryReceipt =
            read_recovery_json_at(&session_anchor, recovery_name, &recovery_path)?;
        validate_recovery_receipt_scope(&receipt, allocation, request, &attestation)?;
        let lease_fence = lease::reaudit_terminal_session_fence_tuple_anchored_locked(
            &anchored_allocation,
            allocation_authority.owner(),
            &request.session_id,
            request.lease_epoch,
            request.owner_epoch,
            &request.prior_operation_id,
            operation_id,
        )?;
        if receipt.lease_fence != lease_fence {
            return Err(PocError::RecoveryRequired(
                "durable seal-recovery receipt has a different terminal lease fence".to_owned(),
            ));
        }
        match receipt.disposition {
            SessionSealRecoveryDisposition::Old => {
                if path_entry_exists_at(&session_anchor, sealing_name, &sealing_path)? {
                    return Err(PocError::RecoveryRequired(
                        "Old terminal recovery collided with a later Sealing record".to_owned(),
                    ));
                }
                require_absent_projection_at(
                    &session_anchor,
                    std::ffi::OsStr::new("STABLE.json"),
                    &session_dir.join("STABLE.json"),
                )?;
                require_absent_projection_at(
                    &session_anchor,
                    std::ffi::OsStr::new("QUIESCENCE.json"),
                    &session_dir.join("QUIESCENCE.json"),
                )?;
                require_pre_ratification_temporary_absent(
                    &session_anchor,
                    &session_dir,
                    operation_id,
                )?;
            }
            SessionSealRecoveryDisposition::CompleteNew => {
                let observed = sealing.as_ref().ok_or_else(|| {
                    PocError::RecoveryRequired(
                        "CompleteNew terminal recovery lost its durable Sealing record".to_owned(),
                    )
                })?;
                if receipt.sealing.as_ref() != Some(observed) {
                    return Err(PocError::RecoveryRequired(
                        "terminal recovery Sealing witness changed after completion".to_owned(),
                    ));
                }
            }
        }
        require_attested_mount_absent_anchored(
            &attestation,
            required_recovery_anchor(&workspace_anchor, "workspace")?,
        )?;
        let audit_identity = anchored_workspace_audit_identity(
            &attestation,
            required_recovery_anchor(&workspace_anchor, "workspace")?,
            AttestedMountCleanupState::AlreadyAbsent,
        )?;
        let post = wait_terminal_audit(&audit_identity, cgroup_membership.as_ref(), true)?;
        if !post.is_clear() {
            return Err(PocError::RecoveryRequired(
                "completed terminal recovery has residual process authority".to_owned(),
            ));
        }
        if let (Some(stable), Some(quiescence)) =
            (receipt.stable.as_ref(), receipt.quiescence.as_ref())
        {
            validate_terminal_stabilization_anchored(
                &session_anchor,
                &session_dir,
                operation_id,
                allocation,
                &allocation_authority,
                &request.session_id,
                request.owner_epoch,
                stable,
                Some(quiescence),
            )?;
            ensure_exact_projection(&session_anchor, &session_dir.join("STABLE.json"), stable)?;
            ensure_exact_projection(
                &session_anchor,
                &session_dir.join("QUIESCENCE.json"),
                quiescence,
            )?;
        }
        if record.phase != receipt.terminal_phase {
            persist_recovery_session_record(
                &session_anchor,
                &session_dir,
                request,
                receipt.terminal_phase,
                &workspace_root,
            )?;
        }
        allocation_authority.revalidate(allocation)?;
        return Ok(receipt);
    }

    let lease_fence = lease::fence_or_reaudit_terminal_session_anchored_locked(
        &anchored_allocation,
        allocation_authority.owner(),
        &request.session_id,
        request.lease_epoch,
        request.owner_epoch,
        &request.prior_operation_id,
        operation_id,
    )?;
    allocation_authority.revalidate(allocation)?;

    let stable_path = session_dir.join("STABLE.json");
    let quiescence_path = session_dir.join("QUIESCENCE.json");
    let stable_name = std::ffi::OsStr::new("STABLE.json");
    let quiescence_name = std::ffi::OsStr::new("QUIESCENCE.json");
    let stable_exists = path_entry_exists_at(&session_anchor, stable_name, &stable_path)?;
    let quiescence_exists =
        path_entry_exists_at(&session_anchor, quiescence_name, &quiescence_path)?;
    if quiescence_exists && !stable_exists {
        return Err(PocError::RecoveryRequired(
            "QUIESCENCE exists without its preceding STABLE projection".to_owned(),
        ));
    }

    let mut cleanup = None;
    if sealing.is_none() {
        if stable_exists || quiescence_exists {
            return Err(PocError::RecoveryRequired(
                "unratified session has stable/quiescence projections".to_owned(),
            ));
        }
        cleanup = Some(cleanup_terminal_mount(
            &attestation,
            &session_anchor,
            &mut workspace_anchor,
            false,
        )?);
        // The final record is the linearization boundary.  Re-read it only
        // after cleanup so a concurrently finishing original process can
        // never be classified Old if it made Sealing durable first.
        if path_entry_exists_at(&session_anchor, sealing_name, &sealing_path)? {
            let observed: SealingRecord =
                read_recovery_json_at(&session_anchor, sealing_name, &sealing_path)?;
            validate_recovery_sealing_record(&observed, request)?;
            sealing = Some(observed);
        } else {
            remove_pre_ratification_temporary(&session_anchor, &session_dir, request)?;
            if path_entry_exists_at(&session_anchor, sealing_name, &sealing_path)? {
                let observed: SealingRecord =
                    read_recovery_json_at(&session_anchor, sealing_name, &sealing_path)?;
                validate_recovery_sealing_record(&observed, request)?;
                sealing = Some(observed);
            }
        }
        if sealing.is_none() {
            let receipt = SessionSealRecoveryReceipt {
                schema_version: SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                prior_operation_id: request.prior_operation_id.clone(),
                session_id: request.session_id.clone(),
                allocation_id: request.allocation_id.clone(),
                lease_epoch: request.lease_epoch,
                owner_epoch: request.owner_epoch,
                lease_fence,
                disposition: SessionSealRecoveryDisposition::Old,
                terminal_phase: SessionPhase::RecoveryRequired,
                sealing: None,
                cleanup: cleanup.expect("terminal cleanup was captured"),
                stable: None,
                quiescence: None,
                completed_unix_ms: unix_time_ms()?,
            };
            allocation_authority.revalidate(allocation)?;
            write_recovery_immutable_json(&session_anchor, &recovery_path, &receipt)?;
            if record.phase != receipt.terminal_phase {
                persist_recovery_session_record(
                    &session_anchor,
                    &session_dir,
                    request,
                    receipt.terminal_phase,
                    &workspace_root,
                )?;
            }
            allocation_authority.revalidate(allocation)?;
            return Ok(receipt);
        }
    }

    reconcile_ratified_sealing_temporary(
        &session_anchor,
        &session_dir,
        operation_id,
        &sealing_path,
    )?;
    let sealing = sealing.expect("ratified branch has an exact Sealing record");
    let (stable, quiescence, cleanup) = if stable_exists && quiescence_exists {
        require_attested_mount_absent_anchored(
            &attestation,
            required_recovery_anchor(&workspace_anchor, "workspace")?,
        )?;
        let audit_identity = anchored_workspace_audit_identity(
            &attestation,
            required_recovery_anchor(&workspace_anchor, "workspace")?,
            AttestedMountCleanupState::AlreadyAbsent,
        )?;
        let stable: crate::StableAllocationReceipt =
            read_recovery_json_at(&session_anchor, stable_name, &stable_path)?;
        let quiescence: crate::QuiescenceReceipt =
            read_recovery_json_at(&session_anchor, quiescence_name, &quiescence_path)?;
        let post = wait_terminal_audit(&audit_identity, cgroup_membership.as_ref(), true)?;
        validate_terminal_stabilization_anchored(
            &session_anchor,
            &session_dir,
            operation_id,
            allocation,
            &allocation_authority,
            &request.session_id,
            request.owner_epoch,
            &stable,
            Some(&quiescence),
        )?;
        let cleanup = SessionSealCleanupWitness {
            killed_or_signaled_pids: quiescence.killed_or_signaled_pids.clone(),
            pre_unmount_audit: quiescence.pre_unmount_audit.clone(),
            post_unmount_audit: post,
            unmounted: attested_unmounted(&attestation),
        };
        (stable, quiescence, cleanup)
    } else {
        let prior_stable = if stable_exists {
            Some(read_recovery_json_at::<crate::StableAllocationReceipt>(
                &session_anchor,
                stable_name,
                &stable_path,
            )?)
        } else {
            None
        };
        if let Some(stable) = prior_stable.as_ref() {
            validate_terminal_stabilization_anchored(
                &session_anchor,
                &session_dir,
                operation_id,
                allocation,
                &allocation_authority,
                &request.session_id,
                request.owner_epoch,
                stable,
                None,
            )?;
        }
        let cleanup = match cleanup {
            Some(cleanup) => cleanup,
            None => {
                cleanup_terminal_mount(&attestation, &session_anchor, &mut workspace_anchor, true)?
            }
        };
        let (stable, quiescence) = stabilize_terminal_recovery_anchored(
            &session_anchor,
            &session_dir,
            operation_id,
            allocation,
            &allocation_authority,
            &request.session_id,
            request.owner_epoch,
            cleanup.killed_or_signaled_pids.clone(),
            cleanup.pre_unmount_audit.clone(),
            cleanup.post_unmount_audit.clone(),
            &cleanup.unmounted,
        )?;
        if prior_stable.as_ref().is_some_and(|prior| prior != &stable) {
            return Err(PocError::RecoveryRequired(
                "recovered stabilization differs from the durable STABLE projection".to_owned(),
            ));
        }
        (stable, quiescence, cleanup)
    };

    let terminal_phase = if record.phase == SessionPhase::PublicationCommitted {
        SessionPhase::PublicationCommitted
    } else {
        SessionPhase::RecoveryRequired
    };
    let receipt = SessionSealRecoveryReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        prior_operation_id: request.prior_operation_id.clone(),
        session_id: request.session_id.clone(),
        allocation_id: request.allocation_id.clone(),
        lease_epoch: request.lease_epoch,
        owner_epoch: request.owner_epoch,
        lease_fence,
        disposition: SessionSealRecoveryDisposition::CompleteNew,
        terminal_phase,
        sealing: Some(sealing),
        cleanup,
        stable: Some(stable),
        quiescence: Some(quiescence),
        completed_unix_ms: unix_time_ms()?,
    };
    allocation_authority.revalidate(allocation)?;
    write_recovery_immutable_json(&session_anchor, &recovery_path, &receipt)?;
    ensure_exact_projection(
        &session_anchor,
        &stable_path,
        receipt
            .stable
            .as_ref()
            .expect("CompleteNew has a stable receipt"),
    )?;
    ensure_exact_projection(
        &session_anchor,
        &quiescence_path,
        receipt
            .quiescence
            .as_ref()
            .expect("CompleteNew has a quiescence receipt"),
    )?;
    if record.phase != terminal_phase {
        persist_recovery_session_record(
            &session_anchor,
            &session_dir,
            request,
            terminal_phase,
            &workspace_root,
        )?;
    }
    allocation_authority.revalidate(allocation)?;
    Ok(receipt)
}

fn validate_session_record(
    record: &SessionRecord,
    lease: &MutableLease,
    workspace_root: &Path,
) -> PocResult<()> {
    if record.schema_version == SCHEMA_VERSION
        && record.session_id == lease.session_id
        && record.allocation_id == lease.allocation_id
        && record.lease_epoch == lease.lease_epoch
        && record.owner_epoch == lease.owner_epoch
        && record.workspace_root == workspace_root
    {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "durable session record differs from the exact recovery tuple".to_owned(),
        ))
    }
}

fn validate_recovery_session_record(
    record: &SessionRecord,
    request: &SessionSealRecoveryRequest,
    workspace_root: &Path,
) -> PocResult<()> {
    if record.schema_version == SCHEMA_VERSION
        && record.session_id == request.session_id
        && record.allocation_id == request.allocation_id
        && record.lease_epoch == request.lease_epoch
        && record.owner_epoch == request.owner_epoch
        && record.workspace_root == workspace_root
    {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "durable session record differs from the secret-free recovery tuple".to_owned(),
        ))
    }
}

fn validate_mount_attestation_scope(
    attestation: &OverlayMountAttestation,
    allocation: &AllocationHandle,
    request: &SessionSealRecoveryRequest,
    workspace_root: &Path,
) -> PocResult<()> {
    if attestation.schema_version == SCHEMA_VERSION
        && attestation.allocation_id == request.allocation_id
        && attestation.session_id == request.session_id
        && attestation.lease_epoch == request.lease_epoch
        && attestation.owner_epoch == request.owner_epoch
        && attestation.workspace_root == workspace_root
        && attestation.allocation_root == allocation.allocation_root
        && attestation.allocation_upper == allocation.upper_dir
        && attestation.allocation_work == allocation.work_dir
    {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "durable mount attestation differs from the exact recovery tuple".to_owned(),
        ))
    }
}

fn validate_recovery_receipt_scope(
    receipt: &SessionSealRecoveryReceipt,
    allocation: &AllocationHandle,
    request: &SessionSealRecoveryRequest,
    attestation: &OverlayMountAttestation,
) -> PocResult<()> {
    let cleanup_exact = receipt.cleanup.pre_unmount_audit.is_clear()
        && receipt.cleanup.post_unmount_audit.is_clear()
        && receipt.cleanup.pre_unmount_audit.workspace_root == attestation.workspace_root
        && receipt.cleanup.post_unmount_audit.workspace_root == attestation.workspace_root
        && receipt.cleanup.unmounted.workspace_root == attestation.workspace_root
        && receipt.cleanup.unmounted.allocation_root == allocation.allocation_root
        && receipt.cleanup.unmounted.allocation_upper == allocation.upper_dir
        && receipt.cleanup.unmounted.allocation_work == allocation.work_dir;
    let disposition_exact = match receipt.disposition {
        SessionSealRecoveryDisposition::Old => {
            receipt.terminal_phase == SessionPhase::RecoveryRequired
                && receipt.sealing.is_none()
                && receipt.stable.is_none()
                && receipt.quiescence.is_none()
        }
        SessionSealRecoveryDisposition::CompleteNew => {
            receipt.sealing.is_some()
                && receipt.stable.is_some()
                && receipt.quiescence.as_ref().is_some_and(|quiescence| {
                    quiescence.killed_or_signaled_pids == receipt.cleanup.killed_or_signaled_pids
                        && quiescence.pre_unmount_audit == receipt.cleanup.pre_unmount_audit
                        && quiescence.post_unmount_audit == receipt.cleanup.post_unmount_audit
                })
        }
    };
    let lease_fence_exact = receipt.lease_fence.schema_version == SCHEMA_VERSION
        && receipt.lease_fence.operation_id == request.operation_id
        && receipt.lease_fence.prior_operation_id == request.prior_operation_id
        && receipt.lease_fence.allocation_id == request.allocation_id
        && receipt.lease_fence.session_id == request.session_id
        && receipt.lease_fence.prior_lease_epoch == request.lease_epoch
        && receipt.lease_fence.prior_owner_epoch == request.owner_epoch
        && request
            .lease_epoch
            .checked_add(1)
            .is_some_and(|epoch| receipt.lease_fence.fenced_lease_epoch == epoch)
        && request
            .owner_epoch
            .checked_add(1)
            .is_some_and(|epoch| receipt.lease_fence.fenced_owner_epoch == epoch)
        && receipt.lease_fence.writer_revoked
        && receipt.lease_fence.deleter_revoked;
    if receipt.schema_version == SCHEMA_VERSION
        && receipt.operation_id == request.operation_id
        && receipt.prior_operation_id == request.prior_operation_id
        && receipt.session_id == request.session_id
        && receipt.allocation_id == request.allocation_id
        && receipt.lease_epoch == request.lease_epoch
        && receipt.owner_epoch == request.owner_epoch
        && matches!(
            receipt.terminal_phase,
            SessionPhase::RecoveryRequired | SessionPhase::PublicationCommitted
        )
        && cleanup_exact
        && disposition_exact
        && lease_fence_exact
    {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "durable seal-recovery receipt differs from the exact requested tuple".to_owned(),
        ))
    }
}

fn cleanup_terminal_mount(
    attestation: &OverlayMountAttestation,
    session_anchor: &std::os::fd::OwnedFd,
    workspace_anchor: &mut Option<std::os::fd::OwnedFd>,
    allow_already_absent: bool,
) -> PocResult<SessionSealCleanupWitness> {
    let mount_state = validate_attested_mount_for_cleanup_anchored(
        attestation,
        session_anchor,
        required_recovery_anchor(workspace_anchor, "workspace")?,
    )?;
    let audit_identity = anchored_workspace_audit_identity(
        attestation,
        required_recovery_anchor(workspace_anchor, "workspace")?,
        mount_state,
    )?;
    let cgroup_membership = validated_attested_cgroup_path(attestation)?;
    if mount_state == AttestedMountCleanupState::AlreadyAbsent {
        if !allow_already_absent {
            return Err(PocError::RecoveryRequired(
                "unratified terminal recovery cannot authenticate PID scope from an absent mount"
                    .to_owned(),
            ));
        }
        let audit = wait_terminal_audit(&audit_identity, cgroup_membership.as_ref(), true)?;
        require_attested_mount_absent_anchored(
            attestation,
            required_recovery_anchor(workspace_anchor, "workspace")?,
        )?;
        return Ok(SessionSealCleanupWitness {
            killed_or_signaled_pids: Vec::new(),
            pre_unmount_audit: audit.clone(),
            post_unmount_audit: audit,
            unmounted: attested_unmounted(attestation),
        });
    }
    let (mut killed_or_signaled_pids, _) = terminate_terminal_workspace_references_anchored(
        &audit_identity,
        cgroup_membership.as_ref(),
    )?;
    freeze_attested_mount_read_only_anchored(
        attestation,
        session_anchor,
        required_recovery_anchor(workspace_anchor, "workspace")?,
    )?;
    let (post_freeze_pids, pre_unmount_audit) = terminate_terminal_workspace_references_anchored(
        &audit_identity,
        cgroup_membership.as_ref(),
    )?;
    killed_or_signaled_pids.extend(post_freeze_pids);
    killed_or_signaled_pids.sort_unstable();
    killed_or_signaled_pids.dedup();
    let workspace = workspace_anchor.take().ok_or_else(|| {
        PocError::RecoveryRequired("terminal recovery lost its workspace descriptor".to_owned())
    })?;
    let unmounted =
        strict_unmount_attested_frozen_anchored(attestation, session_anchor, workspace)?;
    let post_unmount_workspace = open_child_directory_no_symlink(
        "terminal recovery post-unmount workspace",
        session_anchor,
        std::ffi::OsStr::new("mount"),
    )?;
    require_attested_mount_absent_anchored(attestation, &post_unmount_workspace)?;
    *workspace_anchor = Some(post_unmount_workspace);
    let post_unmount_identity = anchored_workspace_audit_identity(
        attestation,
        required_recovery_anchor(workspace_anchor, "workspace")?,
        AttestedMountCleanupState::AlreadyAbsent,
    )?;
    let post_unmount_audit =
        wait_terminal_audit(&post_unmount_identity, cgroup_membership.as_ref(), true)?;
    Ok(SessionSealCleanupWitness {
        killed_or_signaled_pids,
        pre_unmount_audit,
        post_unmount_audit,
        unmounted,
    })
}

fn required_recovery_anchor<'a>(
    anchor: &'a Option<std::os::fd::OwnedFd>,
    label: &str,
) -> PocResult<&'a std::os::fd::OwnedFd> {
    anchor.as_ref().ok_or_else(|| {
        PocError::RecoveryRequired(format!(
            "terminal recovery lost its pinned {label} descriptor"
        ))
    })
}

fn attested_unmounted(attestation: &OverlayMountAttestation) -> UnmountedOverlay {
    UnmountedOverlay {
        workspace_root: attestation.workspace_root.clone(),
        allocation_root: attestation.allocation_root.clone(),
        allocation_upper: attestation.allocation_upper.clone(),
        allocation_work: attestation.allocation_work.clone(),
    }
}

fn pre_ratification_temporary_path(session_dir: &Path, operation_id: &OperationId) -> PathBuf {
    session_dir.join(format!(".SEALING.{}.tmp", operation_id.as_str()))
}

fn remove_pre_ratification_temporary(
    session_anchor: &std::os::fd::OwnedFd,
    session_dir: &Path,
    request: &SessionSealRecoveryRequest,
) -> PocResult<()> {
    let path = pre_ratification_temporary_path(session_dir, &request.operation_id);
    let file_name = path.file_name().ok_or_else(|| {
        PocError::Integrity("pre-ratification Sealing temporary has no file name".to_owned())
    })?;
    let metadata = match rustix::fs::statat(
        session_anchor,
        file_name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => {
            return Err(PocError::io(
                "stat anchored Sealing temporary",
                &path,
                std::io::Error::from(error),
            ));
        }
    };
    if !raw_mode_is_regular(metadata.st_mode) {
        return Err(PocError::RecoveryRequired(format!(
            "pre-ratification Sealing temporary is not a regular file: {}",
            path.display()
        )));
    }
    let temporary: SealingRecord = read_recovery_json_at(session_anchor, file_name, &path)?;
    validate_recovery_sealing_record(&temporary, request)?;
    remove_anchored_regular_file(
        session_anchor,
        &path,
        metadata.st_dev as u64,
        metadata.st_ino as u64,
        "remove Sealing temporary",
    )
}

fn reconcile_ratified_sealing_temporary(
    session_anchor: &std::os::fd::OwnedFd,
    session_dir: &Path,
    operation_id: &OperationId,
    final_path: &Path,
) -> PocResult<()> {
    let temporary_path = pre_ratification_temporary_path(session_dir, operation_id);
    let temporary_name = temporary_path.file_name().ok_or_else(|| {
        PocError::Integrity("ratified Sealing temporary has no file name".to_owned())
    })?;
    let final_name = final_path.file_name().ok_or_else(|| {
        PocError::Integrity("ratified Sealing record has no file name".to_owned())
    })?;
    let temporary = match rustix::fs::statat(
        session_anchor,
        temporary_name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => {
            return Err(PocError::io(
                "stat anchored ratified Sealing temporary",
                &temporary_path,
                std::io::Error::from(error),
            ));
        }
    };
    let final_metadata = rustix::fs::statat(
        session_anchor,
        final_name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|error| {
        PocError::io(
            "stat anchored ratified Sealing record",
            final_path,
            std::io::Error::from(error),
        )
    })?;
    if !raw_mode_is_regular(temporary.st_mode) || !raw_mode_is_regular(final_metadata.st_mode) {
        return Err(PocError::RecoveryRequired(
            "ratified Sealing temporary or final is not a no-follow regular file".to_owned(),
        ));
    }
    if temporary.st_dev != final_metadata.st_dev || temporary.st_ino != final_metadata.st_ino {
        return Err(PocError::RecoveryRequired(
            "ratified Sealing temporary is not the exact installed final inode".to_owned(),
        ));
    }
    let temporary_record: SealingRecord =
        read_recovery_json_at(session_anchor, temporary_name, &temporary_path)?;
    let final_record: SealingRecord =
        read_recovery_json_at(session_anchor, final_name, final_path)?;
    if temporary_record != final_record {
        return Err(PocError::RecoveryRequired(
            "ratified Sealing temporary differs from the immutable final record".to_owned(),
        ));
    }
    remove_anchored_regular_file(
        session_anchor,
        &temporary_path,
        temporary.st_dev as u64,
        temporary.st_ino as u64,
        "remove exact ratified Sealing temporary",
    )
}

fn remove_anchored_regular_file(
    parent: &std::os::fd::OwnedFd,
    path: &Path,
    expected_device: u64,
    expected_inode: u64,
    operation: &'static str,
) -> PocResult<()> {
    let file_name = path
        .file_name()
        .ok_or_else(|| PocError::Integrity(format!("{operation} target has no file name")))?;
    let observed = rustix::fs::statat(parent, file_name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| PocError::io(operation, path, std::io::Error::from(error)))?;
    if !raw_mode_is_regular(observed.st_mode)
        || observed.st_dev as u64 != expected_device
        || observed.st_ino as u64 != expected_inode
    {
        return Err(PocError::RecoveryRequired(format!(
            "{operation} target changed after authentication: {}",
            path.display()
        )));
    }
    rustix::fs::unlinkat(parent, file_name, rustix::fs::AtFlags::empty())
        .map_err(|error| PocError::io(operation, path, std::io::Error::from(error)))?;
    rustix::fs::fsync(parent).map_err(|error| {
        PocError::io(
            "fsync anchored recovery directory",
            path.parent().unwrap_or_else(|| Path::new(".")),
            std::io::Error::from(error),
        )
    })
}

fn require_pre_ratification_temporary_absent(
    session_anchor: &std::os::fd::OwnedFd,
    session_dir: &Path,
    operation_id: &OperationId,
) -> PocResult<()> {
    let path = pre_ratification_temporary_path(session_dir, operation_id);
    let file_name = path.file_name().ok_or_else(|| {
        PocError::Integrity("pre-ratification Sealing temporary has no file name".to_owned())
    })?;
    require_absent_projection_at(session_anchor, file_name, &path)
}

fn require_absent_projection_at(
    parent: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    display_path: &Path,
) -> PocResult<()> {
    if path_entry_exists_at(parent, file_name, display_path)? {
        Err(PocError::RecoveryRequired(format!(
            "terminal Old outcome collided with {}",
            display_path.display()
        )))
    } else {
        Ok(())
    }
}

fn path_entry_exists_at(
    parent: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    display_path: &Path,
) -> PocResult<bool> {
    require_single_component_os("recovery control-record name", file_name)?;
    match rustix::fs::statat(parent, file_name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(PocError::io(
            "inspect anchored recovery control record",
            display_path,
            std::io::Error::from(error),
        )),
    }
}

fn path_entry_exists(path: &Path) -> PocResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(PocError::io("inspect durable session path", path, error)),
    }
}

fn require_single_component(label: &str, value: &str) -> PocResult<()> {
    let mut components = Path::new(value).components();
    let exact = matches!(components.next(), Some(std::path::Component::Normal(component)) if component == std::ffi::OsStr::new(value))
        && components.next().is_none();
    if value.is_empty() || !exact {
        return Err(PocError::RecoveryRequired(format!(
            "{label} is not one normalized path component"
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_canonical_allocation_paths(allocation: &AllocationHandle) -> PocResult<()> {
    if allocation.upper_dir == allocation.allocation_root.join("upper")
        && allocation.work_dir == allocation.allocation_root.join("work")
        && allocation.owner_dir == allocation.allocation_root.join("owner")
    {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "allocation handle has non-canonical root/upper/work/owner paths".to_owned(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn allocation_directory_identity(
    directory: &OwnedFd,
    context: &'static str,
    display_path: &Path,
) -> PocResult<AllocationDirectoryIdentity> {
    let status = rustix::fs::fstat(directory)
        .map_err(|error| PocError::io(context, display_path, std::io::Error::from(error)))?;
    if rustix::fs::FileType::from_raw_mode(status.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::Directory
    {
        return Err(PocError::RecoveryRequired(format!(
            "pinned allocation authority is not a directory: {}",
            display_path.display()
        )));
    }
    Ok(AllocationDirectoryIdentity {
        device: status.st_dev as u64,
        inode: status.st_ino as u64,
    })
}

#[cfg(target_os = "linux")]
fn allocation_descriptor_path(directory: &OwnedFd) -> PathBuf {
    PathBuf::from("/proc/self/fd").join(directory.as_raw_fd().to_string())
}

fn open_directory_no_symlink(label: &str, path: &Path) -> PocResult<std::os::fd::OwnedFd> {
    if !path.is_absolute() {
        return Err(PocError::RecoveryRequired(format!(
            "{label} must be an absolute no-symlink path: {}",
            path.display()
        )));
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut current =
        rustix::fs::open(Path::new("/"), flags, rustix::fs::Mode::empty()).map_err(|error| {
            PocError::io(
                "open recovery directory root",
                Path::new("/"),
                std::io::Error::from(error),
            )
        })?;
    for component in path.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(component) => {
                current = rustix::fs::openat(&current, component, flags, rustix::fs::Mode::empty())
                    .map_err(|error| {
                        PocError::io(
                            "open anchored recovery directory",
                            path,
                            std::io::Error::from(error),
                        )
                    })?;
            }
            _ => {
                return Err(PocError::RecoveryRequired(format!(
                    "{label} is not a normalized absolute path: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(current)
}

fn open_child_directory_no_symlink(
    label: &str,
    parent: &std::os::fd::OwnedFd,
    child: &std::ffi::OsStr,
) -> PocResult<std::os::fd::OwnedFd> {
    let mut components = Path::new(child).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(value)) if value == child)
        || components.next().is_some()
    {
        return Err(PocError::Integrity(format!(
            "{label} is not one normalized directory component"
        )));
    }
    rustix::fs::openat(
        parent,
        child,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open anchored recovery child directory",
            Path::new(child),
            std::io::Error::from(error),
        )
    })
}

/// Open recovery control records without following a substituted final
/// symlink, then prove the opened inode is the one inspected before parsing.
fn read_recovery_json<T: DeserializeOwned>(path: &Path) -> PocResult<T> {
    let before = std::fs::symlink_metadata(path)
        .map_err(|error| PocError::io("inspect recovery JSON", path, error))?;
    if !before.file_type().is_file() {
        return Err(PocError::RecoveryRequired(format!(
            "recovery control record is not a regular file: {}",
            path.display()
        )));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).map_err(|error| {
        PocError::io("open recovery JSON without symlink following", path, error)
    })?;
    let opened = file
        .metadata()
        .map_err(|error| PocError::io("stat opened recovery JSON", path, error))?;
    #[cfg(unix)]
    if before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err(PocError::RecoveryRequired(format!(
            "recovery control record changed while opening: {}",
            path.display()
        )));
    }
    if opened.len() > 16 * 1024 * 1024 {
        return Err(PocError::RecoveryRequired(format!(
            "recovery control record is oversized: {}",
            path.display()
        )));
    }
    serde_json::from_reader(file).map_err(PocError::from)
}

fn raw_mode_is_regular(mode: rustix::fs::RawMode) -> bool {
    rustix::fs::FileType::from_raw_mode(mode) == rustix::fs::FileType::RegularFile
}

fn read_recovery_json_at<T: DeserializeOwned>(
    parent: &std::os::fd::OwnedFd,
    file_name: &std::ffi::OsStr,
    display_path: &Path,
) -> PocResult<T> {
    require_single_component_os("recovery control-record name", file_name)?;
    let before = rustix::fs::statat(parent, file_name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| {
            PocError::io(
                "inspect anchored recovery JSON",
                display_path,
                std::io::Error::from(error),
            )
        })?;
    if !raw_mode_is_regular(before.st_mode as rustix::fs::RawMode) {
        return Err(PocError::RecoveryRequired(format!(
            "anchored recovery control record is not a regular file: {}",
            display_path.display()
        )));
    }
    let file_fd = rustix::fs::openat(
        parent,
        file_name,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open anchored recovery JSON",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let opened = rustix::fs::fstat(&file_fd).map_err(|error| {
        PocError::io(
            "stat anchored recovery JSON",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    if !raw_mode_is_regular(opened.st_mode as rustix::fs::RawMode)
        || before.st_dev != opened.st_dev
        || before.st_ino != opened.st_ino
        || opened.st_size > 16 * 1024 * 1024
    {
        return Err(PocError::RecoveryRequired(format!(
            "anchored recovery control record changed or is oversized: {}",
            display_path.display()
        )));
    }
    let value = serde_json::from_reader(std::fs::File::from(file_fd))?;
    let after = rustix::fs::statat(parent, file_name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|error| {
            PocError::io(
                "reinspect anchored recovery JSON",
                display_path,
                std::io::Error::from(error),
            )
        })?;
    if after.st_dev != opened.st_dev || after.st_ino != opened.st_ino {
        return Err(PocError::RecoveryRequired(format!(
            "anchored recovery control record changed while reading: {}",
            display_path.display()
        )));
    }
    Ok(value)
}

fn require_single_component_os(label: &str, value: &std::ffi::OsStr) -> PocResult<()> {
    let mut components = Path::new(value).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(component)) if component == value)
        || components.next().is_some()
    {
        return Err(PocError::Integrity(format!(
            "{label} is not one normalized path component"
        )));
    }
    Ok(())
}

fn write_recovery_immutable_json<T>(
    parent: &std::os::fd::OwnedFd,
    path: &Path,
    value: &T,
) -> PocResult<()>
where
    T: DeserializeOwned + Eq + Serialize,
{
    let file_name = path.file_name().ok_or_else(|| {
        PocError::RecoveryRequired("recovery control record has no file name".to_owned())
    })?;
    require_single_component_os("recovery control-record name", file_name)?;
    let temporary_name = std::ffi::OsString::from(format!(
        ".{}.{}.tmp",
        file_name
            .to_str()
            .ok_or_else(|| PocError::RecoveryRequired(
                "recovery control record has a non-UTF-8 name".to_owned()
            ))?,
        uuid::Uuid::new_v4()
    ));
    let temporary = path.with_file_name(&temporary_name);
    let bytes = serde_json::to_vec(value)?;
    let temporary_fd = rustix::fs::openat(
        parent,
        &temporary_name,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .map_err(|error| {
        PocError::io(
            "create anchored recovery temporary",
            &temporary,
            std::io::Error::from(error),
        )
    })?;
    let mut file = std::fs::File::from(temporary_fd);
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|error| PocError::io("write recovery control record", &temporary, error))?;
        file.sync_all()
            .map_err(|error| PocError::io("fsync recovery control record", &temporary, error))?;
        drop(file);
        match rustix::fs::linkat(
            parent,
            &temporary_name,
            parent,
            file_name,
            rustix::fs::AtFlags::empty(),
        ) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {
                let observed: T = read_recovery_json_at(parent, file_name, path)?;
                if &observed != value {
                    return Err(PocError::RecoveryRequired(format!(
                        "immutable recovery control-record collision at {}",
                        path.display()
                    )));
                }
            }
            Err(error) => {
                return Err(PocError::io(
                    "install immutable recovery control record",
                    path,
                    std::io::Error::from(error),
                ));
            }
        }
        rustix::fs::unlinkat(parent, &temporary_name, rustix::fs::AtFlags::empty()).map_err(
            |error| {
                PocError::io(
                    "remove anchored recovery temporary",
                    &temporary,
                    std::io::Error::from(error),
                )
            },
        )?;
        rustix::fs::fsync(parent).map_err(|error| {
            PocError::io(
                "fsync anchored recovery directory",
                path.parent().unwrap_or_else(|| Path::new(".")),
                std::io::Error::from(error),
            )
        })
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, &temporary_name, rustix::fs::AtFlags::empty());
    }
    result
}

fn replace_recovery_json<T: Serialize>(
    parent: &std::os::fd::OwnedFd,
    path: &Path,
    value: &T,
) -> PocResult<()> {
    let file_name = path.file_name().ok_or_else(|| {
        PocError::RecoveryRequired("recovery control record has no file name".to_owned())
    })?;
    require_single_component_os("recovery control-record name", file_name)?;
    let temporary_name = std::ffi::OsString::from(format!(
        ".{}.{}.tmp",
        file_name
            .to_str()
            .ok_or_else(|| PocError::RecoveryRequired(
                "recovery control record has a non-UTF-8 name".to_owned()
            ))?,
        uuid::Uuid::new_v4()
    ));
    let temporary = path.with_file_name(&temporary_name);
    let bytes = serde_json::to_vec(value)?;
    let temporary_fd = rustix::fs::openat(
        parent,
        &temporary_name,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .map_err(|error| {
        PocError::io(
            "create anchored recovery replacement",
            &temporary,
            std::io::Error::from(error),
        )
    })?;
    let mut file = std::fs::File::from(temporary_fd);
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|error| PocError::io("write recovery control record", &temporary, error))?;
        file.sync_all()
            .map_err(|error| PocError::io("fsync recovery control record", &temporary, error))?;
        drop(file);
        rustix::fs::renameat(parent, &temporary_name, parent, file_name).map_err(|error| {
            PocError::io(
                "replace anchored recovery control record",
                path,
                std::io::Error::from(error),
            )
        })?;
        rustix::fs::fsync(parent).map_err(|error| {
            PocError::io(
                "fsync anchored recovery directory",
                path.parent().unwrap_or_else(|| Path::new(".")),
                std::io::Error::from(error),
            )
        })
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, &temporary_name, rustix::fs::AtFlags::empty());
    }
    result
}

fn ensure_exact_projection<T>(
    parent: &std::os::fd::OwnedFd,
    path: &Path,
    expected: &T,
) -> PocResult<()>
where
    T: DeserializeOwned + Eq + Serialize,
{
    let file_name = path.file_name().ok_or_else(|| {
        PocError::RecoveryRequired("terminal recovery projection has no file name".to_owned())
    })?;
    if path_entry_exists_at(parent, file_name, path)? {
        let observed: T = read_recovery_json_at(parent, file_name, path)?;
        if &observed != expected {
            return Err(PocError::RecoveryRequired(format!(
                "terminal recovery projection collision at {}",
                path.display()
            )));
        }
        Ok(())
    } else {
        write_recovery_immutable_json(parent, path, expected)
    }
}

#[cfg(target_os = "linux")]
fn stabilize_terminal_recovery_anchored(
    session_anchor: &std::os::fd::OwnedFd,
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    allocation_authority: &AnchoredAllocationAuthority,
    session_id: &SessionId,
    owner_epoch: u64,
    killed_or_signaled_pids: Vec<i32>,
    pre_unmount_audit: ProcessAudit,
    post_unmount_audit: ProcessAudit,
    unmounted: &UnmountedOverlay,
) -> PocResult<(crate::StableAllocationReceipt, crate::QuiescenceReceipt)> {
    if !pre_unmount_audit.is_clear() || !post_unmount_audit.is_clear() {
        return Err(PocError::RecoveryRequired(
            "terminal recovery cannot stabilize with residual process authority".to_owned(),
        ));
    }
    allocation_authority.revalidate(allocation)?;
    syncfs_recovery_descriptor(allocation_authority.upper(), &allocation.upper_dir)?;
    rustix::fs::fsync(allocation_authority.owner()).map_err(|error| {
        PocError::io(
            "fsync pinned recovery allocation owner",
            &allocation.owner_dir,
            std::io::Error::from(error),
        )
    })?;
    allocation_authority.revalidate(allocation)?;
    let (before, after, first_sha256, second_sha256) = capture_terminal_witness_anchored(
        session_anchor,
        session_dir,
        allocation,
        allocation_authority,
    )?;
    if before != after || first_sha256 != second_sha256 {
        return Err(PocError::RecoveryRequired(format!(
            "allocation {} changed between terminal recovery witnesses",
            allocation.descriptor.allocation_id
        )));
    }
    let stable = crate::StableAllocationReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        allocation: allocation.descriptor.clone(),
        expected_owner_epoch: owner_epoch,
        before,
        after,
        sync_completed: true,
    };
    let quiescence = crate::QuiescenceReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        session_id: session_id.clone(),
        killed_or_signaled_pids,
        pre_unmount_audit,
        post_unmount_audit,
        workspace_root: unmounted.workspace_root.clone(),
        allocation_root: unmounted.allocation_root.clone(),
        syncfs_completed: true,
        first_inventory_sha256: first_sha256,
        second_inventory_sha256: second_sha256,
    };
    Ok((stable, quiescence))
}

#[cfg(target_os = "linux")]
fn validate_terminal_stabilization_anchored(
    session_anchor: &std::os::fd::OwnedFd,
    session_dir: &Path,
    operation_id: &OperationId,
    allocation: &AllocationHandle,
    allocation_authority: &AnchoredAllocationAuthority,
    session_id: &SessionId,
    owner_epoch: u64,
    stable: &crate::StableAllocationReceipt,
    quiescence: Option<&crate::QuiescenceReceipt>,
) -> PocResult<()> {
    if stable.schema_version != SCHEMA_VERSION
        || stable.operation_id != *operation_id
        || stable.allocation != allocation.descriptor
        || stable.expected_owner_epoch != owner_epoch
        || !stable.sync_completed
        || stable.before != stable.after
    {
        return Err(PocError::RecoveryRequired(
            "terminal stable receipt does not match the requested operation tuple".to_owned(),
        ));
    }
    if let Some(receipt) = quiescence {
        if receipt.schema_version != SCHEMA_VERSION
            || receipt.operation_id != *operation_id
            || receipt.session_id != *session_id
            || receipt.workspace_root != session_dir.join("mount")
            || receipt.allocation_root != allocation.allocation_root
            || !receipt.syncfs_completed
            || !receipt.pre_unmount_audit.is_clear()
            || !receipt.post_unmount_audit.is_clear()
            || receipt.first_inventory_sha256 != receipt.second_inventory_sha256
        {
            return Err(PocError::RecoveryRequired(
                "terminal quiescence receipt does not match the requested operation tuple"
                    .to_owned(),
            ));
        }
    }
    let (before, after, first_sha256, second_sha256) = capture_terminal_witness_anchored(
        session_anchor,
        session_dir,
        allocation,
        allocation_authority,
    )?;
    if before != stable.before
        || after != stable.after
        || before != after
        || first_sha256 != second_sha256
        || quiescence.is_some_and(|receipt| {
            receipt.first_inventory_sha256 != first_sha256
                || receipt.second_inventory_sha256 != second_sha256
        })
    {
        return Err(PocError::RecoveryRequired(
            "fresh terminal witness differs from the persisted stabilization".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn capture_terminal_witness_anchored(
    session_anchor: &std::os::fd::OwnedFd,
    session_dir: &Path,
    allocation: &AllocationHandle,
    allocation_authority: &AnchoredAllocationAuthority,
) -> PocResult<(
    crate::PhysicalSnapshot,
    crate::PhysicalSnapshot,
    String,
    String,
)> {
    let receipt_hit_name = std::ffi::OsStr::new("RECEIPT-HIT.json");
    let receipt_hit_path = session_dir.join(receipt_hit_name);
    if path_entry_exists_at(session_anchor, receipt_hit_name, &receipt_hit_path)? {
        let input: ReceiptHitSealInput =
            read_recovery_json_at(session_anchor, receipt_hit_name, &receipt_hit_path)?;
        quiesce::validate_receipt_hit_input(&input)?;
        allocation_authority.revalidate(allocation)?;
        let before = crate::inventory::capture_physical_witness_anchored(
            allocation,
            allocation_authority.upper(),
            &input.affected_paths,
        )?;
        thread::yield_now();
        let after = crate::inventory::capture_physical_witness_anchored(
            allocation,
            allocation_authority.upper(),
            &input.affected_paths,
        )?;
        allocation_authority.revalidate(allocation)?;
        let first_sha256 = digest_recovery_json(&before)?;
        let second_sha256 = digest_recovery_json(&after)?;
        return Ok((before, after, first_sha256, second_sha256));
    }
    allocation_authority.revalidate(allocation)?;
    let first =
        crate::inventory::capture_inventory_anchored(allocation, allocation_authority.upper())?;
    thread::yield_now();
    let second =
        crate::inventory::capture_inventory_anchored(allocation, allocation_authority.upper())?;
    allocation_authority.revalidate(allocation)?;
    Ok((
        first.physical,
        second.physical,
        first.inventory_sha256,
        second.inventory_sha256,
    ))
}

#[cfg(target_os = "linux")]
fn syncfs_recovery_descriptor(directory: &OwnedFd, display_path: &Path) -> PocResult<()> {
    // SAFETY: `syncfs(2)` only consumes the valid borrowed descriptor and does
    // not retain it or dereference user memory.
    let result = unsafe { libc::syncfs(directory.as_raw_fd()) };
    if result == 0 {
        Ok(())
    } else {
        Err(PocError::io(
            "syncfs pinned recovery allocation filesystem",
            display_path,
            std::io::Error::last_os_error(),
        ))
    }
}

fn digest_recovery_json<T: Serialize>(value: &T) -> PocResult<String> {
    use std::fmt::Write as _;

    let bytes = Sha256::digest(serde_json::to_vec(value)?);
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

fn wait_terminal_audit(
    identity: &AnchoredWorkspaceAuditIdentity,
    cgroup_membership: Option<&AttestedCgroupMembership>,
    include_mount_namespaces: bool,
) -> PocResult<ProcessAudit> {
    let deadline = Instant::now() + TERMINAL_AUDIT_BUDGET;
    loop {
        let audit = audit_terminal_workspace_anchored(
            identity,
            cgroup_membership,
            include_mount_namespaces,
        )?;
        if audit.is_clear() {
            return Ok(audit);
        }
        if Instant::now() >= deadline {
            return Err(PocError::RecoveryRequired(format!(
                "terminal recovery process audit did not clear: {audit:?}"
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// M0 session binding. Payload state lives only in `allocation`; the session
/// directory holds control metadata plus a disposable mountpoint.
#[derive(Debug)]
pub struct MplaSession {
    session_dir: PathBuf,
    runtime_session_dir: PathBuf,
    session_anchor: Option<OwnedFd>,
    #[cfg(target_os = "linux")]
    allocation_authority: AnchoredAllocationAuthority,
    allocation: AllocationHandle,
    lease: MutableLease,
    phase: SessionPhase,
    process_tree: ManagedProcessTree,
    overlay: Option<PermanentOverlayMount>,
}

impl MplaSession {
    #[cfg(target_os = "linux")]
    pub fn open(
        control_root: &Path,
        allocation: AllocationHandle,
        lease: MutableLease,
        lower_dirs_newest_first: Vec<PathBuf>,
        cgroup_procs_path: Option<PathBuf>,
    ) -> PocResult<Self> {
        std::fs::create_dir_all(control_root)
            .map_err(|error| PocError::io("create session control root", control_root, error))?;
        let control_root_anchor = open_directory_no_symlink("session control root", control_root)?;
        let allocation_upper =
            open_directory_no_symlink("allocation upper", &allocation.upper_dir)?;
        let allocation_work = open_directory_no_symlink("allocation work", &allocation.work_dir)?;
        Self::open_anchored(
            control_root,
            &control_root_anchor,
            allocation,
            lease,
            lower_dirs_newest_first,
            &allocation_upper,
            &allocation_work,
            cgroup_procs_path,
        )
    }

    #[cfg(not(target_os = "linux"))]
    pub fn open(
        _control_root: &Path,
        _allocation: AllocationHandle,
        _lease: MutableLease,
        _lower_dirs_newest_first: Vec<PathBuf>,
        _cgroup_procs_path: Option<PathBuf>,
    ) -> PocResult<Self> {
        Err(PocError::Unsupported(
            "MPLA sessions require Linux descriptor-anchored mounts".to_owned(),
        ))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn open_anchored(
        control_root_label: &Path,
        control_root: &OwnedFd,
        allocation: AllocationHandle,
        lease: MutableLease,
        lower_dirs_newest_first: Vec<PathBuf>,
        allocation_upper: &OwnedFd,
        allocation_work: &OwnedFd,
        cgroup_procs_path: Option<PathBuf>,
    ) -> PocResult<Self> {
        let allocation_authority = AnchoredAllocationAuthority::pin_for_session(
            &allocation,
            allocation_upper,
            allocation_work,
        )?;
        let prepared = prepare_external_session_anchored(
            control_root_label,
            control_root,
            &allocation,
            &lease,
        )?;
        let overlay = mount_permanent_overlay_anchored(
            &allocation,
            lower_dirs_newest_first,
            &prepared.workspace_root,
            &prepared.session,
            &prepared.workspace,
            allocation_upper,
            allocation_work,
        )?;
        allocation_authority.revalidate(&allocation)?;
        let runtime_workspace = overlay.anchored_runtime_workspace_root()?;
        let process_tree = ManagedProcessTree::new(runtime_workspace, cgroup_procs_path)?;
        let attestation = overlay.attest_anchored(
            &lease,
            process_tree.cgroup_attestation(),
            &prepared.session,
            &prepared.workspace,
            allocation_authority.root(),
            allocation_authority.owner(),
        )?;
        write_recovery_immutable_json(
            &prepared.session,
            &prepared.session_dir.join(MOUNT_ATTESTATION_FILE),
            &attestation,
        )?;
        let runtime_session_dir =
            PathBuf::from(format!("/proc/self/fd/{}", prepared.session.as_raw_fd()));
        let session = Self {
            session_dir: prepared.session_dir,
            runtime_session_dir,
            session_anchor: Some(prepared.session),
            allocation_authority,
            allocation,
            lease,
            phase: SessionPhase::Open,
            process_tree,
            overlay: Some(overlay),
        };
        drop(prepared.workspace);
        session.persist_record()?;
        Ok(session)
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn open_anchored(
        _control_root_label: &Path,
        _control_root: &OwnedFd,
        _allocation: AllocationHandle,
        _lease: MutableLease,
        _lower_dirs_newest_first: Vec<PathBuf>,
        _allocation_upper: &OwnedFd,
        _allocation_work: &OwnedFd,
        _cgroup_procs_path: Option<PathBuf>,
    ) -> PocResult<Self> {
        Err(PocError::Unsupported(
            "descriptor-anchored MPLA sessions require Linux".to_owned(),
        ))
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.lease.session_id
    }

    #[must_use]
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    #[must_use]
    pub fn allocation(&self) -> &AllocationHandle {
        &self.allocation
    }

    #[must_use]
    pub fn mutable_lease(&self) -> &MutableLease {
        &self.lease
    }

    #[must_use]
    pub const fn phase(&self) -> SessionPhase {
        self.phase
    }

    #[must_use]
    pub fn workspace_root(&self) -> Option<&Path> {
        self.overlay
            .as_ref()
            .map(PermanentOverlayMount::workspace_root)
    }

    pub(crate) fn compare_and_adopt_after_intent(
        &self,
        stable: &StableAllocationReceipt,
        request: &OwnerTransitionRequest,
        after_durable_intent: impl FnOnce() -> PocResult<()>,
    ) -> PocResult<AdoptionReceipt> {
        #[cfg(target_os = "linux")]
        return self.allocation_authority.compare_and_adopt_after_intent(
            &self.allocation,
            stable,
            request,
            after_durable_intent,
        );
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (stable, request, after_durable_intent);
            Err(PocError::Unsupported(
                "descriptor-anchored session adoption requires Linux".to_owned(),
            ))
        }
    }

    pub(crate) fn stale_capabilities_rejected(&self) -> PocResult<(bool, bool)> {
        #[cfg(target_os = "linux")]
        return self
            .allocation_authority
            .stale_capabilities_rejected(&self.allocation, &self.lease);
        #[cfg(not(target_os = "linux"))]
        Err(PocError::Unsupported(
            "descriptor-anchored session capability fencing requires Linux".to_owned(),
        ))
    }

    pub fn execute(
        &mut self,
        capability: &WriterCapability,
        program: &Path,
        arguments: &[String],
        timeout: Duration,
    ) -> PocResult<CommandReceipt> {
        if self.phase != SessionPhase::Open {
            return Err(PocError::StaleCapability {
                capability: "writer",
                allocation_id: self.lease.allocation_id.to_string(),
                expected_epoch: self.lease.lease_epoch,
                observed_epoch: capability.lease_epoch,
            });
        }
        #[cfg(target_os = "linux")]
        {
            self.allocation_authority.revalidate(&self.allocation)?;
            let anchored = self.allocation_authority.anchored_handle(&self.allocation);
            lease::validate_writer_anchored(
                &anchored,
                self.allocation_authority.owner(),
                capability,
            )?;
            self.allocation_authority.revalidate(&self.allocation)?;
        }
        #[cfg(not(target_os = "linux"))]
        lease::validate_writer(&self.allocation.allocation_root, capability)?;
        self.process_tree.run(program, arguments, timeout)
    }

    pub fn probe_readiness(
        &mut self,
        capability: &WriterCapability,
        relative_path: &Path,
        contains: Option<&[u8]>,
        timeout: Duration,
    ) -> PocResult<CommandReceipt> {
        if self.phase != SessionPhase::Open {
            return Err(PocError::StaleCapability {
                capability: "writer",
                allocation_id: self.lease.allocation_id.to_string(),
                expected_epoch: self.lease.lease_epoch,
                observed_epoch: capability.lease_epoch,
            });
        }
        #[cfg(target_os = "linux")]
        {
            self.allocation_authority.revalidate(&self.allocation)?;
            let anchored = self.allocation_authority.anchored_handle(&self.allocation);
            lease::validate_writer_anchored(
                &anchored,
                self.allocation_authority.owner(),
                capability,
            )?;
            self.allocation_authority.revalidate(&self.allocation)?;
        }
        #[cfg(not(target_os = "linux"))]
        lease::validate_writer(&self.allocation.allocation_root, capability)?;
        self.process_tree
            .probe_file(relative_path, contains, timeout)
    }

    /// Cross the terminal Sealing boundary and produce a stable allocation
    /// receipt. Only a failure proven to precede the durable Sealing record may
    /// restore this session to Open.
    pub fn seal(
        &mut self,
        operation_id: &OperationId,
        faults: &mut FaultInjector,
    ) -> PocResult<SealedAllocation> {
        let overlay = self.begin_sealing(operation_id, faults)?;
        #[cfg(target_os = "linux")]
        let result = quiesce::quiesce_and_stabilize_anchored(
            &self.runtime_session_dir,
            operation_id,
            &self.allocation,
            &self.allocation_authority,
            &self.lease,
            &mut self.process_tree,
            overlay,
            faults,
        );
        #[cfg(not(target_os = "linux"))]
        let result = quiesce::quiesce_and_stabilize(
            &self.runtime_session_dir,
            operation_id,
            &self.allocation,
            &self.lease,
            &mut self.process_tree,
            overlay,
            faults,
        );
        result.inspect_err(|_error| {
            self.phase = SessionPhase::RecoveryRequired;
            let _ = self.persist_record();
        })
    }

    pub fn seal_receipt_hit(
        &mut self,
        operation_id: &OperationId,
        input: &ReceiptHitSealInput,
        faults: &mut FaultInjector,
    ) -> PocResult<ReceiptSealedAllocation> {
        self.ensure_open_for_sealing()?;
        quiesce::validate_receipt_hit_input(input)?;
        durable::replace_json(&self.runtime_session_dir.join("RECEIPT-HIT.json"), input)?;
        let overlay = self.begin_sealing(operation_id, faults)?;
        #[cfg(target_os = "linux")]
        let result = quiesce::quiesce_and_stabilize_receipt_hit_anchored(
            &self.runtime_session_dir,
            operation_id,
            &self.allocation,
            &self.allocation_authority,
            &self.lease,
            &mut self.process_tree,
            overlay,
            faults,
        );
        #[cfg(not(target_os = "linux"))]
        let result = quiesce::quiesce_and_stabilize_receipt_hit(
            &self.runtime_session_dir,
            operation_id,
            &self.allocation,
            &self.lease,
            &mut self.process_tree,
            overlay,
            faults,
        );
        result.inspect_err(|_error| {
            self.phase = SessionPhase::RecoveryRequired;
            let _ = self.persist_record();
        })
    }

    fn begin_sealing(
        &mut self,
        operation_id: &OperationId,
        faults: &mut FaultInjector,
    ) -> PocResult<PermanentOverlayMount> {
        self.ensure_open_for_sealing()?;
        #[cfg(target_os = "linux")]
        let _seal_recovery_lock = lock_original_seal_against_recovery_anchored(
            &self.allocation,
            &self.allocation_authority,
            &self.runtime_session_dir,
            operation_id,
        )?;
        #[cfg(not(target_os = "linux"))]
        let _seal_recovery_lock = lock_original_seal_against_recovery(
            &self.allocation,
            &self.runtime_session_dir,
            operation_id,
        )?;
        #[cfg(target_os = "linux")]
        let allocation_upper = self.allocation_authority.upper_path();
        #[cfg(not(target_os = "linux"))]
        let allocation_upper = self.allocation.upper_dir.clone();
        let state_paths = [
            self.session_dir.join("SESSION.json"),
            quiesce::sealing_record_path(&self.session_dir),
        ];
        let mut named_faults = NamedFaultInjector::default()
            .with_physical_context(operation_id.as_str(), state_paths.clone());
        faults.hit(FaultPoint::BeforeSealing, false)?;
        reach_real_operation(
            &mut named_faults,
            NamedFaultPoint::FenceBeforeClose,
            operation_id,
            [self.session_dir.join("SESSION.json")],
            Some(&allocation_upper),
            false,
        )?;
        self.phase = SessionPhase::Closing;
        self.process_tree.fence();
        if let Err(error) = self.persist_record() {
            self.phase = SessionPhase::Open;
            self.process_tree.unfence();
            return Err(error);
        }
        reach_real_operation(
            &mut named_faults,
            NamedFaultPoint::FenceAfterClose,
            operation_id,
            [self.session_dir.join("SESSION.json")],
            Some(&allocation_upper),
            false,
        )?;
        self.process_tree
            .drain_in_flight_commands(Duration::from_secs(1))?;
        reach_real_operation(
            &mut named_faults,
            NamedFaultPoint::FenceAfterDrain,
            operation_id,
            [self.session_dir.join("SESSION.json")],
            Some(&allocation_upper),
            false,
        )?;

        let sealing_path = quiesce::sealing_record_path(&self.runtime_session_dir);
        if let Err(error) = quiesce::persist_sealing(
            &self.runtime_session_dir,
            operation_id,
            &self.lease,
            &allocation_upper,
            &mut named_faults,
        ) {
            let temporary_path =
                pre_ratification_temporary_path(&self.runtime_session_dir, operation_id);
            let boundary_entry = path_entry_exists(&sealing_path).and_then(|final_exists| {
                if final_exists {
                    Ok(true)
                } else {
                    path_entry_exists(&temporary_path)
                }
            });
            match boundary_entry {
                Ok(false) => {}
                boundary_state => {
                    self.phase = SessionPhase::RecoveryRequired;
                    let _ = self.persist_record();
                    return Err(PocError::RecoveryRequired(format!(
                        "Sealing publication left a final/temporary entry or could not be audited: {error}; entry audit: {boundary_state:?}"
                    )));
                }
            }
            self.phase = SessionPhase::Open;
            self.process_tree.unfence();
            let _ = self.persist_record();
            return Err(error);
        }
        self.phase = SessionPhase::Sealing;
        self.persist_record().map_err(|error| {
            PocError::RecoveryRequired(format!(
                "session phase write failed after durable Sealing: {error}"
            ))
        })?;
        faults.hit(FaultPoint::AfterSealingDurable, true)?;

        self.overlay.take().ok_or_else(|| {
            PocError::RecoveryRequired("sealed session has no live overlay guard".to_owned())
        })
    }

    fn ensure_open_for_sealing(&self) -> PocResult<()> {
        if self.phase == SessionPhase::Open {
            Ok(())
        } else {
            Err(PocError::Integrity(format!(
                "session {} cannot seal from {:?}",
                self.lease.session_id, self.phase
            )))
        }
    }

    pub fn mark_publication_committed(&mut self) -> PocResult<()> {
        if self.phase != SessionPhase::Sealing {
            return Err(PocError::Integrity(format!(
                "session {} cannot commit publication from {:?}",
                self.lease.session_id, self.phase
            )));
        }
        self.phase = SessionPhase::PublicationCommitted;
        self.persist_record()
    }

    pub fn mark_recovery_required(&mut self) -> PocResult<()> {
        if self.phase == SessionPhase::Open || self.phase == SessionPhase::Closing {
            return Err(PocError::Integrity(
                "pre-Sealing session cannot be marked terminal recovery by this path".to_owned(),
            ));
        }
        self.phase = SessionPhase::RecoveryRequired;
        self.persist_record()
    }

    fn persist_record(&self) -> PocResult<()> {
        let workspace_root = self
            .overlay
            .as_ref()
            .map(|overlay| overlay.workspace_root().to_path_buf())
            .unwrap_or_else(|| self.session_dir.join("mount"));
        if let Some(session) = &self.session_anchor {
            replace_recovery_json(
                session,
                &self.session_dir.join("SESSION.json"),
                &SessionRecord {
                    schema_version: SCHEMA_VERSION,
                    session_id: self.lease.session_id.clone(),
                    allocation_id: self.lease.allocation_id.clone(),
                    lease_epoch: self.lease.lease_epoch,
                    owner_epoch: self.lease.owner_epoch,
                    phase: self.phase,
                    workspace_root,
                    updated_unix_ms: unix_time_ms()?,
                },
            )
        } else {
            persist_session_record(&self.session_dir, &self.lease, self.phase, &workspace_root)
        }
    }
}

fn persist_session_record(
    session_dir: &Path,
    lease: &MutableLease,
    phase: SessionPhase,
    workspace_root: &Path,
) -> PocResult<()> {
    durable::replace_json(
        &session_dir.join("SESSION.json"),
        &SessionRecord {
            schema_version: SCHEMA_VERSION,
            session_id: lease.session_id.clone(),
            allocation_id: lease.allocation_id.clone(),
            lease_epoch: lease.lease_epoch,
            owner_epoch: lease.owner_epoch,
            phase,
            workspace_root: workspace_root.to_path_buf(),
            updated_unix_ms: unix_time_ms()?,
        },
    )
}

fn persist_recovery_session_record(
    session_anchor: &std::os::fd::OwnedFd,
    session_dir: &Path,
    request: &SessionSealRecoveryRequest,
    phase: SessionPhase,
    workspace_root: &Path,
) -> PocResult<()> {
    replace_recovery_json(
        session_anchor,
        &session_dir.join("SESSION.json"),
        &SessionRecord {
            schema_version: SCHEMA_VERSION,
            session_id: request.session_id.clone(),
            allocation_id: request.allocation_id.clone(),
            lease_epoch: request.lease_epoch,
            owner_epoch: request.owner_epoch,
            phase,
            workspace_root: workspace_root.to_path_buf(),
            updated_unix_ms: unix_time_ms()?,
        },
    )
}

impl Drop for MplaSession {
    fn drop(&mut self) {
        self.process_tree.fence();
        let _ = self.process_tree.stop_kill_reap();
        if let Some(overlay) = self.overlay.take() {
            let _ = overlay.strict_unmount();
        }
    }
}
