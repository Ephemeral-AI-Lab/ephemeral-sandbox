use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fault::{FaultInjector, FaultPoint};
use crate::quiesce::{QuiescenceReceipt, SealedAllocation};
use crate::session::MplaSession;
use crate::{
    durable, lease, owner, AdoptionReceipt, AllocationId, OperationId, OwnerTransitionRequest,
    PocError, PocResult, PublicationId, PublicationPhase, StableAllocationReceipt, SCHEMA_VERSION,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StationaryPublicationRequest {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PublicationOperationRecord {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub allocation_id: AllocationId,
    pub phase: PublicationPhase,
    pub stable_inventory_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StationaryPublicationReceipt {
    pub schema_version: u32,
    pub request: StationaryPublicationRequest,
    pub stable: StableAllocationReceipt,
    pub adoption: AdoptionReceipt,
    pub quiescence: QuiescenceReceipt,
    pub allocation_path_before: PathBuf,
    pub allocation_path_after: PathBuf,
    pub representative_inodes_unchanged: bool,
    pub allocated_bytes_unchanged: bool,
    pub no_second_payload_allocation: bool,
    pub stale_writer_rejected: bool,
    pub stale_deleter_rejected: bool,
}

/// M0 stationary publication: terminally seal the old session, prove a stable
/// permanent upper, then compare-and-adopt only its owner metadata.
///
/// Canonical semantic objects, locators, and the paired ref are deliberately
/// M1 work. This receipt must therefore not be represented as a complete
/// canonical publication.
pub fn stationary_adopt(
    session: &mut MplaSession,
    request: &StationaryPublicationRequest,
    operations_root: &Path,
    faults: &mut FaultInjector,
) -> PocResult<StationaryPublicationReceipt> {
    if request.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(format!(
            "unsupported stationary publication schema {}",
            request.schema_version
        )));
    }
    let operation_dir = operations_root
        .join("publication")
        .join(request.operation_id.as_str());
    std::fs::create_dir_all(&operation_dir).map_err(|error| {
        PocError::io(
            "create publication operation directory",
            &operation_dir,
            error,
        )
    })?;
    persist_operation(
        &operation_dir,
        request,
        session.allocation().descriptor.allocation_id.clone(),
        PublicationPhase::Prepared,
        None,
    )?;

    let sealed = session.seal(&request.operation_id, faults)?;
    let inventory_digest = sealed.first_inventory.inventory_sha256.clone();
    persist_operation(
        &operation_dir,
        request,
        sealed.stable.allocation.allocation_id.clone(),
        PublicationPhase::StableAllocation,
        Some(inventory_digest),
    )?;

    validate_stationary_seal(session, &sealed)?;
    let owner_request = OwnerTransitionRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        publication_id: request.publication_id.clone(),
        session_id: session.session_id().clone(),
        allocation_id: sealed.stable.allocation.allocation_id.clone(),
        expected_lease_epoch: session.mutable_lease().lease_epoch,
        expected_owner_epoch: session.mutable_lease().owner_epoch,
    };
    durable::replace_json(&operation_dir.join("owner-intent.json"), &owner_request)?;
    persist_operation(
        &operation_dir,
        request,
        owner_request.allocation_id.clone(),
        PublicationPhase::OwnerIntentDurable,
        Some(sealed.first_inventory.inventory_sha256.clone()),
    )?;
    faults.hit(FaultPoint::AfterOwnerIntent, true)?;

    let adoption = owner::compare_and_adopt(
        &session.allocation().allocation_root,
        &sealed.stable,
        &owner_request,
    )
    .inspect_err(|_error| {
        let _ = session.mark_recovery_required();
    })?;
    faults.hit(FaultPoint::AfterOwnerAdoption, true)?;
    persist_operation(
        &operation_dir,
        request,
        owner_request.allocation_id.clone(),
        PublicationPhase::PayloadOwned,
        Some(sealed.first_inventory.inventory_sha256.clone()),
    )?;

    let stale_writer_rejected = lease::validate_writer(
        &session.allocation().allocation_root,
        &session.mutable_lease().writer,
    )
    .is_err();
    let stale_deleter_rejected = lease::validate_deleter(
        &session.allocation().allocation_root,
        &session.mutable_lease().deleter,
    )
    .is_err();
    if !stale_writer_rejected || !stale_deleter_rejected {
        let _ = session.mark_recovery_required();
        return Err(PocError::Integrity(
            "adoption did not terminally fence both stale capabilities".to_owned(),
        ));
    }

    let receipt = build_receipt(
        request,
        sealed,
        adoption,
        stale_writer_rejected,
        stale_deleter_rejected,
    )?;
    durable::replace_json(&operation_dir.join("stationary-adoption.json"), &receipt)?;
    session.mark_publication_committed()?;
    persist_operation(
        &operation_dir,
        request,
        owner_request.allocation_id,
        PublicationPhase::PublicationCommitted,
        Some(receipt.quiescence.second_inventory_sha256.clone()),
    )?;
    Ok(receipt)
}

fn persist_operation(
    operation_dir: &Path,
    request: &StationaryPublicationRequest,
    allocation_id: AllocationId,
    phase: PublicationPhase,
    stable_inventory_sha256: Option<String>,
) -> PocResult<()> {
    durable::replace_json(
        &operation_dir.join("OPERATION.json"),
        &PublicationOperationRecord {
            schema_version: SCHEMA_VERSION,
            operation_id: request.operation_id.clone(),
            publication_id: request.publication_id.clone(),
            allocation_id,
            phase,
            stable_inventory_sha256,
        },
    )
}

fn validate_stationary_seal(session: &MplaSession, sealed: &SealedAllocation) -> PocResult<()> {
    let expected = session.allocation();
    if sealed.stable.allocation.allocation_id != expected.descriptor.allocation_id
        || sealed.unmounted.allocation_root != expected.allocation_root
        || sealed.unmounted.allocation_upper != expected.upper_dir
        || sealed.unmounted.allocation_work != expected.work_dir
        || sealed.stable.before.allocation_path != expected.allocation_root
        || sealed.stable.after.allocation_path != expected.allocation_root
    {
        return Err(PocError::Integrity(
            "stationary seal changed allocation identity or physical paths".to_owned(),
        ));
    }
    if sealed.first_inventory != sealed.second_inventory {
        return Err(PocError::RecoveryRequired(
            "stationary seal inventories are not identical".to_owned(),
        ));
    }
    Ok(())
}

fn build_receipt(
    request: &StationaryPublicationRequest,
    sealed: SealedAllocation,
    adoption: AdoptionReceipt,
    stale_writer_rejected: bool,
    stale_deleter_rejected: bool,
) -> PocResult<StationaryPublicationReceipt> {
    let representative_inodes_unchanged =
        sealed.stable.before.representative_inodes == sealed.stable.after.representative_inodes;
    let allocated_bytes_unchanged =
        sealed.stable.before.allocated_bytes == sealed.stable.after.allocated_bytes;
    if !representative_inodes_unchanged || !allocated_bytes_unchanged {
        return Err(PocError::Integrity(
            "physical allocation changed across stationary adoption".to_owned(),
        ));
    }
    if adoption.allocation_id != sealed.stable.allocation.allocation_id {
        return Err(PocError::Integrity(
            "adoption receipt selected a different allocation".to_owned(),
        ));
    }
    Ok(StationaryPublicationReceipt {
        schema_version: SCHEMA_VERSION,
        request: request.clone(),
        stable: sealed.stable,
        adoption,
        quiescence: sealed.quiescence,
        allocation_path_before: sealed.first_inventory.allocation_root,
        allocation_path_after: sealed.second_inventory.allocation_root,
        representative_inodes_unchanged,
        allocated_bytes_unchanged,
        no_second_payload_allocation: true,
        stale_writer_rejected,
        stale_deleter_rejected,
    })
}
