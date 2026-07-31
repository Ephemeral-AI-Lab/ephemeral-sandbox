use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fault::{FaultInjector, FaultPoint};
use crate::inventory::AllocationInventory;
use crate::publication::{PublicationOperationRecord, StationaryPublicationRequest};
use crate::session::PreparedExternalSession;
use crate::{
    durable, lease, owner, AdoptionReceipt, AllocationHandle, MutableLease, OwnerTransitionRequest,
    PocError, PocResult, PublicationPhase, StableAllocationReceipt, StorageAdminAction,
    StorageAdminOutcome, StorageAdminReceipt, INTERFACE_VERSION, SCHEMA_VERSION,
    STORAGE_ADMIN_PRIVILEGED_SYSCALLS, STORAGE_ADMIN_TRUSTED_EXECUTABLE,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalStationarySeal {
    pub quiesce: StorageAdminReceipt,
    pub strict_unmount: StorageAdminReceipt,
    pub first_inventory: AllocationInventory,
    pub second_inventory: AllocationInventory,
    pub workload_cgroup_empty: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalStationaryPublicationReceipt {
    pub schema_version: u32,
    pub request: StationaryPublicationRequest,
    pub stable: StableAllocationReceipt,
    pub adoption: AdoptionReceipt,
    pub seal: ExternalStationarySeal,
    pub allocation_path_before: PathBuf,
    pub allocation_path_after: PathBuf,
    pub representative_inodes_unchanged: bool,
    pub allocated_bytes_unchanged: bool,
    pub no_second_payload_allocation: bool,
    pub stale_writer_rejected: bool,
    pub stale_deleter_rejected: bool,
    pub idempotent_replay: bool,
}

pub fn stationary_adopt_prepared(
    prepared: &PreparedExternalSession,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    request: &StationaryPublicationRequest,
    operations_root: &Path,
    seal: ExternalStationarySeal,
    faults: &mut FaultInjector,
) -> PocResult<ExternalStationaryPublicationReceipt> {
    validate_request(request)?;
    let session = prepared.validate_stationary_binding(allocation, lease, &request.operation_id)?;
    let operation_dir = operations_root
        .join("publication")
        .join(request.operation_id.as_str());
    std::fs::create_dir_all(&operation_dir).map_err(|error| {
        PocError::io(
            "create external publication operation directory",
            &operation_dir,
            error,
        )
    })?;
    validate_operation_identity(
        &operation_dir,
        request,
        &allocation.descriptor.allocation_id,
    )?;

    let receipt_path = operation_dir.join("external-stationary-adoption.json");
    if receipt_path.exists() {
        let mut receipt: ExternalStationaryPublicationReceipt = durable::read_json(&receipt_path)?;
        validate_replayed_receipt(&receipt, request, allocation, lease)?;
        persist_operation(
            &operation_dir,
            request,
            allocation,
            PublicationPhase::PayloadOwned,
            Some(receipt.stable_inventory_sha256().to_owned()),
        )?;
        receipt.idempotent_replay = true;
        return Ok(receipt);
    }
    if session.phase == crate::SessionPhase::PublicationCommitted {
        return Err(PocError::RecoveryRequired(
            "external session is publication-committed without its durable adoption receipt"
                .to_owned(),
        ));
    }

    let inventory_sha256 = seal.first_inventory.inventory_sha256.clone();
    let result = complete_stationary_adoption(
        prepared,
        allocation,
        lease,
        request,
        &operation_dir,
        seal,
        faults,
    );
    if result.is_err() {
        mark_recovery_required(
            prepared,
            allocation,
            lease,
            &operation_dir,
            request,
            Some(&inventory_sha256),
        );
    }
    result
}

fn complete_stationary_adoption(
    prepared: &PreparedExternalSession,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    request: &StationaryPublicationRequest,
    operation_dir: &Path,
    seal: ExternalStationarySeal,
    faults: &mut FaultInjector,
) -> PocResult<ExternalStationaryPublicationReceipt> {
    let stable = validate_external_seal(allocation, lease, prepared, request, &seal)?;
    persist_operation(
        operation_dir,
        request,
        allocation,
        PublicationPhase::StableAllocation,
        Some(seal.first_inventory.inventory_sha256.clone()),
    )?;
    faults.hit(FaultPoint::AfterStableAllocation, true)?;

    let owner_request = OwnerTransitionRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        publication_id: request.publication_id.clone(),
        session_id: lease.session_id.clone(),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        expected_lease_epoch: lease.lease_epoch,
        expected_owner_epoch: lease.owner_epoch,
    };
    let adoption = owner::compare_and_adopt_after_intent(
        &allocation.allocation_root,
        &stable,
        &owner_request,
        || faults.hit(FaultPoint::AfterOwnerIntent, true),
    )?;
    faults.hit(FaultPoint::AfterOwnerAdoption, true)?;

    let stale_writer_rejected =
        lease::validate_writer(&allocation.allocation_root, &lease.writer).is_err();
    let stale_deleter_rejected =
        lease::validate_deleter(&allocation.allocation_root, &lease.deleter).is_err();
    if !stale_writer_rejected || !stale_deleter_rejected {
        return Err(PocError::Integrity(
            "external adoption did not terminally fence both stale capabilities".to_owned(),
        ));
    }

    let receipt = build_receipt(
        request,
        stable,
        adoption,
        seal,
        stale_writer_rejected,
        stale_deleter_rejected,
    )?;
    durable::replace_json(
        &operation_dir.join("external-stationary-adoption.json"),
        &receipt,
    )?;
    // Stationary adoption ends at PayloadOwned. The paired-ref directory
    // fsync performed by the public lifecycle is the later publication
    // linearization point, so this helper must not claim publication before
    // that ref exists.
    persist_operation(
        operation_dir,
        request,
        allocation,
        PublicationPhase::PayloadOwned,
        Some(receipt.stable_inventory_sha256().to_owned()),
    )?;
    Ok(receipt)
}

impl ExternalStationaryPublicationReceipt {
    #[must_use]
    pub fn stable_inventory_sha256(&self) -> &str {
        &self.seal.first_inventory.inventory_sha256
    }
}

fn validate_request(request: &StationaryPublicationRequest) -> PocResult<()> {
    if request.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(format!(
            "unsupported external stationary publication schema {}",
            request.schema_version
        )));
    }
    if request.operation_id.as_str().is_empty() || request.publication_id.as_str().is_empty() {
        return Err(PocError::Integrity(
            "external stationary publication identifiers cannot be empty".to_owned(),
        ));
    }
    Ok(())
}

fn validate_external_seal(
    allocation: &AllocationHandle,
    lease: &MutableLease,
    prepared: &PreparedExternalSession,
    request: &StationaryPublicationRequest,
    seal: &ExternalStationarySeal,
) -> PocResult<StableAllocationReceipt> {
    if !seal.workload_cgroup_empty {
        return Err(PocError::RecoveryRequired(
            "external stationary publication lacks an empty workload-cgroup proof".to_owned(),
        ));
    }
    validate_storage_receipt(
        &seal.quiesce,
        StorageAdminAction::Quiesce,
        allocation,
        lease,
        prepared,
    )?;
    validate_storage_receipt(
        &seal.strict_unmount,
        StorageAdminAction::StrictUnmount,
        allocation,
        lease,
        prepared,
    )?;
    if seal.quiesce.scope != seal.strict_unmount.scope
        || seal.quiesce.mount_receipt_binding != seal.strict_unmount.mount_receipt_binding
        || seal.quiesce.profile_id != seal.strict_unmount.profile_id
        || seal.quiesce.effective_capabilities != seal.strict_unmount.effective_capabilities
        || seal.quiesce.operation_id == seal.strict_unmount.operation_id
    {
        return Err(PocError::Integrity(
            "external quiesce and strict-unmount receipts do not form one exact mount lifecycle"
                .to_owned(),
        ));
    }
    if seal.quiesce.completed_unix_ms > seal.strict_unmount.completed_unix_ms {
        return Err(PocError::Integrity(
            "external strict-unmount receipt predates quiesce completion".to_owned(),
        ));
    }
    if seal
        .strict_unmount
        .mount_plan_evidence
        .mountinfo_after
        .target
        .is_some()
    {
        return Err(PocError::RecoveryRequired(
            "external strict-unmount receipt does not prove target absence".to_owned(),
        ));
    }
    let mounted_target_matches = |receipt: &StorageAdminReceipt, after: bool| {
        let table = if after {
            &receipt.mount_plan_evidence.mountinfo_after
        } else {
            &receipt.mount_plan_evidence.mountinfo_before
        };
        table
            .target
            .as_ref()
            .is_some_and(|target| target.target == receipt.scope.workspace_root)
    };
    if !mounted_target_matches(&seal.quiesce, false)
        || !mounted_target_matches(&seal.quiesce, true)
        || !mounted_target_matches(&seal.strict_unmount, false)
    {
        return Err(PocError::Integrity(
            "external storage receipts do not prove one mounted target before strict unmount"
                .to_owned(),
        ));
    }
    validate_inventory(
        &seal.first_inventory,
        allocation,
        "first external stable inventory",
    )?;
    validate_inventory(
        &seal.second_inventory,
        allocation,
        "second external stable inventory",
    )?;
    if seal.first_inventory != seal.second_inventory {
        return Err(PocError::RecoveryRequired(format!(
            "allocation {} changed between external stability inventories",
            allocation.descriptor.allocation_id
        )));
    }
    Ok(StableAllocationReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        allocation: allocation.descriptor.clone(),
        expected_owner_epoch: lease.owner_epoch,
        before: seal.first_inventory.physical.clone(),
        after: seal.second_inventory.physical.clone(),
        sync_completed: true,
    })
}

fn validate_storage_receipt(
    receipt: &StorageAdminReceipt,
    expected_action: StorageAdminAction,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    prepared: &PreparedExternalSession,
) -> PocResult<()> {
    let expected_receipt_path = receipt
        .scope
        .control_root
        .join("storage-admin")
        .join(receipt.operation_id.as_str())
        .join("RECEIPT.json");
    if receipt.receipt_path != expected_receipt_path {
        return Err(PocError::Integrity(
            "external storage receipt is outside its canonical operation path".to_owned(),
        ));
    }
    let mut durable_receipt: StorageAdminReceipt = durable::read_json(&expected_receipt_path)?;
    durable_receipt.idempotent_replay = receipt.idempotent_replay;
    if durable_receipt != *receipt {
        return Err(PocError::Integrity(
            "external storage receipt differs from its durable authenticated record".to_owned(),
        ));
    }
    let control_root = prepared
        .session_dir()
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            PocError::Integrity("external session directory has no control root".to_owned())
        })?;
    if receipt.schema_version != SCHEMA_VERSION
        || receipt.interface_version != INTERFACE_VERSION
        || receipt.operation_id.as_str().is_empty()
        || receipt.action != expected_action
        || receipt.outcome != StorageAdminOutcome::Succeeded
        || !receipt.cleanup_complete
        || receipt.failure.is_some()
        || receipt.trusted_executable != Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
        || receipt.allowed_privileged_syscalls
            != STORAGE_ADMIN_PRIVILEGED_SYSCALLS
                .iter()
                .map(|syscall| (*syscall).to_owned())
                .collect::<Vec<_>>()
        || !receipt
            .effective_capabilities
            .iter()
            .any(|capability| capability == "CAP_SYS_ADMIN")
        || receipt.mount_attestation.is_some()
        || receipt.mount_receipt_binding.is_none()
        || receipt.scope.control_root != control_root
        || receipt.scope.session_id != lease.session_id
        || receipt.scope.allocation_id != allocation.descriptor.allocation_id
        || receipt.scope.lease_epoch != lease.lease_epoch
        || receipt.scope.allocation_root != allocation.allocation_root
        || receipt.scope.workspace_root != prepared.workspace_root()
        || receipt.process_evidence.mount_namespace_id != receipt.scope.mount_namespace_id
        || receipt.mount_plan_evidence.mount_namespace_id != receipt.scope.mount_namespace_id
        || receipt.mount_plan_evidence.target != receipt.scope.workspace_root
        || receipt.mount_plan_evidence.lower_dirs_newest_first
            != receipt.scope.lower_dirs_newest_first
        || receipt.mount_plan_evidence.upper_dir != allocation.upper_dir
        || receipt.mount_plan_evidence.work_dir != allocation.work_dir
        || receipt.started_unix_ms == 0
        || receipt.completed_unix_ms < receipt.started_unix_ms
    {
        return Err(PocError::Integrity(format!(
            "external {:?} receipt is not bound to the prepared session",
            expected_action
        )));
    }
    lease::validate_active_storage_admin_lease(
        &allocation.allocation_root,
        &allocation.descriptor.allocation_id,
        &lease.session_id,
        &receipt.scope.lease_id,
        lease.lease_epoch,
    )
}

fn validate_inventory(
    inventory: &AllocationInventory,
    allocation: &AllocationHandle,
    label: &str,
) -> PocResult<()> {
    if inventory.schema_version != SCHEMA_VERSION
        || inventory.allocation_id != allocation.descriptor.allocation_id
        || inventory.allocation_root != allocation.allocation_root
        || inventory.physical.allocation_id != allocation.descriptor.allocation_id
        || inventory.physical.allocation_path != allocation.allocation_root
        || !is_lowercase_sha256(&inventory.inventory_sha256)
    {
        return Err(PocError::Integrity(format!(
            "{label} does not match the permanent allocation"
        )));
    }
    Ok(())
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn build_receipt(
    request: &StationaryPublicationRequest,
    stable: StableAllocationReceipt,
    adoption: AdoptionReceipt,
    seal: ExternalStationarySeal,
    stale_writer_rejected: bool,
    stale_deleter_rejected: bool,
) -> PocResult<ExternalStationaryPublicationReceipt> {
    let representative_inodes_unchanged =
        stable.before.representative_inodes == stable.after.representative_inodes;
    let allocated_bytes_unchanged = stable.before.allocated_bytes == stable.after.allocated_bytes;
    if !representative_inodes_unchanged || !allocated_bytes_unchanged {
        return Err(PocError::Integrity(
            "physical allocation changed across external stationary adoption".to_owned(),
        ));
    }
    if adoption.allocation_id != stable.allocation.allocation_id {
        return Err(PocError::Integrity(
            "external adoption receipt selected a different allocation".to_owned(),
        ));
    }
    Ok(ExternalStationaryPublicationReceipt {
        schema_version: SCHEMA_VERSION,
        request: request.clone(),
        allocation_path_before: stable.before.allocation_path.clone(),
        allocation_path_after: stable.after.allocation_path.clone(),
        stable,
        adoption,
        seal,
        representative_inodes_unchanged,
        allocated_bytes_unchanged,
        no_second_payload_allocation: true,
        stale_writer_rejected,
        stale_deleter_rejected,
        idempotent_replay: false,
    })
}

fn validate_replayed_receipt(
    receipt: &ExternalStationaryPublicationReceipt,
    request: &StationaryPublicationRequest,
    allocation: &AllocationHandle,
    lease: &MutableLease,
) -> PocResult<()> {
    if receipt.schema_version != SCHEMA_VERSION
        || receipt.request != *request
        || receipt.stable.allocation != allocation.descriptor
        || receipt.stable.expected_owner_epoch != lease.owner_epoch
        || receipt.stable.operation_id != request.operation_id
        || receipt.adoption.operation_id != request.operation_id
        || receipt.adoption.publication_id != request.publication_id
        || receipt.adoption.allocation_id != allocation.descriptor.allocation_id
        || !receipt.representative_inodes_unchanged
        || !receipt.allocated_bytes_unchanged
        || !receipt.no_second_payload_allocation
        || !receipt.stale_writer_rejected
        || !receipt.stale_deleter_rejected
        || receipt.idempotent_replay
    {
        return Err(PocError::RecoveryRequired(
            "durable external stationary receipt differs from its replay request".to_owned(),
        ));
    }
    Ok(())
}

fn validate_operation_identity(
    operation_dir: &Path,
    request: &StationaryPublicationRequest,
    allocation_id: &crate::AllocationId,
) -> PocResult<()> {
    let path = operation_dir.join("OPERATION.json");
    if !path.exists() {
        return Ok(());
    }
    let record: PublicationOperationRecord = durable::read_json(&path)?;
    if record.schema_version != SCHEMA_VERSION
        || record.operation_id != request.operation_id
        || record.publication_id != request.publication_id
        || record.allocation_id != *allocation_id
    {
        return Err(PocError::Integrity(
            "external publication operation identity differs from its durable record".to_owned(),
        ));
    }
    Ok(())
}

fn persist_operation(
    operation_dir: &Path,
    request: &StationaryPublicationRequest,
    allocation: &AllocationHandle,
    phase: PublicationPhase,
    stable_inventory_sha256: Option<String>,
) -> PocResult<()> {
    let path = operation_dir.join("OPERATION.json");
    let proposed = PublicationOperationRecord {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        publication_id: request.publication_id.clone(),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        phase,
        stable_inventory_sha256,
    };
    if path.exists() {
        let current: PublicationOperationRecord = durable::read_json(&path)?;
        validate_operation_identity(operation_dir, request, &allocation.descriptor.allocation_id)?;
        if let (Some(current_digest), Some(proposed_digest)) = (
            current.stable_inventory_sha256.as_ref(),
            proposed.stable_inventory_sha256.as_ref(),
        ) {
            if current_digest != proposed_digest {
                return Err(PocError::RecoveryRequired(
                    "external publication stable inventory changed during replay".to_owned(),
                ));
            }
        }
        if current.phase != PublicationPhase::RecoveryRequired
            && publication_phase_rank(current.phase) > publication_phase_rank(proposed.phase)
        {
            return Ok(());
        }
    }
    durable::replace_json(&path, &proposed)
}

fn publication_phase_rank(phase: PublicationPhase) -> u8 {
    match phase {
        PublicationPhase::Prepared => 0,
        PublicationPhase::Sealing => 1,
        PublicationPhase::StableAllocation => 2,
        PublicationPhase::OwnerIntentDurable => 3,
        PublicationPhase::PayloadOwned => 4,
        PublicationPhase::CanonicalDurable => 5,
        PublicationPhase::LocatorDurable => 6,
        PublicationPhase::RefCommitted => 7,
        PublicationPhase::PublicationCommitted => 8,
        PublicationPhase::RecoveryRequired | PublicationPhase::RejectedBeforeAdoption => 9,
    }
}

fn mark_recovery_required(
    prepared: &PreparedExternalSession,
    allocation: &AllocationHandle,
    lease: &MutableLease,
    operation_dir: &Path,
    request: &StationaryPublicationRequest,
    inventory_sha256: Option<&str>,
) {
    let _ = prepared.mark_recovery_required(allocation, lease);
    let _ = persist_operation(
        operation_dir,
        request,
        allocation,
        PublicationPhase::RecoveryRequired,
        inventory_sha256.map(str::to_owned),
    );
}
