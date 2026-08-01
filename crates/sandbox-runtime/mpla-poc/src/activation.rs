use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, FlockOperation, Timespec, Timestamps, CWD};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
use crate::overlay_adapter::PinnedOverlayLower;
use crate::projection::{select_exact, ExactProjectionReceipt, ProjectionRecipe};
use crate::recovery::reach_real_operation;
use crate::{
    allocation, durable, lease, ActivationOperationId, AllocationDescriptor, AllocationHandle,
    AllocationId, CommandReceipt, MplaSession, NamedFaultInjector, NamedFaultPoint, OperationId,
    OwnerGeneration, PairedRefValue, PocError, PocResult, SessionId, SessionPhase, SessionRecord,
    SCHEMA_VERSION,
};

const ACTIVATION_PLAN_FILE: &str = "PLAN.json";
const FRESH_ACTIVATION_FILE: &str = "FRESH.json";
const ACTIVATION_RECOVERY_INTENT_FILE: &str = "RECOVERY-INTENT.json";
const ACTIVATION_RECOVERY_FILE: &str = "RECOVERY.json";
const ACTIVATION_LOCK_FILE: &str = "LOCK";
const MAX_ACTIVATION_JSON_BYTES: u64 = 1024 * 1024;

struct ActivationOperationLock {
    control_root: OwnedFd,
    directory: OwnedFd,
    _lock: File,
}

struct RecoverySessionAnchors {
    sessions_root: Option<OwnedFd>,
    session: Option<OwnedFd>,
}

struct PinnedAllocation {
    arena: OwnedFd,
    prefix: OwnedFd,
    allocation: OwnedFd,
    upper: OwnedFd,
    work: OwnedFd,
    owner: OwnedFd,
    handle: AllocationHandle,
    supplied: AllocationHandle,
    identity: AllocationPhysicalIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AllocationPhysicalIdentity {
    pub allocation_device: u64,
    pub allocation_inode: u64,
    pub upper_device: u64,
    pub upper_inode: u64,
    pub work_device: u64,
    pub work_inode: u64,
    pub owner_device: u64,
    pub owner_inode: u64,
}

#[derive(Clone, Debug)]
pub struct ExactActivationRequest {
    pub activation_operation_id: ActivationOperationId,
    pub allocation_operation_id: OperationId,
    pub selected_ref: PairedRefValue,
    pub recipe: ProjectionRecipe,
    pub payload_allocations: Vec<AllocationHandle>,
    pub arena_root: PathBuf,
    pub control_root: PathBuf,
    pub cgroup_procs_path: Option<PathBuf>,
    pub readiness_path: PathBuf,
    pub readiness_contains: Option<Vec<u8>>,
    pub readiness_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActivationBinding {
    pub schema_version: u32,
    pub activation_operation_id: ActivationOperationId,
    pub session_id: SessionId,
    pub fresh_allocation_id: AllocationId,
    pub selected_ref: PairedRefValue,
    pub projection: ExactProjectionReceipt,
    pub bound_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActivationReceipt {
    pub schema_version: u32,
    pub activation_operation_id: ActivationOperationId,
    pub session_id: SessionId,
    pub fresh_allocation_id: AllocationId,
    pub selected_payload_allocation_ids: Vec<AllocationId>,
    pub selected_payload_physical_identities: Vec<AllocationPhysicalIdentity>,
    pub projection: ExactProjectionReceipt,
    pub fresh_upper_empty_before_mount: bool,
    pub readiness: CommandReceipt,
    pub phase_spans: Vec<ActivationPhaseSpan>,
    pub elapsed_ns: u64,
    pub session_binding_path: PathBuf,
    pub session_binding_parent_synced: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActivationPhaseSpan {
    pub phase: String,
    pub elapsed_ns: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LocatorPinRecord {
    schema_version: u32,
    activation_operation_id: ActivationOperationId,
    selected_ref_operation_id: OperationId,
    locator_generation: crate::LocatorGeneration,
    selected_payload_allocation_ids: Vec<AllocationId>,
    durable_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ActivationPlanRecord {
    schema_version: u32,
    activation_operation_id: ActivationOperationId,
    allocation_operation_id: OperationId,
    session_id: SessionId,
    selected_ref: PairedRefValue,
    recipe: ProjectionRecipe,
    payload_allocations: Vec<AllocationHandle>,
    payload_physical_identities: Vec<AllocationPhysicalIdentity>,
    arena_root: PathBuf,
    control_root: PathBuf,
    cgroup_procs_path: Option<PathBuf>,
    readiness_path: PathBuf,
    readiness_contains: Option<Vec<u8>>,
    readiness_timeout_ns: u64,
    created_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct FreshActivationRecord {
    schema_version: u32,
    activation_operation_id: ActivationOperationId,
    allocation_operation_id: OperationId,
    session_id: SessionId,
    allocation: AllocationDescriptor,
    durable_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ActivationRecoveryIntent {
    schema_version: u32,
    activation_operation_id: ActivationOperationId,
    allocation_operation_id: OperationId,
    selected_ref: PairedRefValue,
    projection: ExactProjectionReceipt,
    disposition: ActivationRecoveryDisposition,
    fresh_allocation: Option<AllocationDescriptor>,
    session_id: SessionId,
    binding: Option<ActivationBinding>,
    created_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationRecoveryDisposition {
    Old,
    CompleteNew,
}

/// Immutable evidence that one exact activation operation was recovered to a
/// terminal old-or-complete-new state without returning execution authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActivationRecoveryReceipt {
    pub schema_version: u32,
    pub activation_operation_id: ActivationOperationId,
    pub allocation_operation_id: OperationId,
    pub selected_ref: PairedRefValue,
    pub projection: ExactProjectionReceipt,
    pub disposition: ActivationRecoveryDisposition,
    pub fresh_allocation: Option<AllocationDescriptor>,
    pub fresh_owner: Option<OwnerGeneration>,
    pub session_id: SessionId,
    pub binding: Option<ActivationBinding>,
    pub original_outcome: Option<ActivationReceipt>,
    pub terminal_session_record: Option<SessionRecord>,
    pub locator_pin_durable: bool,
    pub allocation_removed: bool,
    pub allocation_retained: bool,
    pub mount_removed: bool,
    pub process_audit_clear: bool,
    pub terminated_process_ids: Vec<i32>,
    pub selected_payload_allocations_before: Vec<AllocationHandle>,
    pub selected_payload_allocations_after: Vec<AllocationHandle>,
    pub selected_payload_physical_identities_before: Vec<AllocationPhysicalIdentity>,
    pub selected_payload_physical_identities_after: Vec<AllocationPhysicalIdentity>,
    pub selected_payload_owners_before: Vec<OwnerGeneration>,
    pub selected_payload_owners_after: Vec<OwnerGeneration>,
    pub selected_payloads_preserved: bool,
    pub terminal_lease_fence: Option<lease::TerminalLeaseFenceWitness>,
    pub authority_fenced: bool,
    pub executable_authority_returned: bool,
    pub recovered_unix_ms: u64,
}

#[derive(Debug)]
pub struct ActivatedSession {
    pub session: MplaSession,
    pub receipt: ActivationReceipt,
}

pub fn activate_exact(request: ExactActivationRequest) -> PocResult<ActivatedSession> {
    let started = Instant::now();
    let selection_started = Instant::now();
    validate_request(&request)?;
    let operation_id = OperationId::from_string(request.activation_operation_id.as_str());
    let activation_directory = activation_directory(&request);
    let operation_lock = lock_activation_operation(&activation_directory)?;
    let activation_anchor = &operation_lock.directory;
    let pinned_payloads = pin_validated_payload_allocations(&request.payload_allocations)?;
    let payload_physical_identities = pinned_payload_identities(&pinned_payloads);
    if read_optional_json_at::<ActivationRecoveryReceipt>(
        activation_anchor,
        &activation_directory.join(ACTIVATION_RECOVERY_FILE),
    )?
    .is_some()
    {
        return Err(PocError::RecoveryRequired(format!(
            "activation {} already has a terminal recovery receipt",
            request.activation_operation_id
        )));
    }
    let (plan, plan_created) = load_or_create_plan(
        &request,
        &payload_physical_identities,
        &activation_directory,
        activation_anchor,
    )?;
    if !plan_created {
        return Err(PocError::RecoveryRequired(format!(
            "activation {} already started and must use terminal recovery",
            request.activation_operation_id
        )));
    }
    let projection = select_exact(&request.recipe)?;
    let locator_pin_path = activation_directory.join("LOCATOR_PIN.json");
    let mut named_faults = NamedFaultInjector::default().with_physical_context(
        operation_id.as_str(),
        [
            activation_directory.join(ACTIVATION_PLAN_FILE),
            activation_directory.join(FRESH_ACTIVATION_FILE),
            locator_pin_path.clone(),
            activation_directory.join("SESSION_BOUND.json"),
            activation_directory.join("OUTCOME.json"),
        ],
    );
    if let [selected_allocation_id] = projection.lower_allocation_ids_newest_first.as_slice() {
        if let Some(selected_payload) = pinned_payloads
            .iter()
            .find(|payload| &payload.supplied.descriptor.allocation_id == selected_allocation_id)
        {
            named_faults = named_faults
                .with_physical_stationary_payload_path(selected_payload.supplied.upper_dir.clone());
        }
    }
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::ActivateAfterRefSelect,
        &operation_id,
        request
            .payload_allocations
            .iter()
            .map(|allocation| allocation.upper_dir.clone()),
        None,
        true,
    )?;
    let payload_by_id: BTreeMap<_, _> = pinned_payloads
        .iter()
        .map(|allocation| {
            (
                allocation.supplied.descriptor.allocation_id.clone(),
                allocation,
            )
        })
        .collect();
    #[cfg(target_os = "linux")]
    let lower_dirs = projection
        .lower_allocation_ids_newest_first
        .iter()
        .map(|allocation_id| {
            let allocation = payload_by_id.get(allocation_id).ok_or_else(|| {
                PocError::Integrity(format!(
                    "projection allocation {allocation_id} has no validated handle"
                ))
            })?;
            PinnedOverlayLower::from_authenticated_descriptor(
                &allocation.supplied.upper_dir,
                &allocation.upper,
            )
        })
        .collect::<PocResult<Vec<_>>>()?;
    #[cfg(not(target_os = "linux"))]
    let lower_dirs = projection
        .lower_allocation_ids_newest_first
        .iter()
        .map(|allocation_id| {
            payload_by_id
                .get(allocation_id)
                .map(|allocation| allocation.supplied.upper_dir.clone())
                .ok_or_else(|| {
                    PocError::Integrity(format!(
                        "projection allocation {allocation_id} has no validated handle"
                    ))
                })
        })
        .collect::<PocResult<Vec<_>>>()?;
    let selected_allocation_id = projection
        .lower_allocation_ids_newest_first
        .first()
        .ok_or_else(|| {
            PocError::Integrity("activation projection selected no payload root".to_owned())
        })?;
    let selected_payload = pinned_payloads
        .iter()
        .find(|payload| &payload.supplied.descriptor.allocation_id == selected_allocation_id)
        .ok_or_else(|| {
            PocError::Integrity(format!(
                "projection allocation {selected_allocation_id} has no pinned handle"
            ))
        })?;
    write_immutable_json_at(
        activation_anchor,
        &locator_pin_path,
        &LocatorPinRecord {
            schema_version: SCHEMA_VERSION,
            activation_operation_id: request.activation_operation_id.clone(),
            selected_ref_operation_id: request.selected_ref.operation_id.clone(),
            locator_generation: request.selected_ref.locator_generation,
            selected_payload_allocation_ids: projection.lower_allocation_ids_newest_first.clone(),
            durable_unix_ms: crate::unix_time_ms()?,
        },
    )?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::ActivateAfterLocatorPin,
        &operation_id,
        [locator_pin_path.clone()],
        None,
        true,
    )?;
    let selection_elapsed_ns = elapsed_ns(selection_started);

    let allocation_started = Instant::now();
    let pinned_fresh =
        load_or_create_fresh(&request, &plan, &activation_directory, activation_anchor)?;
    let fresh = &pinned_fresh.supplied;
    if projection
        .lower_allocation_ids_newest_first
        .contains(&fresh.descriptor.allocation_id)
    {
        return Err(PocError::Integrity(
            "activation fresh allocation aliases selected payload".to_owned(),
        ));
    }
    let pinned_fresh_upper =
        PathBuf::from("/proc/self/fd").join(pinned_fresh.upper.as_raw_fd().to_string());
    let fresh_upper_empty_before_mount = directory_is_empty(&pinned_fresh_upper)?;
    if !fresh_upper_empty_before_mount {
        return Err(PocError::Integrity(format!(
            "activation upper is not empty: {}",
            fresh.upper_dir.display()
        )));
    }
    revalidate_pinned_allocation(selected_payload)?;
    revalidate_pinned_allocation(&pinned_fresh)?;
    inherit_projection_root_metadata_anchored(
        &selected_payload.upper,
        &selected_payload.supplied.upper_dir,
        &pinned_fresh.upper,
        &fresh.upper_dir,
    )?;
    revalidate_pinned_allocation(selected_payload)?;
    revalidate_pinned_allocation(&pinned_fresh)?;
    let allocation_elapsed_ns = elapsed_ns(allocation_started);

    let lease_started = Instant::now();
    let session_id = plan.session_id.clone();
    let mutable_lease = lease::issue_workspace_lease_anchored(
        &pinned_fresh.handle,
        &pinned_fresh.owner,
        session_id.clone(),
        &request.allocation_operation_id,
    )?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::ActivateAfterFreshOwner,
        &operation_id,
        [fresh.owner_dir.join("CURRENT")],
        None,
        true,
    )?;
    let lease_elapsed_ns = elapsed_ns(lease_started);
    let session_started = Instant::now();
    revalidate_pinned_allocation(&pinned_fresh)?;
    let mut session = MplaSession::open_anchored(
        &request.control_root,
        &operation_lock.control_root,
        fresh.clone(),
        mutable_lease.clone(),
        lower_dirs,
        &pinned_fresh.upper,
        &pinned_fresh.work,
        plan.cgroup_procs_path.clone(),
    )?;
    revalidate_pinned_allocation(&pinned_fresh)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::ActivateAfterMount,
        &operation_id,
        [session.session_dir().join("SESSION.json")],
        None,
        true,
    )?;
    let session_elapsed_ns = elapsed_ns(session_started);
    let readiness_started = Instant::now();
    let readiness = session.probe_readiness(
        &mutable_lease.writer,
        &plan.readiness_path,
        plan.readiness_contains.as_deref(),
        Duration::from_nanos(plan.readiness_timeout_ns),
    )?;
    if !readiness.success {
        return Err(PocError::Integrity(
            "external activation readiness probe failed".to_owned(),
        ));
    }
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::ActivateAfterReady,
        &operation_id,
        [session.session_dir().join("SESSION.json")],
        None,
        true,
    )?;
    let readiness_elapsed_ns = elapsed_ns(readiness_started);

    let binding_started = Instant::now();
    let session_binding_path = activation_directory.join("SESSION_BOUND.json");
    let binding = ActivationBinding {
        schema_version: SCHEMA_VERSION,
        activation_operation_id: request.activation_operation_id.clone(),
        session_id: session_id.clone(),
        fresh_allocation_id: session.allocation().descriptor.allocation_id.clone(),
        selected_ref: request.selected_ref.clone(),
        projection: projection.clone(),
        bound_unix_ms: crate::unix_time_ms()?,
    };
    write_immutable_json_at(activation_anchor, &session_binding_path, &binding)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::ActivateAfterBindingFsync,
        &operation_id,
        [session_binding_path.clone()],
        None,
        true,
    )?;
    let binding_elapsed_ns = elapsed_ns(binding_started);
    let elapsed_ns = elapsed_ns(started);
    for payload in &pinned_payloads {
        revalidate_pinned_allocation(payload)?;
    }
    let receipt = ActivationReceipt {
        schema_version: SCHEMA_VERSION,
        activation_operation_id: request.activation_operation_id.clone(),
        session_id,
        fresh_allocation_id: session.allocation().descriptor.allocation_id.clone(),
        selected_payload_allocation_ids: projection.lower_allocation_ids_newest_first.clone(),
        selected_payload_physical_identities: payload_physical_identities,
        projection,
        fresh_upper_empty_before_mount,
        readiness,
        phase_spans: vec![
            phase("validate-select", selection_elapsed_ns),
            phase("durable-allocation", allocation_elapsed_ns),
            phase("durable-lease", lease_elapsed_ns),
            phase("mount-session", session_elapsed_ns),
            phase("readiness", readiness_elapsed_ns),
            phase("durable-binding", binding_elapsed_ns),
            phase("activation-total", elapsed_ns),
        ],
        elapsed_ns,
        session_binding_path: session_binding_path.clone(),
        session_binding_parent_synced: true,
    };
    let outcome_path = activation_directory.join("OUTCOME.json");
    write_immutable_json_at(activation_anchor, &outcome_path, &receipt)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::ResponseLossActivate,
        &operation_id,
        [outcome_path, session_binding_path],
        None,
        true,
    )?;
    Ok(ActivatedSession { session, receipt })
}

/// Recover one previously started exact activation after its runtime process
/// disappeared. The result is terminal evidence only and contains no writer,
/// deleter, mount, or process-execution capability.
pub fn recover_exact_activation(
    request: &ExactActivationRequest,
) -> PocResult<ActivationRecoveryReceipt> {
    validate_request(request)?;
    let activation_directory = activation_directory(request);
    let operation_lock = lock_activation_operation(&activation_directory)?;
    let activation_anchor = &operation_lock.directory;
    let plan_path = activation_directory.join(ACTIVATION_PLAN_FILE);
    let plan = read_optional_json_at::<ActivationPlanRecord>(activation_anchor, &plan_path)?
        .ok_or_else(|| {
            PocError::RecoveryRequired(format!(
                "activation {} has no durable operation plan",
                request.activation_operation_id
            ))
        })?;
    let pinned_payloads = pin_validated_payload_allocations(&request.payload_allocations)?;
    let selected_payload_physical_identities_before = pinned_payload_identities(&pinned_payloads);
    validate_plan(&plan, request, &selected_payload_physical_identities_before)?;
    let projection = select_exact(&request.recipe)?;
    let recovery_path = activation_directory.join(ACTIVATION_RECOVERY_FILE);
    if let Some(receipt) =
        read_optional_json_at::<ActivationRecoveryReceipt>(activation_anchor, &recovery_path)?
    {
        validate_recovery_receipt(&receipt, request, &plan, &projection)?;
        reaudit_activation_recovery(&receipt, request, &plan, &projection, &operation_lock)?;
        return Ok(receipt);
    }

    let selected_payload_allocations_before = pinned_payload_handles(&pinned_payloads);
    let selected_payload_owners_before = selected_pinned_payload_owners(&pinned_payloads)?;

    let locator_pin_path = activation_directory.join("LOCATOR_PIN.json");
    let locator_pin =
        read_optional_json_at::<LocatorPinRecord>(activation_anchor, &locator_pin_path)?;
    if let Some(locator_pin) = locator_pin.as_ref() {
        validate_locator_pin(locator_pin, request, &projection)?;
    }
    let binding_path = activation_directory.join("SESSION_BOUND.json");
    let binding = read_optional_json_at::<ActivationBinding>(activation_anchor, &binding_path)?;
    let outcome_path = activation_directory.join("OUTCOME.json");
    let original_outcome =
        read_optional_json_at::<ActivationReceipt>(activation_anchor, &outcome_path)?;
    let (fresh, recorded_fresh_descriptor) =
        load_fresh_for_recovery(request, &plan, &activation_directory, activation_anchor)?;
    let fresh_descriptor = fresh
        .as_ref()
        .map(|allocation| allocation.supplied.descriptor.clone())
        .or(recorded_fresh_descriptor);

    if let Some(binding) = binding.as_ref() {
        validate_activation_binding(
            binding,
            request,
            &plan,
            &projection,
            fresh_descriptor.as_ref(),
        )?;
    }
    if let Some(outcome) = original_outcome.as_ref() {
        validate_activation_outcome(outcome, request, &plan, &projection, binding.as_ref())?;
    }
    if original_outcome.is_some() && binding.is_none() {
        return Err(PocError::RecoveryRequired(
            "activation outcome exists without its ratifying binding".to_owned(),
        ));
    }
    if (binding.is_some() || original_outcome.is_some()) && locator_pin.is_none() {
        return Err(PocError::RecoveryRequired(
            "committed activation is missing its durable locator pin".to_owned(),
        ));
    }

    let disposition = if binding.is_some() {
        ActivationRecoveryDisposition::CompleteNew
    } else {
        ActivationRecoveryDisposition::Old
    };
    let recovery_intent_path = activation_directory.join(ACTIVATION_RECOVERY_INTENT_FILE);
    let existing_recovery_intent = read_optional_json_at::<ActivationRecoveryIntent>(
        activation_anchor,
        &recovery_intent_path,
    )?;
    if fresh.is_none() && fresh_descriptor.is_some() && existing_recovery_intent.is_none() {
        return Err(PocError::RecoveryRequired(
            "activation fresh allocation disappeared before cleanup was ratified".to_owned(),
        ));
    }
    load_or_create_recovery_intent(
        request,
        &plan,
        &projection,
        disposition,
        fresh_descriptor.as_ref(),
        binding.as_ref(),
        activation_anchor,
        &recovery_intent_path,
        existing_recovery_intent,
    )?;

    let mut fresh_owner = None;
    let mut terminal_session_record = None;
    let mut terminated_process_ids = Vec::new();
    let mut mount_removed = true;
    let mut process_audit_clear = true;
    let mut allocation_removed = false;
    let mut allocation_retained = false;
    let mut terminal_lease_fence = None;
    let mut authority_fenced = false;

    let sessions_root = request.control_root.join("sessions");
    let session_dir = sessions_root.join(plan.session_id.as_str());
    let session_anchors =
        open_recovery_session_anchors(&operation_lock.control_root, &session_dir)?;
    let session_record_path = session_dir.join("SESSION.json");
    let durable_session_record = match session_anchors.session.as_ref() {
        Some(session_anchor) => {
            read_optional_json_at::<SessionRecord>(session_anchor, &session_record_path)?
        }
        None => None,
    };
    if let Some(pinned_fresh) = fresh.as_ref() {
        let fresh = &pinned_fresh.supplied;
        let fence_operation_id = OperationId::from_string(request.activation_operation_id.as_str());
        if disposition == ActivationRecoveryDisposition::Old {
            // Old recovery must never call issue_workspace_lease: doing so can
            // install CURRENT and reconstruct live writer/deleter capability
            // objects after restart.  Inspect only the already-selected owner,
            // validate any durable session against it, and fence the exact
            // private lease tuple without returning either nonce.
            let _owner_lock = lock_pinned_allocation_owner(pinned_fresh, fresh)?;
            let selected_owner = crate::owner::selected_owner_locked_anchored(
                &pinned_fresh.handle.allocation_root,
                &pinned_fresh.owner,
            )?;
            validate_private_activation_owner(selected_owner.as_ref(), fresh, &plan)?;
            if let Some(session_record) = durable_session_record.as_ref() {
                validate_activation_session(
                    session_record,
                    fresh,
                    selected_owner.as_ref(),
                    &session_dir,
                    &plan,
                )?;
            }
            let fence = lease::fence_or_reaudit_private_activation_anchored_locked(
                &pinned_fresh.handle,
                &pinned_fresh.owner,
                &plan.session_id,
                &plan.allocation_operation_id,
                &fence_operation_id,
            )?;
            if let Some(session_record) = durable_session_record.as_ref() {
                let witness = fence.as_ref().ok_or_else(|| {
                    PocError::RecoveryRequired(
                        "activation session exists without a durable workspace lease".to_owned(),
                    )
                })?;
                validate_terminal_lease_fence(
                    witness,
                    request,
                    &plan,
                    &fresh.descriptor,
                    session_record,
                )?;
            }
            authority_fenced = fence
                .as_ref()
                .is_some_and(|witness| witness.writer_revoked && witness.deleter_revoked);
            terminal_lease_fence = fence;
            fresh_owner = selected_owner;
        }
        if let Some(session_record) = durable_session_record.clone() {
            let session_anchor = session_anchors.session.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "activation session disappeared after its record was anchored".to_owned(),
                )
            })?;
            if disposition == ActivationRecoveryDisposition::CompleteNew {
                validate_removed_activation_session(
                    &session_record,
                    &fresh.descriptor,
                    &plan,
                    &session_dir,
                )?;
            }
            let mount_path = session_dir.join("MOUNT.json");
            let attestation = read_optional_json_at::<
                crate::overlay_adapter::OverlayMountAttestation,
            >(session_anchor, &mount_path)?
            .ok_or_else(|| {
                PocError::RecoveryRequired(
                    "activation session has no durable mount attestation".to_owned(),
                )
            })?;
            match disposition {
                ActivationRecoveryDisposition::Old => {
                    validate_activation_mount(&attestation, fresh, &session_record, &plan)?
                }
                ActivationRecoveryDisposition::CompleteNew => {
                    validate_removed_activation_mount(
                        &attestation,
                        &fresh.descriptor,
                        &plan,
                        &session_record,
                    )?;
                }
            }
            if disposition == ActivationRecoveryDisposition::CompleteNew {
                let _owner_lock = lock_pinned_allocation_owner(pinned_fresh, fresh)?;
                let selected_owner = crate::owner::selected_owner_locked_anchored(
                    &pinned_fresh.handle.allocation_root,
                    &pinned_fresh.owner,
                )?;
                validate_private_activation_owner(selected_owner.as_ref(), fresh, &plan)?;
                validate_activation_session(
                    &session_record,
                    fresh,
                    selected_owner.as_ref(),
                    &session_dir,
                    &plan,
                )?;
                let fence = lease::fence_or_reaudit_terminal_session_anchored_locked(
                    &pinned_fresh.handle,
                    &pinned_fresh.owner,
                    &session_record.session_id,
                    session_record.lease_epoch,
                    session_record.owner_epoch,
                    &plan.allocation_operation_id,
                    &fence_operation_id,
                )?;
                fresh_owner = selected_owner;
                authority_fenced = fence.writer_revoked && fence.deleter_revoked;
                terminal_lease_fence = Some(fence);
            }
            let (terminated, audit) = drain_and_unmount_activation_session(
                &attestation,
                &session_record,
                &plan,
                session_anchor,
            )?;
            terminated_process_ids = terminated;
            mount_removed = audit.mount_namespace_pins.is_empty();
            process_audit_clear = audit.is_clear();
            if !process_audit_clear {
                return Err(PocError::RecoveryRequired(format!(
                    "activation terminal process audit is not clear: {audit:?}"
                )));
            }
            if disposition == ActivationRecoveryDisposition::CompleteNew {
                let mut terminal = session_record;
                if terminal.phase != SessionPhase::RecoveryRequired {
                    terminal.phase = SessionPhase::RecoveryRequired;
                    terminal.updated_unix_ms = crate::unix_time_ms()?;
                    replace_json_at(session_anchor, &session_record_path, &terminal)?;
                }
                terminal_session_record = Some(terminal);
            }
        } else if binding.is_some() {
            return Err(PocError::RecoveryRequired(
                "committed activation is missing its exact session record".to_owned(),
            ));
        } else if let Some(session_anchor) = session_anchors.session.as_ref() {
            validate_unrecorded_private_session(&session_dir, session_anchor)?;
        }

        match disposition {
            ActivationRecoveryDisposition::Old => {
                remove_private_activation_allocation(
                    request,
                    &plan,
                    fresh,
                    pinned_fresh,
                    terminal_lease_fence.as_ref(),
                )?;
                remove_private_session_directory(
                    &operation_lock.control_root,
                    &session_dir,
                    &session_anchors,
                )?;
                allocation_removed = true;
            }
            ActivationRecoveryDisposition::CompleteNew => {
                if terminal_session_record.is_none() || terminal_lease_fence.is_none() {
                    return Err(PocError::RecoveryRequired(
                        "committed activation lacks a terminal fenced lease/session tuple"
                            .to_owned(),
                    ));
                }
                revalidate_pinned_allocation(pinned_fresh)?;
                allocation_retained = true;
            }
        }
    } else {
        if binding.is_some() {
            return Err(PocError::RecoveryRequired(
                "committed activation is missing its fresh allocation".to_owned(),
            ));
        }
        if let Some(session_record) = durable_session_record {
            let session_anchor = session_anchors.session.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "activation session disappeared after its record was anchored".to_owned(),
                )
            })?;
            let descriptor = fresh_descriptor.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "activation session exists without a recorded fresh allocation".to_owned(),
                )
            })?;
            validate_removed_activation_session(&session_record, descriptor, &plan, &session_dir)?;
            let mount_path = session_dir.join("MOUNT.json");
            let attestation = read_optional_json_at::<
                crate::overlay_adapter::OverlayMountAttestation,
            >(session_anchor, &mount_path)?
            .ok_or_else(|| {
                PocError::RecoveryRequired(
                    "activation session has no durable mount attestation".to_owned(),
                )
            })?;
            validate_removed_activation_mount(&attestation, descriptor, &plan, &session_record)?;
            let (terminated, audit) = drain_and_unmount_activation_session(
                &attestation,
                &session_record,
                &plan,
                session_anchor,
            )?;
            terminated_process_ids = terminated;
            mount_removed = audit.mount_namespace_pins.is_empty();
            process_audit_clear = audit.is_clear();
        } else if let Some(session_anchor) = session_anchors.session.as_ref() {
            validate_unrecorded_private_session(&session_dir, session_anchor)?;
        }
        remove_private_session_directory(
            &operation_lock.control_root,
            &session_dir,
            &session_anchors,
        )?;
        allocation_removed = fresh_descriptor.is_some();
    }

    for payload in &pinned_payloads {
        revalidate_pinned_allocation(payload)?;
    }
    let selected_payload_allocations_after = pinned_payload_handles(&pinned_payloads);
    let selected_payload_physical_identities_after = pinned_payload_identities(&pinned_payloads);
    let selected_payload_owners_after = selected_pinned_payload_owners(&pinned_payloads)?;
    let receipt = ActivationRecoveryReceipt {
        schema_version: SCHEMA_VERSION,
        activation_operation_id: request.activation_operation_id.clone(),
        allocation_operation_id: request.allocation_operation_id.clone(),
        selected_ref: request.selected_ref.clone(),
        projection,
        disposition,
        fresh_allocation: fresh_descriptor,
        fresh_owner,
        session_id: plan.session_id.clone(),
        binding,
        original_outcome,
        terminal_session_record,
        locator_pin_durable: locator_pin.is_some(),
        allocation_removed,
        allocation_retained,
        mount_removed,
        process_audit_clear,
        terminated_process_ids,
        selected_payloads_preserved: selected_payload_allocations_before
            == selected_payload_allocations_after
            && selected_payload_physical_identities_before
                == selected_payload_physical_identities_after
            && selected_payload_owners_before == selected_payload_owners_after,
        selected_payload_allocations_before,
        selected_payload_allocations_after,
        selected_payload_physical_identities_before,
        selected_payload_physical_identities_after,
        selected_payload_owners_before: selected_payload_owners_before.clone(),
        selected_payload_owners_after: selected_payload_owners_after.clone(),
        terminal_lease_fence,
        authority_fenced,
        executable_authority_returned: false,
        recovered_unix_ms: crate::unix_time_ms()?,
    };
    validate_recovery_receipt(&receipt, request, &plan, &receipt.projection)?;
    write_immutable_json_at(activation_anchor, &recovery_path, &receipt)?;
    Ok(receipt)
}

fn activation_directory(request: &ExactActivationRequest) -> PathBuf {
    request
        .control_root
        .join("activations")
        .join(request.activation_operation_id.as_str())
}

fn lock_activation_operation(activation_directory: &Path) -> PocResult<ActivationOperationLock> {
    let activation_root = activation_directory.parent().ok_or_else(|| {
        PocError::Integrity("activation operation directory has no parent".to_owned())
    })?;
    let control_root = activation_root.parent().ok_or_else(|| {
        PocError::Integrity("activation control directory has no parent".to_owned())
    })?;
    let activation_name = activation_directory.file_name().ok_or_else(|| {
        PocError::Integrity("activation operation directory has no name".to_owned())
    })?;
    require_single_component_os("activation operation directory", activation_name)?;
    let control_anchor =
        open_activation_directory_no_symlink("activation control root", control_root)?;
    create_activation_child_directory(
        &control_anchor,
        OsStr::new("activations"),
        activation_root,
        "activation operation root",
    )?;
    let activation_root_anchor = open_activation_child_directory_no_symlink(
        "activation operation root",
        &control_anchor,
        OsStr::new("activations"),
    )?;
    create_activation_child_directory(
        &activation_root_anchor,
        activation_name,
        activation_directory,
        "activation operation directory",
    )?;
    let activation_anchor = open_activation_child_directory_no_symlink(
        "activation operation directory",
        &activation_root_anchor,
        activation_name,
    )?;
    let lock_path = activation_directory.join(ACTIVATION_LOCK_FILE);
    let (lock_fd, created) = match rustix::fs::openat(
        &activation_anchor,
        OsStr::new(ACTIVATION_LOCK_FILE),
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    ) {
        Ok(lock) => (lock, true),
        Err(rustix::io::Errno::EXIST) => (
            rustix::fs::openat(
                &activation_anchor,
                OsStr::new(ACTIVATION_LOCK_FILE),
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|error| {
                PocError::io(
                    "open anchored activation lock",
                    &lock_path,
                    std::io::Error::from(error),
                )
            })?,
            false,
        ),
        Err(error) => {
            return Err(PocError::io(
                "create anchored activation lock",
                &lock_path,
                std::io::Error::from(error),
            ));
        }
    };
    let lock_metadata = rustix::fs::fstat(&lock_fd).map_err(|error| {
        PocError::io(
            "stat anchored activation lock",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    if rustix::fs::FileType::from_raw_mode(lock_metadata.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::RegularFile
    {
        return Err(PocError::RecoveryRequired(format!(
            "activation operation lock is not a real regular file: {}",
            lock_path.display()
        )));
    }
    let lock = File::from(lock_fd);
    if created {
        durable::sync_all(&lock)
            .map_err(|source| PocError::io("sync activation lock", &lock_path, source))?;
        fsync_activation_anchor(&activation_anchor, activation_directory)?;
    }
    rustix::fs::flock(&lock, FlockOperation::LockExclusive).map_err(|error| {
        PocError::io(
            "lock anchored activation operation",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    let installed_lock = rustix::fs::statat(
        &activation_anchor,
        OsStr::new(ACTIVATION_LOCK_FILE),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|error| {
        PocError::io(
            "revalidate anchored activation lock",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    if raw_mode_file_type(installed_lock.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::RegularFile
        || installed_lock.st_dev != lock_metadata.st_dev
        || installed_lock.st_ino != lock_metadata.st_ino
    {
        return Err(PocError::RecoveryRequired(format!(
            "activation operation lock changed while it was acquired: {}",
            lock_path.display()
        )));
    }
    Ok(ActivationOperationLock {
        control_root: control_anchor,
        directory: activation_anchor,
        _lock: lock,
    })
}

fn create_activation_child_directory(
    parent: &OwnedFd,
    child: &OsStr,
    display_path: &Path,
    label: &str,
) -> PocResult<()> {
    require_single_component_os(label, child)?;
    match rustix::fs::mkdirat(
        parent,
        child,
        rustix::fs::Mode::RUSR
            | rustix::fs::Mode::WUSR
            | rustix::fs::Mode::XUSR
            | rustix::fs::Mode::RGRP
            | rustix::fs::Mode::XGRP
            | rustix::fs::Mode::ROTH
            | rustix::fs::Mode::XOTH,
    ) {
        Ok(()) => fsync_activation_anchor(parent, display_path.parent().unwrap_or(display_path)),
        Err(rustix::io::Errno::EXIST) => {
            open_activation_child_directory_no_symlink(label, parent, child).map(drop)
        }
        Err(error) => Err(PocError::io(
            "create anchored activation directory",
            display_path,
            std::io::Error::from(error),
        )),
    }
}

fn fsync_activation_anchor(anchor: &OwnedFd, display_path: &Path) -> PocResult<()> {
    rustix::fs::fsync(anchor).map_err(|error| {
        PocError::io(
            "fsync anchored activation directory",
            display_path,
            std::io::Error::from(error),
        )
    })
}

fn load_or_create_plan(
    request: &ExactActivationRequest,
    payload_physical_identities: &[AllocationPhysicalIdentity],
    activation_directory: &Path,
    activation_anchor: &OwnedFd,
) -> PocResult<(ActivationPlanRecord, bool)> {
    let plan_path = activation_directory.join(ACTIVATION_PLAN_FILE);
    if let Some(plan) =
        read_optional_json_at::<ActivationPlanRecord>(activation_anchor, &plan_path)?
    {
        validate_plan(&plan, request, payload_physical_identities)?;
        return Ok((plan, false));
    }
    let plan = ActivationPlanRecord {
        schema_version: SCHEMA_VERSION,
        activation_operation_id: request.activation_operation_id.clone(),
        allocation_operation_id: request.allocation_operation_id.clone(),
        session_id: SessionId::new(),
        selected_ref: request.selected_ref.clone(),
        recipe: request.recipe.clone(),
        payload_allocations: request.payload_allocations.clone(),
        payload_physical_identities: payload_physical_identities.to_vec(),
        arena_root: request.arena_root.clone(),
        control_root: request.control_root.clone(),
        cgroup_procs_path: request.cgroup_procs_path.clone(),
        readiness_path: request.readiness_path.clone(),
        readiness_contains: request.readiness_contains.clone(),
        readiness_timeout_ns: duration_ns(request.readiness_timeout)?,
        created_unix_ms: crate::unix_time_ms()?,
    };
    write_immutable_json_at(activation_anchor, &plan_path, &plan)?;
    Ok((plan, true))
}

fn validate_plan(
    plan: &ActivationPlanRecord,
    request: &ExactActivationRequest,
    payload_physical_identities: &[AllocationPhysicalIdentity],
) -> PocResult<()> {
    if plan.schema_version != SCHEMA_VERSION
        || plan.activation_operation_id != request.activation_operation_id
        || plan.allocation_operation_id != request.allocation_operation_id
        || plan.selected_ref != request.selected_ref
        || plan.recipe != request.recipe
        || plan.payload_allocations != request.payload_allocations
        || plan.payload_physical_identities != payload_physical_identities
        || plan.arena_root != request.arena_root
        || plan.control_root != request.control_root
        || plan.cgroup_procs_path != request.cgroup_procs_path
        || plan.readiness_path != request.readiness_path
        || plan.readiness_contains != request.readiness_contains
        || plan.readiness_timeout_ns != duration_ns(request.readiness_timeout)?
    {
        return Err(PocError::RecoveryRequired(format!(
            "activation {} request differs from its immutable plan",
            request.activation_operation_id
        )));
    }
    validate_identifier_component(plan.session_id.as_str(), "activation session ID")
}

fn load_or_create_fresh(
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    activation_directory: &Path,
    activation_anchor: &OwnedFd,
) -> PocResult<PinnedAllocation> {
    let record_path = activation_directory.join(FRESH_ACTIVATION_FILE);
    if let Some(record) =
        read_optional_json_at::<FreshActivationRecord>(activation_anchor, &record_path)?
    {
        validate_fresh_record(&record, request, plan)?;
        let allocations = discover_fresh_allocations(request)?;
        if allocations.len() != 1 || allocations[0].supplied.descriptor != record.allocation {
            return Err(PocError::RecoveryRequired(
                "durable activation fresh record does not name the unique operation allocation"
                    .to_owned(),
            ));
        }
        return allocations.into_iter().next().ok_or_else(|| {
            PocError::RecoveryRequired("activation fresh allocation disappeared".to_owned())
        });
    }

    let allocations = discover_fresh_allocations(request)?;
    let fresh = match allocations.len() {
        0 => {
            let created = allocation::create_allocation(
                &request.arena_root,
                &request.allocation_operation_id,
            )?;
            pin_private_activation_allocation(request, &created)?
        }
        1 => allocations.into_iter().next().ok_or_else(|| {
            PocError::RecoveryRequired("activation fresh allocation disappeared".to_owned())
        })?,
        count => {
            return Err(PocError::RecoveryRequired(format!(
                "activation allocation operation owns {count} allocations"
            )));
        }
    };
    let record = FreshActivationRecord {
        schema_version: SCHEMA_VERSION,
        activation_operation_id: request.activation_operation_id.clone(),
        allocation_operation_id: request.allocation_operation_id.clone(),
        session_id: plan.session_id.clone(),
        allocation: fresh.supplied.descriptor.clone(),
        durable_unix_ms: crate::unix_time_ms()?,
    };
    write_immutable_json_at(activation_anchor, &record_path, &record)?;
    Ok(fresh)
}

fn load_fresh_for_recovery(
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    activation_directory: &Path,
    activation_anchor: &OwnedFd,
) -> PocResult<(Option<PinnedAllocation>, Option<AllocationDescriptor>)> {
    let record_path = activation_directory.join(FRESH_ACTIVATION_FILE);
    let record = read_optional_json_at::<FreshActivationRecord>(activation_anchor, &record_path)?;
    if let Some(record) = record.as_ref() {
        validate_fresh_record(record, request, plan)?;
    }
    let allocations = discover_fresh_allocations(request)?;
    if allocations.len() > 1 {
        return Err(PocError::RecoveryRequired(format!(
            "activation allocation operation owns {} allocations",
            allocations.len()
        )));
    }
    let fresh = allocations.into_iter().next();
    match (record, fresh) {
        (Some(record), Some(fresh)) => {
            if record.allocation != fresh.supplied.descriptor {
                return Err(PocError::RecoveryRequired(
                    "activation fresh allocation differs from its durable record".to_owned(),
                ));
            }
            Ok((Some(fresh), Some(record.allocation)))
        }
        (Some(record), None) => Ok((None, Some(record.allocation))),
        (None, Some(fresh)) => {
            let record = FreshActivationRecord {
                schema_version: SCHEMA_VERSION,
                activation_operation_id: request.activation_operation_id.clone(),
                allocation_operation_id: request.allocation_operation_id.clone(),
                session_id: plan.session_id.clone(),
                allocation: fresh.supplied.descriptor.clone(),
                durable_unix_ms: crate::unix_time_ms()?,
            };
            write_immutable_json_at(activation_anchor, &record_path, &record)?;
            Ok((Some(fresh), Some(record.allocation)))
        }
        (None, None) => Ok((None, None)),
    }
}

fn validate_fresh_record(
    record: &FreshActivationRecord,
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
) -> PocResult<()> {
    if record.schema_version != SCHEMA_VERSION
        || record.activation_operation_id != request.activation_operation_id
        || record.allocation_operation_id != request.allocation_operation_id
        || record.session_id != plan.session_id
        || record.allocation.schema_version != SCHEMA_VERSION
        || record.allocation.created_by_operation != request.allocation_operation_id
    {
        return Err(PocError::RecoveryRequired(
            "activation fresh allocation record has a mismatched scope".to_owned(),
        ));
    }
    Ok(())
}

fn discover_fresh_allocations(
    request: &ExactActivationRequest,
) -> PocResult<Vec<PinnedAllocation>> {
    discover_pinned_fresh_allocations(request)
}

#[cfg(target_os = "linux")]
fn discover_pinned_fresh_allocations(
    request: &ExactActivationRequest,
) -> PocResult<Vec<PinnedAllocation>> {
    if !real_directory_exists(&request.arena_root, "allocation arena")? {
        return Ok(Vec::new());
    }
    let arena =
        open_activation_directory_no_symlink("activation allocation arena", &request.arena_root)?;
    let mut matching = Vec::new();
    for (prefix_name, prefix_device, prefix_inode, prefix_mode) in
        anchored_directory_names(&arena, &request.arena_root, "activation allocation arena")?
    {
        let prefix_path = request.arena_root.join(&prefix_name);
        let prefix = open_activation_child_directory_no_symlink(
            "activation allocation prefix",
            &arena,
            &prefix_name,
        )?;
        require_stat_matches_fd(
            prefix_device,
            prefix_inode,
            prefix_mode,
            &prefix,
            &prefix_path,
        )?;
        for (allocation_name, allocation_device, allocation_inode, allocation_mode) in
            anchored_directory_names(&prefix, &prefix_path, "activation allocation prefix")?
        {
            let allocation_root = prefix_path.join(&allocation_name);
            let allocation = open_activation_child_directory_no_symlink(
                "activation allocation",
                &prefix,
                &allocation_name,
            )?;
            require_stat_matches_fd(
                allocation_device,
                allocation_inode,
                allocation_mode,
                &allocation,
                &allocation_root,
            )?;
            let descriptor_path = allocation_root.join("ALLOCATION.json");
            let descriptor =
                read_optional_json_at::<AllocationDescriptor>(&allocation, &descriptor_path)?
                    .ok_or_else(|| {
                        PocError::RecoveryRequired(format!(
                            "activation allocation has no descriptor: {}",
                            allocation_root.display()
                        ))
                    })?;
            if descriptor.created_by_operation != request.allocation_operation_id {
                continue;
            }
            if descriptor.schema_version != SCHEMA_VERSION
                || allocation_root_for(&request.arena_root, &descriptor.allocation_id)?
                    != allocation_root
                || allocation_name.as_os_str() != OsStr::new(descriptor.allocation_id.as_str())
            {
                return Err(PocError::RecoveryRequired(
                    "activation operation allocation is not at its canonical path".to_owned(),
                ));
            }
            let supplied = AllocationHandle {
                descriptor,
                upper_dir: allocation_root.join("upper"),
                work_dir: allocation_root.join("work"),
                owner_dir: allocation_root.join("owner"),
                allocation_root,
            };
            let pinned_prefix = rustix::io::dup(&prefix).map_err(|error| {
                PocError::io(
                    "duplicate pinned activation allocation prefix",
                    &prefix_path,
                    std::io::Error::from(error),
                )
            })?;
            let pinned_arena = rustix::io::dup(&arena).map_err(|error| {
                PocError::io(
                    "duplicate pinned activation allocation arena",
                    &request.arena_root,
                    std::io::Error::from(error),
                )
            })?;
            matching.push(finish_pinned_allocation(
                pinned_arena,
                pinned_prefix,
                allocation,
                &supplied,
            )?);
        }
    }
    matching.sort_by(|left, right| {
        left.supplied
            .descriptor
            .allocation_id
            .cmp(&right.supplied.descriptor.allocation_id)
    });
    Ok(matching)
}

#[cfg(not(target_os = "linux"))]
fn discover_pinned_fresh_allocations(
    _request: &ExactActivationRequest,
) -> PocResult<Vec<PinnedAllocation>> {
    Err(PocError::Unsupported(
        "descriptor-anchored activation allocation discovery requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn anchored_directory_names(
    directory: &OwnedFd,
    display_path: &Path,
    label: &str,
) -> PocResult<Vec<(OsString, u64, u64, rustix::fs::RawMode)>> {
    let reader = rustix::fs::Dir::read_from(directory.as_fd()).map_err(|error| {
        PocError::io(
            "read anchored activation directory",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let mut names = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|error| {
            PocError::io(
                "read anchored activation directory entry",
                display_path,
                std::io::Error::from(error),
            )
        })?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = OsString::from_vec(bytes.to_vec());
        let entry_path = display_path.join(&name);
        let metadata =
            rustix::fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
                PocError::io(
                    "stat anchored activation directory entry",
                    &entry_path,
                    std::io::Error::from(error),
                )
            })?;
        if raw_mode_file_type(metadata.st_mode as rustix::fs::RawMode)
            != rustix::fs::FileType::Directory
        {
            return Err(PocError::RecoveryRequired(format!(
                "{label} contains a non-directory entry: {}",
                entry_path.display()
            )));
        }
        names.push((
            name,
            metadata.st_dev as u64,
            metadata.st_ino as u64,
            metadata.st_mode as rustix::fs::RawMode,
        ));
    }
    names.sort();
    Ok(names)
}

#[allow(clippy::too_many_arguments)]
fn load_or_create_recovery_intent(
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    projection: &ExactProjectionReceipt,
    disposition: ActivationRecoveryDisposition,
    fresh_allocation: Option<&AllocationDescriptor>,
    binding: Option<&ActivationBinding>,
    activation_anchor: &OwnedFd,
    intent_path: &Path,
    existing: Option<ActivationRecoveryIntent>,
) -> PocResult<ActivationRecoveryIntent> {
    if let Some(intent) = existing {
        validate_recovery_intent(
            &intent,
            request,
            plan,
            projection,
            disposition,
            fresh_allocation,
            binding,
        )?;
        return Ok(intent);
    }
    let intent = ActivationRecoveryIntent {
        schema_version: SCHEMA_VERSION,
        activation_operation_id: request.activation_operation_id.clone(),
        allocation_operation_id: request.allocation_operation_id.clone(),
        selected_ref: request.selected_ref.clone(),
        projection: projection.clone(),
        disposition,
        fresh_allocation: fresh_allocation.cloned(),
        session_id: plan.session_id.clone(),
        binding: binding.cloned(),
        created_unix_ms: crate::unix_time_ms()?,
    };
    write_immutable_json_at(activation_anchor, intent_path, &intent)?;
    Ok(intent)
}

#[allow(clippy::too_many_arguments)]
fn validate_recovery_intent(
    intent: &ActivationRecoveryIntent,
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    projection: &ExactProjectionReceipt,
    disposition: ActivationRecoveryDisposition,
    fresh_allocation: Option<&AllocationDescriptor>,
    binding: Option<&ActivationBinding>,
) -> PocResult<()> {
    if intent.schema_version != SCHEMA_VERSION
        || intent.activation_operation_id != request.activation_operation_id
        || intent.allocation_operation_id != request.allocation_operation_id
        || intent.selected_ref != request.selected_ref
        || intent.projection != *projection
        || intent.disposition != disposition
        || intent.fresh_allocation.as_ref() != fresh_allocation
        || intent.session_id != plan.session_id
        || intent.binding.as_ref() != binding
    {
        return Err(PocError::RecoveryRequired(
            "activation recovery state differs from its immutable intent".to_owned(),
        ));
    }
    Ok(())
}

fn validate_locator_pin(
    pin: &LocatorPinRecord,
    request: &ExactActivationRequest,
    projection: &ExactProjectionReceipt,
) -> PocResult<()> {
    if pin.schema_version != SCHEMA_VERSION
        || pin.activation_operation_id != request.activation_operation_id
        || pin.selected_ref_operation_id != request.selected_ref.operation_id
        || pin.locator_generation != request.selected_ref.locator_generation
        || pin.selected_payload_allocation_ids != projection.lower_allocation_ids_newest_first
    {
        return Err(PocError::RecoveryRequired(
            "activation locator pin differs from the exact selected ref".to_owned(),
        ));
    }
    Ok(())
}

fn validate_activation_binding(
    binding: &ActivationBinding,
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    projection: &ExactProjectionReceipt,
    fresh: Option<&AllocationDescriptor>,
) -> PocResult<()> {
    if binding.schema_version != SCHEMA_VERSION
        || binding.activation_operation_id != request.activation_operation_id
        || binding.session_id != plan.session_id
        || binding.selected_ref != request.selected_ref
        || binding.projection != *projection
        || fresh.is_none_or(|descriptor| binding.fresh_allocation_id != descriptor.allocation_id)
    {
        return Err(PocError::RecoveryRequired(
            "activation binding differs from the immutable operation graph".to_owned(),
        ));
    }
    Ok(())
}

fn validate_activation_outcome(
    outcome: &ActivationReceipt,
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    projection: &ExactProjectionReceipt,
    binding: Option<&ActivationBinding>,
) -> PocResult<()> {
    let binding = binding.ok_or_else(|| {
        PocError::RecoveryRequired("activation outcome has no ratifying binding".to_owned())
    })?;
    let expected_binding_path = activation_directory(request).join("SESSION_BOUND.json");
    let mut expected_readiness_arguments = vec![
        "--path".to_owned(),
        request.readiness_path.display().to_string(),
    ];
    if let Some(needle) = request.readiness_contains.as_deref() {
        expected_readiness_arguments.push("--contains".to_owned());
        expected_readiness_arguments.push(String::from_utf8_lossy(needle).into_owned());
    }
    if outcome.schema_version != SCHEMA_VERSION
        || outcome.activation_operation_id != request.activation_operation_id
        || outcome.session_id != plan.session_id
        || outcome.fresh_allocation_id != binding.fresh_allocation_id
        || outcome.selected_payload_allocation_ids != projection.lower_allocation_ids_newest_first
        || outcome.selected_payload_physical_identities != plan.payload_physical_identities
        || outcome.projection != *projection
        || !outcome.fresh_upper_empty_before_mount
        || outcome.readiness.schema_version != SCHEMA_VERSION
        || outcome.readiness.program != PathBuf::from("adapter-direct-open-read-metadata")
        || outcome.readiness.arguments != expected_readiness_arguments
        || !outcome.readiness.success
        || outcome.readiness.timed_out
        || outcome.readiness.exit_code != Some(0)
        || outcome.session_binding_path != expected_binding_path
        || !outcome.session_binding_parent_synced
    {
        return Err(PocError::RecoveryRequired(
            "activation outcome differs from the ratified exact graph".to_owned(),
        ));
    }
    Ok(())
}

fn validate_private_activation_owner(
    owner: Option<&OwnerGeneration>,
    fresh: &AllocationHandle,
    plan: &ActivationPlanRecord,
) -> PocResult<()> {
    let Some(owner) = owner else {
        return Ok(());
    };
    if owner.schema_version != SCHEMA_VERSION
        || owner.allocation_id != fresh.descriptor.allocation_id
        || owner.operation_id != plan.allocation_operation_id
        || !matches!(
            &owner.subject,
            crate::OwnerSubject::WorkspaceOwned {
                session_id,
                ..
            } if session_id == &plan.session_id
        )
    {
        return Err(PocError::RecoveryRequired(
            "selected activation owner differs from the exact private operation graph".to_owned(),
        ));
    }
    Ok(())
}

fn validate_activation_session(
    record: &SessionRecord,
    fresh: &AllocationHandle,
    owner: Option<&OwnerGeneration>,
    session_dir: &Path,
    plan: &ActivationPlanRecord,
) -> PocResult<()> {
    let owner = owner.ok_or_else(|| {
        PocError::RecoveryRequired(
            "activation session exists without a selected workspace owner".to_owned(),
        )
    })?;
    let owner_lease_epoch = match &owner.subject {
        crate::OwnerSubject::WorkspaceOwned {
            session_id,
            lease_epoch,
        } if session_id == &plan.session_id => *lease_epoch,
        _ => {
            return Err(PocError::RecoveryRequired(
                "activation session owner is not the exact private workspace".to_owned(),
            ));
        }
    };
    let expected_workspace = session_dir.join("mount");
    if record.schema_version != SCHEMA_VERSION
        || record.session_id != plan.session_id
        || record.allocation_id != fresh.descriptor.allocation_id
        || record.lease_epoch != owner_lease_epoch
        || record.owner_epoch != owner.owner_epoch
        || !matches!(
            record.phase,
            SessionPhase::Open | SessionPhase::RecoveryRequired
        )
        || record.workspace_root != expected_workspace
    {
        return Err(PocError::RecoveryRequired(
            "activation session record differs from its allocation/lease binding".to_owned(),
        ));
    }
    Ok(())
}

fn validate_activation_mount(
    attestation: &crate::overlay_adapter::OverlayMountAttestation,
    fresh: &AllocationHandle,
    record: &SessionRecord,
    plan: &ActivationPlanRecord,
) -> PocResult<()> {
    if attestation.schema_version != SCHEMA_VERSION
        || attestation.allocation_id != fresh.descriptor.allocation_id
        || attestation.session_id != record.session_id
        || attestation.lease_epoch != record.lease_epoch
        || attestation.owner_epoch != record.owner_epoch
        || attestation.workspace_root != record.workspace_root
        || attestation.allocation_root != fresh.allocation_root
        || attestation.allocation_upper != fresh.upper_dir
        || attestation.allocation_work != fresh.work_dir
        || attestation.cgroup_procs_path != plan.cgroup_procs_path
    {
        return Err(PocError::RecoveryRequired(
            "activation mount attestation differs from the exact session graph".to_owned(),
        ));
    }
    Ok(())
}

fn validate_removed_activation_session(
    record: &SessionRecord,
    descriptor: &AllocationDescriptor,
    plan: &ActivationPlanRecord,
    session_dir: &Path,
) -> PocResult<()> {
    if record.schema_version != SCHEMA_VERSION
        || record.session_id != plan.session_id
        || record.allocation_id != descriptor.allocation_id
        || !matches!(
            record.phase,
            SessionPhase::Open | SessionPhase::RecoveryRequired
        )
        || record.workspace_root != session_dir.join("mount")
    {
        return Err(PocError::RecoveryRequired(
            "removed activation allocation has a mismatched residual session".to_owned(),
        ));
    }
    Ok(())
}

fn validate_removed_activation_mount(
    attestation: &crate::overlay_adapter::OverlayMountAttestation,
    descriptor: &AllocationDescriptor,
    plan: &ActivationPlanRecord,
    record: &SessionRecord,
) -> PocResult<()> {
    let allocation_root = allocation_root_for(&plan.arena_root, &descriptor.allocation_id)?;
    if attestation.schema_version != SCHEMA_VERSION
        || attestation.allocation_id != descriptor.allocation_id
        || attestation.session_id != plan.session_id
        || attestation.lease_epoch != record.lease_epoch
        || attestation.owner_epoch != record.owner_epoch
        || attestation.workspace_root != record.workspace_root
        || attestation.allocation_root != allocation_root
        || attestation.allocation_upper != allocation_root.join("upper")
        || attestation.allocation_work != allocation_root.join("work")
        || attestation.cgroup_procs_path != plan.cgroup_procs_path
    {
        return Err(PocError::RecoveryRequired(
            "residual activation mount differs from its immutable operation graph".to_owned(),
        ));
    }
    Ok(())
}

fn drain_and_unmount_activation_session(
    attestation: &crate::overlay_adapter::OverlayMountAttestation,
    record: &SessionRecord,
    plan: &ActivationPlanRecord,
    session_anchor: &OwnedFd,
) -> PocResult<(Vec<i32>, crate::ProcessAudit)> {
    if attestation.workspace_root != record.workspace_root
        || attestation.cgroup_procs_path != plan.cgroup_procs_path
    {
        return Err(PocError::RecoveryRequired(
            "activation cleanup scope differs from its mount attestation".to_owned(),
        ));
    }
    let workspace_anchor = open_activation_child_directory_no_symlink(
        "activation workspace",
        session_anchor,
        OsStr::new("mount"),
    )?;
    let mount_state = crate::overlay_adapter::validate_attested_mount_for_cleanup_anchored(
        attestation,
        session_anchor,
        &workspace_anchor,
    )?;
    let audit_identity = crate::process_tree::anchored_workspace_audit_identity(
        attestation,
        &workspace_anchor,
        mount_state,
    )?;
    let cgroup_membership = crate::overlay_adapter::validated_attested_cgroup_path(attestation)?;
    let mut terminated = Vec::new();
    let final_audit_identity = match mount_state {
        crate::overlay_adapter::AttestedMountCleanupState::MountedExact => {
            let (initially_terminated, _) =
                crate::process_tree::terminate_terminal_workspace_references_anchored(
                    &audit_identity,
                    cgroup_membership.as_ref(),
                )?;
            terminated.extend(initially_terminated);
            crate::overlay_adapter::freeze_attested_mount_read_only_anchored(
                attestation,
                session_anchor,
                &workspace_anchor,
            )?;
            let (post_freeze_terminated, post_freeze_audit) =
                crate::process_tree::terminate_terminal_workspace_references_anchored(
                    &audit_identity,
                    cgroup_membership.as_ref(),
                )?;
            terminated.extend(post_freeze_terminated);
            if !post_freeze_audit.is_clear() {
                return Err(PocError::RecoveryRequired(format!(
                    "activation post-freeze process audit is not clear: {post_freeze_audit:?}"
                )));
            }
            crate::overlay_adapter::strict_unmount_attested_frozen_anchored(
                attestation,
                session_anchor,
                workspace_anchor,
            )?;
            let post_unmount_workspace = open_activation_child_directory_no_symlink(
                "post-unmount activation workspace",
                session_anchor,
                OsStr::new("mount"),
            )?;
            crate::overlay_adapter::require_attested_mount_absent_anchored(
                attestation,
                &post_unmount_workspace,
            )?;
            crate::process_tree::anchored_workspace_audit_identity(
                attestation,
                &post_unmount_workspace,
                crate::overlay_adapter::AttestedMountCleanupState::AlreadyAbsent,
            )?
        }
        crate::overlay_adapter::AttestedMountCleanupState::AlreadyAbsent => {
            crate::overlay_adapter::require_attested_mount_absent_anchored(
                attestation,
                &workspace_anchor,
            )?;
            let (replay_terminated, _) =
                crate::process_tree::terminate_terminal_workspace_references_anchored(
                    &audit_identity,
                    cgroup_membership.as_ref(),
                )?;
            terminated.extend(replay_terminated);
            audit_identity
        }
    };
    terminated.sort_unstable();
    terminated.dedup();
    let audit = crate::process_tree::audit_terminal_workspace_anchored(
        &final_audit_identity,
        cgroup_membership.as_ref(),
        true,
    )?;
    if !audit.is_clear() {
        return Err(PocError::RecoveryRequired(format!(
            "activation terminal process audit is not clear: {audit:?}"
        )));
    }
    Ok((terminated, audit))
}

fn validate_unrecorded_private_session(
    session_dir: &Path,
    session_anchor: &OwnedFd,
) -> PocResult<()> {
    if read_optional_json_at::<serde_json::Value>(session_anchor, &session_dir.join("MOUNT.json"))?
        .is_some()
    {
        return Err(PocError::RecoveryRequired(
            "activation mount attestation exists without its session record".to_owned(),
        ));
    }
    let workspace_root = session_dir.join("mount");
    if let Some(workspace_anchor) = open_optional_activation_child_directory_no_symlink(
        "unrecorded activation workspace",
        session_anchor,
        OsStr::new("mount"),
        &workspace_root,
    )? {
        require_unrecorded_workspace_unmounted(session_anchor, &workspace_anchor)?;
    }
    Ok(())
}

fn open_activation_directory_no_symlink(
    label: &str,
    path: &Path,
) -> PocResult<std::os::fd::OwnedFd> {
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
                "open activation directory root",
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
                            "open anchored activation directory",
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

fn open_activation_child_directory_no_symlink(
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
            "open anchored activation child directory",
            Path::new(child),
            std::io::Error::from(error),
        )
    })
}

fn open_optional_activation_child_directory_no_symlink(
    label: &str,
    parent: &OwnedFd,
    child: &OsStr,
    display_path: &Path,
) -> PocResult<Option<OwnedFd>> {
    require_single_component_os(label, child)?;
    match rustix::fs::openat(
        parent,
        child,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) {
        Ok(directory) => Ok(Some(directory)),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(PocError::io(
            "open optional anchored activation directory",
            display_path,
            std::io::Error::from(error),
        )),
    }
}

fn open_recovery_session_anchors(
    control_root: &OwnedFd,
    session_dir: &Path,
) -> PocResult<RecoverySessionAnchors> {
    let sessions_path = session_dir.parent().ok_or_else(|| {
        PocError::Integrity("activation session directory has no sessions parent".to_owned())
    })?;
    let sessions_root = open_optional_activation_child_directory_no_symlink(
        "activation sessions root",
        control_root,
        OsStr::new("sessions"),
        sessions_path,
    )?;
    let session = match sessions_root.as_ref() {
        Some(sessions_root) => {
            let session_name = session_dir.file_name().ok_or_else(|| {
                PocError::Integrity("activation session directory has no name".to_owned())
            })?;
            open_optional_activation_child_directory_no_symlink(
                "activation session directory",
                sessions_root,
                session_name,
                session_dir,
            )?
        }
        None => None,
    };
    Ok(RecoverySessionAnchors {
        sessions_root,
        session,
    })
}

#[cfg(target_os = "linux")]
fn require_unrecorded_workspace_unmounted(
    session_anchor: &std::os::fd::OwnedFd,
    workspace_anchor: &std::os::fd::OwnedFd,
) -> PocResult<()> {
    if crate::overlay_adapter::mount_id_for_fd(session_anchor)?
        != crate::overlay_adapter::mount_id_for_fd(workspace_anchor)?
    {
        return Err(PocError::RecoveryRequired(
            "unrecorded activation workspace is a mount without a durable attestation".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn require_unrecorded_workspace_unmounted(
    _session_anchor: &std::os::fd::OwnedFd,
    _workspace_anchor: &std::os::fd::OwnedFd,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "descriptor-anchored unrecorded workspace validation requires Linux mount IDs".to_owned(),
    ))
}

fn remove_private_activation_allocation(
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    fresh: &AllocationHandle,
    pinned: &PinnedAllocation,
    expected_fence: Option<&lease::TerminalLeaseFenceWitness>,
) -> PocResult<()> {
    let expected_root = allocation_root_for(&request.arena_root, &fresh.descriptor.allocation_id)?;
    if fresh.allocation_root != expected_root
        || fresh.descriptor.schema_version != SCHEMA_VERSION
        || fresh.descriptor.created_by_operation != request.allocation_operation_id
        || request.payload_allocations.iter().any(|payload| {
            payload.descriptor.allocation_id == fresh.descriptor.allocation_id
                || payload.allocation_root == fresh.allocation_root
        })
    {
        return Err(PocError::RecoveryRequired(
            "activation cleanup target is not its exact private allocation".to_owned(),
        ));
    }
    revalidate_pinned_allocation(pinned)?;
    let fence_operation_id = OperationId::from_string(request.activation_operation_id.as_str());
    let _owner_lock = lock_pinned_allocation_owner(pinned, fresh)?;
    let observed_fence = lease::fence_or_reaudit_private_activation_anchored_locked(
        &pinned.handle,
        &pinned.owner,
        &plan.session_id,
        &plan.allocation_operation_id,
        &fence_operation_id,
    )?;
    if observed_fence.as_ref() != expected_fence {
        return Err(PocError::RecoveryRequired(
            "private activation authority changed between terminal fencing and removal".to_owned(),
        ));
    }
    remove_anchored_directory_tree(
        &pinned.prefix,
        OsStr::new(fresh.descriptor.allocation_id.as_str()),
        &pinned.allocation,
        &fresh.allocation_root,
        "terminal private activation allocation",
    )
}

fn remove_private_session_directory(
    control_root: &OwnedFd,
    session_dir: &Path,
    anchors: &RecoverySessionAnchors,
) -> PocResult<()> {
    let Some(sessions_root) = anchors.sessions_root.as_ref() else {
        return require_anchored_entry_absent(
            control_root,
            OsStr::new("sessions"),
            session_dir.parent().unwrap_or(session_dir),
            "activation sessions root",
        );
    };
    let session_name = session_dir.file_name().ok_or_else(|| {
        PocError::Integrity("activation session directory has no name".to_owned())
    })?;
    let Some(session) = anchors.session.as_ref() else {
        return require_anchored_entry_absent(
            sessions_root,
            session_name,
            session_dir,
            "private activation session",
        );
    };
    remove_anchored_directory_tree(
        sessions_root,
        session_name,
        session,
        session_dir,
        "private activation session",
    )
}

fn require_anchored_entry_absent(
    parent: &OwnedFd,
    name: &OsStr,
    display_path: &Path,
    label: &str,
) -> PocResult<()> {
    require_single_component_os(label, name)?;
    match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => Ok(()),
        Ok(_) => Err(PocError::RecoveryRequired(format!(
            "{label} appeared after recovery pinned its parent: {}",
            display_path.display()
        ))),
        Err(error) => Err(PocError::io(
            "revalidate absent activation cleanup entry",
            display_path,
            std::io::Error::from(error),
        )),
    }
}

#[cfg(target_os = "linux")]
fn pin_private_activation_allocation(
    request: &ExactActivationRequest,
    fresh: &AllocationHandle,
) -> PocResult<PinnedAllocation> {
    pin_activation_allocation(&request.arena_root, fresh)
}

#[cfg(target_os = "linux")]
fn pin_activation_allocation(
    arena_root: &Path,
    fresh: &AllocationHandle,
) -> PocResult<PinnedAllocation> {
    if allocation_root_for(arena_root, &fresh.descriptor.allocation_id)? != fresh.allocation_root {
        return Err(PocError::RecoveryRequired(
            "activation allocation handle is not at its canonical arena path".to_owned(),
        ));
    }
    let prefix = fresh
        .descriptor
        .allocation_id
        .as_str()
        .get(..2)
        .ok_or_else(|| PocError::Integrity("activation allocation ID is too short".to_owned()))?;
    let arena = open_activation_directory_no_symlink("activation allocation arena", arena_root)?;
    let prefix_anchor = open_activation_child_directory_no_symlink(
        "activation allocation prefix",
        &arena,
        OsStr::new(prefix),
    )?;
    let allocation = open_activation_child_directory_no_symlink(
        "private activation allocation",
        &prefix_anchor,
        OsStr::new(fresh.descriptor.allocation_id.as_str()),
    )?;
    finish_pinned_allocation(arena, prefix_anchor, allocation, fresh)
}

#[cfg(target_os = "linux")]
fn finish_pinned_allocation(
    arena: OwnedFd,
    prefix: OwnedFd,
    allocation: OwnedFd,
    fresh: &AllocationHandle,
) -> PocResult<PinnedAllocation> {
    let expected_upper = fresh.allocation_root.join("upper");
    let expected_work = fresh.allocation_root.join("work");
    let expected_owner = fresh.allocation_root.join("owner");
    if fresh.upper_dir != expected_upper
        || fresh.work_dir != expected_work
        || fresh.owner_dir != expected_owner
    {
        return Err(PocError::RecoveryRequired(
            "activation allocation handle has non-canonical child paths".to_owned(),
        ));
    }
    let owner = open_activation_child_directory_no_symlink(
        "private activation owner directory",
        &allocation,
        OsStr::new("owner"),
    )?;
    let upper = open_activation_child_directory_no_symlink(
        "activation allocation upper directory",
        &allocation,
        OsStr::new("upper"),
    )?;
    let work = open_activation_child_directory_no_symlink(
        "activation allocation work directory",
        &allocation,
        OsStr::new("work"),
    )?;
    let descriptor_path = fresh.allocation_root.join("ALLOCATION.json");
    let descriptor = read_optional_json_at::<AllocationDescriptor>(&allocation, &descriptor_path)?
        .ok_or_else(|| {
            PocError::RecoveryRequired(
                "private activation allocation lost its descriptor".to_owned(),
            )
        })?;
    if descriptor != fresh.descriptor
        || crate::overlay_adapter::mount_id_for_fd(&arena)?
            != crate::overlay_adapter::mount_id_for_fd(&prefix)?
        || crate::overlay_adapter::mount_id_for_fd(&prefix)?
            != crate::overlay_adapter::mount_id_for_fd(&allocation)?
        || crate::overlay_adapter::mount_id_for_fd(&allocation)?
            != crate::overlay_adapter::mount_id_for_fd(&owner)?
        || crate::overlay_adapter::mount_id_for_fd(&allocation)?
            != crate::overlay_adapter::mount_id_for_fd(&upper)?
        || crate::overlay_adapter::mount_id_for_fd(&allocation)?
            != crate::overlay_adapter::mount_id_for_fd(&work)?
    {
        return Err(PocError::RecoveryRequired(
            "private activation allocation changed while it was pinned".to_owned(),
        ));
    }
    let descriptor_root = PathBuf::from("/proc/self/fd").join(allocation.as_raw_fd().to_string());
    let allocation_stat = rustix::fs::fstat(&allocation).map_err(|error| {
        PocError::io(
            "stat pinned activation allocation",
            &fresh.allocation_root,
            std::io::Error::from(error),
        )
    })?;
    let upper_stat = rustix::fs::fstat(&upper).map_err(|error| {
        PocError::io(
            "stat pinned activation upper",
            &fresh.upper_dir,
            std::io::Error::from(error),
        )
    })?;
    let work_stat = rustix::fs::fstat(&work).map_err(|error| {
        PocError::io(
            "stat pinned activation work",
            &fresh.work_dir,
            std::io::Error::from(error),
        )
    })?;
    let owner_stat = rustix::fs::fstat(&owner).map_err(|error| {
        PocError::io(
            "stat pinned activation owner",
            &fresh.owner_dir,
            std::io::Error::from(error),
        )
    })?;
    let identity = AllocationPhysicalIdentity {
        allocation_device: allocation_stat.st_dev as u64,
        allocation_inode: allocation_stat.st_ino as u64,
        upper_device: upper_stat.st_dev as u64,
        upper_inode: upper_stat.st_ino as u64,
        work_device: work_stat.st_dev as u64,
        work_inode: work_stat.st_ino as u64,
        owner_device: owner_stat.st_dev as u64,
        owner_inode: owner_stat.st_ino as u64,
    };
    let handle = AllocationHandle {
        descriptor,
        upper_dir: descriptor_root.join("upper"),
        work_dir: descriptor_root.join("work"),
        owner_dir: descriptor_root.join("owner"),
        allocation_root: descriptor_root,
    };
    Ok(PinnedAllocation {
        arena,
        prefix,
        allocation,
        upper,
        work,
        owner,
        handle,
        supplied: fresh.clone(),
        identity,
    })
}

#[cfg(not(target_os = "linux"))]
fn pin_activation_allocation(
    _arena_root: &Path,
    _fresh: &AllocationHandle,
) -> PocResult<PinnedAllocation> {
    Err(PocError::Unsupported(
        "descriptor-anchored activation allocation operations require Linux mount IDs".to_owned(),
    ))
}

#[cfg(not(target_os = "linux"))]
fn pin_private_activation_allocation(
    _request: &ExactActivationRequest,
    _fresh: &AllocationHandle,
) -> PocResult<PinnedAllocation> {
    Err(PocError::Unsupported(
        "descriptor-anchored activation allocation recovery requires Linux mount IDs".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn lock_pinned_allocation_owner(
    pinned: &PinnedAllocation,
    fresh: &AllocationHandle,
) -> PocResult<File> {
    require_directory_entry_matches(
        &pinned.allocation,
        OsStr::new("owner"),
        &pinned.owner,
        &fresh.owner_dir,
        "private activation owner directory",
    )?;
    let lock_path = fresh.owner_dir.join("LOCK");
    let lock_fd = rustix::fs::openat(
        &pinned.owner,
        OsStr::new("LOCK"),
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        PocError::io(
            "open pinned activation owner lock",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    let metadata = rustix::fs::fstat(&lock_fd).map_err(|error| {
        PocError::io(
            "stat pinned activation owner lock",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    if raw_mode_file_type(metadata.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::RegularFile
    {
        return Err(PocError::RecoveryRequired(
            "private activation owner lock is not a regular file".to_owned(),
        ));
    }
    let lock = File::from(lock_fd);
    rustix::fs::flock(&lock, FlockOperation::LockExclusive).map_err(|error| {
        PocError::io(
            "lock pinned activation owner",
            &lock_path,
            std::io::Error::from(error),
        )
    })?;
    let installed =
        rustix::fs::statat(&pinned.owner, OsStr::new("LOCK"), AtFlags::SYMLINK_NOFOLLOW).map_err(
            |error| {
                PocError::io(
                    "revalidate pinned activation owner lock",
                    &lock_path,
                    std::io::Error::from(error),
                )
            },
        )?;
    if raw_mode_file_type(installed.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::RegularFile
        || installed.st_dev != metadata.st_dev
        || installed.st_ino != metadata.st_ino
    {
        return Err(PocError::RecoveryRequired(
            "private activation owner lock changed while it was acquired".to_owned(),
        ));
    }
    Ok(lock)
}

#[cfg(not(target_os = "linux"))]
fn lock_pinned_allocation_owner(
    _pinned: &PinnedAllocation,
    _fresh: &AllocationHandle,
) -> PocResult<File> {
    Err(PocError::Unsupported(
        "descriptor-anchored activation owner fencing requires Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn remove_anchored_directory_tree(
    parent: &OwnedFd,
    name: &OsStr,
    directory: &OwnedFd,
    display_path: &Path,
    label: &str,
) -> PocResult<()> {
    require_directory_entry_matches(parent, name, directory, display_path, label)?;
    let parent_mount_id = crate::overlay_adapter::mount_id_for_fd(parent)?;
    let mount_id = crate::overlay_adapter::mount_id_for_fd(directory)?;
    if parent_mount_id != mount_id {
        return Err(PocError::RecoveryRequired(format!(
            "activation cleanup refuses to enter a mounted directory at {}",
            display_path.display()
        )));
    }
    let quarantine = quarantine_anchored_entry(parent, name, directory, display_path, label)?;
    remove_anchored_directory_contents(directory, mount_id, display_path)?;
    require_directory_entry_matches(parent, &quarantine, directory, display_path, label)?;
    rustix::fs::unlinkat(parent, &quarantine, AtFlags::REMOVEDIR).map_err(|error| {
        PocError::io(
            "remove anchored activation directory",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    require_anchored_entry_absent(parent, &quarantine, display_path, label)?;
    fsync_activation_anchor(parent, display_path.parent().unwrap_or(display_path))
}

#[cfg(target_os = "linux")]
fn quarantine_anchored_entry(
    parent: &OwnedFd,
    name: &OsStr,
    entry: &OwnedFd,
    display_path: &Path,
    label: &str,
) -> PocResult<OsString> {
    require_directory_entry_matches_or_type(parent, name, entry, display_path, label)?;
    let quarantine = OsString::from(format!(".activation-cleanup-{}", uuid::Uuid::new_v4()));
    let source = CString::new(name.as_bytes()).map_err(|_| {
        PocError::Integrity(format!("{label} contains NUL: {}", display_path.display()))
    })?;
    let target = CString::new(quarantine.as_bytes()).map_err(|_| {
        PocError::Integrity("activation cleanup quarantine contains NUL".to_owned())
    })?;
    // SAFETY: both names are NUL-terminated single components beneath the
    // pinned parent; RENAME_NOREPLACE makes quarantine installation atomic.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.as_raw_fd(),
            source.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(PocError::io(
            "atomically quarantine activation cleanup entry",
            display_path,
            std::io::Error::last_os_error(),
        ));
    }
    require_directory_entry_matches_or_type(parent, &quarantine, entry, display_path, label)?;
    Ok(quarantine)
}

#[cfg(not(target_os = "linux"))]
fn remove_anchored_directory_tree(
    _parent: &OwnedFd,
    _name: &OsStr,
    _directory: &OwnedFd,
    _display_path: &Path,
    _label: &str,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "descriptor-anchored recursive activation cleanup requires Linux mount IDs".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn remove_anchored_directory_contents(
    directory: &OwnedFd,
    root_mount_id: u64,
    display_path: &Path,
) -> PocResult<()> {
    let reader = rustix::fs::Dir::read_from(directory.as_fd()).map_err(|error| {
        PocError::io(
            "read anchored activation cleanup directory",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let mut names = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|error| {
            PocError::io(
                "read anchored activation cleanup entry",
                display_path,
                std::io::Error::from(error),
            )
        })?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        names.push(OsString::from_vec(name.to_vec()));
    }
    names.sort();
    for name in names {
        let child_path = display_path.join(&name);
        let before =
            rustix::fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
                PocError::io(
                    "inspect anchored activation cleanup entry",
                    &child_path,
                    std::io::Error::from(error),
                )
            })?;
        let file_type = raw_mode_file_type(before.st_mode as rustix::fs::RawMode);
        if file_type == rustix::fs::FileType::Directory {
            let child = open_activation_child_directory_no_symlink(
                "activation cleanup child",
                directory,
                &name,
            )?;
            require_stat_matches_fd(
                before.st_dev as u64,
                before.st_ino as u64,
                before.st_mode as rustix::fs::RawMode,
                &child,
                &child_path,
            )?;
            if crate::overlay_adapter::mount_id_for_fd(&child)? != root_mount_id {
                return Err(PocError::RecoveryRequired(format!(
                    "activation cleanup refuses to cross a mount at {}",
                    child_path.display()
                )));
            }
            let quarantine = quarantine_anchored_entry(
                directory,
                &name,
                &child,
                &child_path,
                "activation cleanup child",
            )?;
            remove_anchored_directory_contents(&child, root_mount_id, &child_path)?;
            require_directory_entry_matches(
                directory,
                &quarantine,
                &child,
                &child_path,
                "activation cleanup child",
            )?;
            rustix::fs::unlinkat(directory, &quarantine, AtFlags::REMOVEDIR).map_err(|error| {
                PocError::io(
                    "remove anchored activation cleanup child",
                    &child_path,
                    std::io::Error::from(error),
                )
            })?;
        } else {
            let child = rustix::fs::openat(
                directory,
                &name,
                rustix::fs::OFlags::PATH
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|error| {
                PocError::io(
                    "pin anchored activation cleanup entry",
                    &child_path,
                    std::io::Error::from(error),
                )
            })?;
            require_stat_matches_fd(
                before.st_dev as u64,
                before.st_ino as u64,
                before.st_mode as rustix::fs::RawMode,
                &child,
                &child_path,
            )?;
            if crate::overlay_adapter::mount_id_for_fd(&child)? != root_mount_id {
                return Err(PocError::RecoveryRequired(format!(
                    "activation cleanup refuses to remove a mounted entry at {}",
                    child_path.display()
                )));
            }
            let quarantine = quarantine_anchored_entry(
                directory,
                &name,
                &child,
                &child_path,
                "activation cleanup entry",
            )?;
            rustix::fs::unlinkat(directory, &quarantine, AtFlags::empty()).map_err(|error| {
                PocError::io(
                    "remove anchored activation cleanup entry",
                    &child_path,
                    std::io::Error::from(error),
                )
            })?;
        }
    }
    fsync_activation_anchor(directory, display_path)
}

fn require_directory_entry_matches_or_type(
    parent: &OwnedFd,
    name: &OsStr,
    entry: &OwnedFd,
    display_path: &Path,
    label: &str,
) -> PocResult<()> {
    require_single_component_os(label, name)?;
    let expected = rustix::fs::fstat(entry).map_err(|error| {
        PocError::io(
            "stat pinned activation cleanup entry",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let observed =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
            PocError::io(
                "revalidate pinned activation cleanup entry",
                display_path,
                std::io::Error::from(error),
            )
        })?;
    if observed.st_dev != expected.st_dev
        || observed.st_ino != expected.st_ino
        || raw_mode_file_type(observed.st_mode as rustix::fs::RawMode)
            != raw_mode_file_type(expected.st_mode as rustix::fs::RawMode)
    {
        return Err(PocError::RecoveryRequired(format!(
            "{label} changed before atomic quarantine: {}",
            display_path.display()
        )));
    }
    Ok(())
}

fn require_directory_entry_matches(
    parent: &OwnedFd,
    name: &OsStr,
    directory: &OwnedFd,
    display_path: &Path,
    label: &str,
) -> PocResult<()> {
    require_single_component_os(label, name)?;
    let expected = rustix::fs::fstat(directory).map_err(|error| {
        PocError::io(
            "stat pinned activation directory",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    let observed =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
            PocError::io(
                "revalidate pinned activation directory",
                display_path,
                std::io::Error::from(error),
            )
        })?;
    if raw_mode_file_type(observed.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::Directory
        || observed.st_dev != expected.st_dev
        || observed.st_ino != expected.st_ino
    {
        return Err(PocError::RecoveryRequired(format!(
            "{label} changed after it was pinned: {}",
            display_path.display()
        )));
    }
    Ok(())
}

fn require_stat_matches_fd(
    expected_device: u64,
    expected_inode: u64,
    expected_mode: rustix::fs::RawMode,
    observed_fd: &OwnedFd,
    display_path: &Path,
) -> PocResult<()> {
    let observed = rustix::fs::fstat(observed_fd).map_err(|error| {
        PocError::io(
            "stat pinned activation cleanup entry",
            display_path,
            std::io::Error::from(error),
        )
    })?;
    if observed.st_dev as u64 != expected_device
        || observed.st_ino as u64 != expected_inode
        || raw_mode_file_type(observed.st_mode as rustix::fs::RawMode)
            != raw_mode_file_type(expected_mode)
    {
        return Err(PocError::RecoveryRequired(format!(
            "activation cleanup entry changed while it was pinned: {}",
            display_path.display()
        )));
    }
    Ok(())
}

fn raw_mode_file_type(mode: rustix::fs::RawMode) -> rustix::fs::FileType {
    rustix::fs::FileType::from_raw_mode(mode)
}

fn validate_recovery_receipt(
    receipt: &ActivationRecoveryReceipt,
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    projection: &ExactProjectionReceipt,
) -> PocResult<()> {
    if receipt.schema_version != SCHEMA_VERSION
        || receipt.activation_operation_id != request.activation_operation_id
        || receipt.allocation_operation_id != request.allocation_operation_id
        || receipt.selected_ref != request.selected_ref
        || receipt.projection != *projection
        || receipt.session_id != plan.session_id
        || !receipt.mount_removed
        || !receipt.process_audit_clear
        || !receipt.selected_payloads_preserved
        || receipt.selected_payload_allocations_before != receipt.selected_payload_allocations_after
        || receipt.selected_payload_allocations_after != request.payload_allocations
        || receipt.selected_payload_physical_identities_before
            != receipt.selected_payload_physical_identities_after
        || receipt.selected_payload_physical_identities_after != plan.payload_physical_identities
        || receipt.selected_payload_physical_identities_after.len()
            != request.payload_allocations.len()
        || receipt.selected_payload_owners_before != receipt.selected_payload_owners_after
        || receipt.selected_payload_owners_after.len() != request.payload_allocations.len()
        || receipt.executable_authority_returned
    {
        return Err(PocError::RecoveryRequired(
            "activation recovery receipt has a mismatched or non-terminal scope".to_owned(),
        ));
    }
    if let Some(descriptor) = receipt.fresh_allocation.as_ref() {
        if descriptor.schema_version != SCHEMA_VERSION
            || descriptor.created_by_operation != request.allocation_operation_id
            || request
                .payload_allocations
                .iter()
                .any(|payload| payload.descriptor.allocation_id == descriptor.allocation_id)
        {
            return Err(PocError::RecoveryRequired(
                "activation recovery receipt names an invalid fresh allocation".to_owned(),
            ));
        }
    }
    if let Some(owner) = receipt.fresh_owner.as_ref() {
        let descriptor = receipt.fresh_allocation.as_ref().ok_or_else(|| {
            PocError::RecoveryRequired(
                "activation recovery owner has no fresh allocation".to_owned(),
            )
        })?;
        if owner.schema_version != SCHEMA_VERSION
            || owner.allocation_id != descriptor.allocation_id
            || owner.operation_id != request.allocation_operation_id
            || !matches!(
                &owner.subject,
                crate::OwnerSubject::WorkspaceOwned { session_id, .. }
                    if session_id == &plan.session_id
            )
        {
            return Err(PocError::RecoveryRequired(
                "activation recovery owner differs from the exact operation graph".to_owned(),
            ));
        }
    }
    match receipt.disposition {
        ActivationRecoveryDisposition::Old => {
            if receipt.binding.is_some()
                || receipt.original_outcome.is_some()
                || receipt.terminal_session_record.is_some()
                || receipt.allocation_retained
                || receipt.allocation_removed != receipt.fresh_allocation.is_some()
            {
                return Err(PocError::RecoveryRequired(
                    "old activation recovery receipt contains committed state".to_owned(),
                ));
            }
            if receipt.authority_fenced != receipt.terminal_lease_fence.is_some()
                || (receipt.fresh_owner.is_some() && receipt.terminal_lease_fence.is_none())
            {
                return Err(PocError::RecoveryRequired(
                    "old activation recovery receipt has incomplete authority evidence".to_owned(),
                ));
            }
            if let Some(fence) = receipt.terminal_lease_fence.as_ref() {
                let (prior_lease_epoch, prior_owner_epoch) = if let Some(owner) =
                    receipt.fresh_owner.as_ref()
                {
                    let lease_epoch = match &owner.subject {
                        crate::OwnerSubject::WorkspaceOwned {
                            session_id,
                            lease_epoch,
                        } if session_id == &plan.session_id => *lease_epoch,
                        _ => {
                            return Err(PocError::RecoveryRequired(
                                "old activation recovery owner is not workspace-owned".to_owned(),
                            ));
                        }
                    };
                    (lease_epoch, owner.owner_epoch)
                } else {
                    // LEASE may be durable before CURRENT.  The lease layer
                    // permits only its exact initial 1/1 tuple in that crash
                    // window and fences it to 2/2 without selecting an owner.
                    (1, 1)
                };
                validate_terminal_lease_fence_tuple(
                    fence,
                    request,
                    plan,
                    receipt.fresh_allocation.as_ref().ok_or_else(|| {
                        PocError::RecoveryRequired(
                            "old activation fence has no fresh allocation identity".to_owned(),
                        )
                    })?,
                    &plan.session_id,
                    prior_lease_epoch,
                    prior_owner_epoch,
                )?;
            }
        }
        ActivationRecoveryDisposition::CompleteNew => {
            let descriptor = receipt.fresh_allocation.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "complete-new activation has no fresh allocation".to_owned(),
                )
            })?;
            let binding = receipt.binding.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "complete-new activation has no ratifying binding".to_owned(),
                )
            })?;
            validate_activation_binding(binding, request, plan, projection, Some(descriptor))?;
            if let Some(outcome) = receipt.original_outcome.as_ref() {
                validate_activation_outcome(outcome, request, plan, projection, Some(binding))?;
            }
            let terminal = receipt.terminal_session_record.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "complete-new activation has no terminal session record".to_owned(),
                )
            })?;
            let fence = receipt.terminal_lease_fence.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "complete-new activation has no terminal lease fence".to_owned(),
                )
            })?;
            validate_terminal_lease_fence(fence, request, plan, descriptor, terminal)?;
            if terminal.schema_version != SCHEMA_VERSION
                || terminal.session_id != plan.session_id
                || terminal.allocation_id != descriptor.allocation_id
                || terminal.phase != SessionPhase::RecoveryRequired
                || receipt.fresh_owner.is_none()
                || receipt.allocation_removed
                || !receipt.allocation_retained
                || !receipt.locator_pin_durable
                || !receipt.authority_fenced
            {
                return Err(PocError::RecoveryRequired(
                    "complete-new activation recovery is not terminal and exact".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn reaudit_activation_recovery(
    receipt: &ActivationRecoveryReceipt,
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    projection: &ExactProjectionReceipt,
    operation_lock: &ActivationOperationLock,
) -> PocResult<()> {
    let pinned_payloads = pin_validated_payload_allocations(&request.payload_allocations)?;
    let payloads = pinned_payload_handles(&pinned_payloads);
    let payload_identities = pinned_payload_identities(&pinned_payloads);
    let payload_owners = selected_pinned_payload_owners(&pinned_payloads)?;
    if payloads != receipt.selected_payload_allocations_after
        || payload_identities != receipt.selected_payload_physical_identities_after
        || payload_owners != receipt.selected_payload_owners_after
    {
        return Err(PocError::RecoveryRequired(
            "selected payload state changed after activation recovery".to_owned(),
        ));
    }
    let intent_path = activation_directory(request).join(ACTIVATION_RECOVERY_INTENT_FILE);
    let intent =
        read_optional_json_at::<ActivationRecoveryIntent>(&operation_lock.directory, &intent_path)?
            .ok_or_else(|| {
                PocError::RecoveryRequired(
                    "activation recovery receipt has no immutable recovery intent".to_owned(),
                )
            })?;
    validate_recovery_intent(
        &intent,
        request,
        plan,
        projection,
        receipt.disposition,
        receipt.fresh_allocation.as_ref(),
        receipt.binding.as_ref(),
    )?;
    let fresh = discover_fresh_allocations(request)?;
    let sessions_root = request.control_root.join("sessions");
    let session_dir = sessions_root.join(plan.session_id.as_str());
    let session_anchors =
        open_recovery_session_anchors(&operation_lock.control_root, &session_dir)?;
    match receipt.disposition {
        ActivationRecoveryDisposition::Old => {
            if !fresh.is_empty() || session_anchors.session.is_some() {
                return Err(PocError::RecoveryRequired(
                    "old activation recovery no longer has terminal cleanup".to_owned(),
                ));
            }
        }
        ActivationRecoveryDisposition::CompleteNew => {
            let session_anchor = session_anchors.session.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "complete-new activation lost its terminal session directory".to_owned(),
                )
            })?;
            let descriptor = receipt.fresh_allocation.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "complete-new activation has no retained allocation".to_owned(),
                )
            })?;
            if fresh.len() != 1 || fresh[0].supplied.descriptor != *descriptor {
                return Err(PocError::RecoveryRequired(
                    "complete-new activation retained allocation changed".to_owned(),
                ));
            }
            let pinned_fresh = &fresh[0];
            let terminal_path = session_dir.join("SESSION.json");
            let terminal = read_optional_json_at::<SessionRecord>(session_anchor, &terminal_path)?
                .ok_or_else(|| {
                    PocError::RecoveryRequired(
                        "complete-new activation lost its terminal session record".to_owned(),
                    )
                })?;
            if Some(&terminal) != receipt.terminal_session_record.as_ref() {
                return Err(PocError::RecoveryRequired(
                    "complete-new activation terminal session changed".to_owned(),
                ));
            }
            validate_removed_activation_session(&terminal, descriptor, plan, &session_dir)?;
            let mount_path = session_dir.join("MOUNT.json");
            let attestation = read_optional_json_at::<
                crate::overlay_adapter::OverlayMountAttestation,
            >(session_anchor, &mount_path)?
            .ok_or_else(|| {
                PocError::RecoveryRequired(
                    "complete-new activation lost its terminal mount attestation".to_owned(),
                )
            })?;
            validate_removed_activation_mount(&attestation, descriptor, plan, &terminal)?;
            let workspace_anchor = open_activation_child_directory_no_symlink(
                "terminal activation workspace",
                session_anchor,
                OsStr::new("mount"),
            )?;
            crate::overlay_adapter::require_attested_mount_absent_anchored(
                &attestation,
                &workspace_anchor,
            )?;
            let audit_identity = crate::process_tree::anchored_workspace_audit_identity(
                &attestation,
                &workspace_anchor,
                crate::overlay_adapter::AttestedMountCleanupState::AlreadyAbsent,
            )?;
            let cgroup = crate::overlay_adapter::validated_attested_cgroup_path(&attestation)?;
            let audit = crate::process_tree::audit_terminal_workspace_anchored(
                &audit_identity,
                cgroup.as_ref(),
                true,
            )?;
            if !audit.is_clear() {
                return Err(PocError::RecoveryRequired(format!(
                    "complete-new activation terminal audit regressed: {audit:?}"
                )));
            }
            let _owner_lock = lock_pinned_allocation_owner(pinned_fresh, &pinned_fresh.supplied)?;
            let owner = crate::owner::selected_owner_locked_anchored(
                &pinned_fresh.handle.allocation_root,
                &pinned_fresh.owner,
            )?
            .ok_or_else(|| {
                PocError::RecoveryRequired(
                    "complete-new activation lost its selected owner".to_owned(),
                )
            })?;
            if Some(&owner) != receipt.fresh_owner.as_ref() {
                return Err(PocError::RecoveryRequired(
                    "complete-new activation retained owner changed".to_owned(),
                ));
            }
            let recorded_fence = receipt.terminal_lease_fence.as_ref().ok_or_else(|| {
                PocError::RecoveryRequired(
                    "complete-new activation lost its terminal lease fence witness".to_owned(),
                )
            })?;
            let fence_operation_id =
                OperationId::from_string(request.activation_operation_id.as_str());
            let observed_fence = lease::reaudit_terminal_session_fence_tuple_anchored_locked(
                &pinned_fresh.handle,
                &pinned_fresh.owner,
                &terminal.session_id,
                terminal.lease_epoch,
                terminal.owner_epoch,
                &plan.allocation_operation_id,
                &fence_operation_id,
            )?;
            if &observed_fence != recorded_fence {
                return Err(PocError::RecoveryRequired(
                    "complete-new activation terminal lease fence changed".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_terminal_lease_fence(
    fence: &lease::TerminalLeaseFenceWitness,
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    descriptor: &AllocationDescriptor,
    terminal: &SessionRecord,
) -> PocResult<()> {
    validate_terminal_lease_fence_tuple(
        fence,
        request,
        plan,
        descriptor,
        &terminal.session_id,
        terminal.lease_epoch,
        terminal.owner_epoch,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_terminal_lease_fence_tuple(
    fence: &lease::TerminalLeaseFenceWitness,
    request: &ExactActivationRequest,
    plan: &ActivationPlanRecord,
    descriptor: &AllocationDescriptor,
    session_id: &SessionId,
    prior_lease_epoch: u64,
    prior_owner_epoch: u64,
) -> PocResult<()> {
    let fence_operation_id = OperationId::from_string(request.activation_operation_id.as_str());
    let expected_lease_epoch = prior_lease_epoch.checked_add(1).ok_or_else(|| {
        PocError::RecoveryRequired("terminal activation lease epoch exhausted".to_owned())
    })?;
    let expected_owner_epoch = prior_owner_epoch.checked_add(1).ok_or_else(|| {
        PocError::RecoveryRequired("terminal activation owner epoch exhausted".to_owned())
    })?;
    if fence.schema_version != SCHEMA_VERSION
        || fence.operation_id != fence_operation_id
        || fence.prior_operation_id != plan.allocation_operation_id
        || fence.allocation_id != descriptor.allocation_id
        || &fence.session_id != session_id
        || fence.prior_lease_epoch != prior_lease_epoch
        || fence.prior_owner_epoch != prior_owner_epoch
        || fence.fenced_lease_epoch != expected_lease_epoch
        || fence.fenced_owner_epoch != expected_owner_epoch
        || !fence.writer_revoked
        || !fence.deleter_revoked
    {
        return Err(PocError::RecoveryRequired(
            "terminal activation lease fence differs from the exact operation graph".to_owned(),
        ));
    }
    Ok(())
}

fn read_optional_json_at<T: DeserializeOwned>(
    parent: &OwnedFd,
    path: &Path,
) -> PocResult<Option<T>> {
    let file_name = path.file_name().ok_or_else(|| {
        PocError::RecoveryRequired("activation control record has no file name".to_owned())
    })?;
    require_single_component_os("activation control-record name", file_name)?;
    let before = match rustix::fs::statat(parent, file_name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => {
            return Err(PocError::io(
                "inspect anchored activation JSON",
                path,
                std::io::Error::from(error),
            ));
        }
    };
    if raw_mode_file_type(before.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::RegularFile
    {
        return Err(PocError::RecoveryRequired(format!(
            "anchored activation control record is not a regular file: {}",
            path.display()
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
            "open anchored activation JSON",
            path,
            std::io::Error::from(error),
        )
    })?;
    let opened = rustix::fs::fstat(&file_fd).map_err(|error| {
        PocError::io(
            "stat anchored activation JSON",
            path,
            std::io::Error::from(error),
        )
    })?;
    if raw_mode_file_type(opened.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::RegularFile
        || before.st_dev != opened.st_dev
        || before.st_ino != opened.st_ino
        || opened.st_size as u64 > MAX_ACTIVATION_JSON_BYTES
    {
        return Err(PocError::RecoveryRequired(format!(
            "anchored activation control record changed or is oversized: {}",
            path.display()
        )));
    }
    let mut file = File::from(file_fd);
    let mut first = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_ACTIVATION_JSON_BYTES + 1)
        .read_to_end(&mut first)
        .map_err(|error| PocError::io("read anchored activation JSON", path, error))?;
    if first.len() as u64 > MAX_ACTIVATION_JSON_BYTES {
        return Err(PocError::RecoveryRequired(format!(
            "anchored activation control record is oversized: {}",
            path.display()
        )));
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| PocError::io("rewind anchored activation JSON", path, error))?;
    let mut second = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_ACTIVATION_JSON_BYTES + 1)
        .read_to_end(&mut second)
        .map_err(|error| PocError::io("reread anchored activation JSON", path, error))?;
    let opened_after = rustix::fs::fstat(&file).map_err(|error| {
        PocError::io(
            "restat anchored activation JSON",
            path,
            std::io::Error::from(error),
        )
    })?;
    let after =
        rustix::fs::statat(parent, file_name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
            PocError::io(
                "reinspect anchored activation JSON",
                path,
                std::io::Error::from(error),
            )
        })?;
    if !activation_json_stat_is_stable(&opened, &opened_after)
        || !activation_json_stat_is_stable(&opened, &after)
        || opened.st_size as usize != first.len()
        || first != second
    {
        return Err(PocError::RecoveryRequired(format!(
            "anchored activation control record metadata or content changed while reading: {}",
            path.display()
        )));
    }
    let value = serde_json::from_slice(&first)?;
    Ok(Some(value))
}

fn activation_json_stat_is_stable(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_mode == right.st_mode
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

fn require_single_component_os(label: &str, value: &OsStr) -> PocResult<()> {
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

fn write_immutable_json_at<T>(parent: &OwnedFd, path: &Path, value: &T) -> PocResult<()>
where
    T: DeserializeOwned + Eq + Serialize,
{
    let file_name = path.file_name().ok_or_else(|| {
        PocError::RecoveryRequired("activation control record has no file name".to_owned())
    })?;
    require_single_component_os("activation control-record name", file_name)?;
    let temporary_name = OsString::from(format!(
        ".{}.{}.tmp",
        file_name
            .to_str()
            .ok_or_else(|| PocError::RecoveryRequired(
                "activation control record has a non-UTF-8 name".to_owned()
            ))?,
        uuid::Uuid::new_v4()
    ));
    let temporary_path = path.with_file_name(&temporary_name);
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
            "create anchored activation temporary",
            &temporary_path,
            std::io::Error::from(error),
        )
    })?;
    let mut file = File::from(temporary_fd);
    let result = (|| {
        file.write_all(&bytes).map_err(|error| {
            PocError::io("write activation control record", &temporary_path, error)
        })?;
        file.sync_all().map_err(|error| {
            PocError::io("fsync activation control record", &temporary_path, error)
        })?;
        let temporary_metadata = rustix::fs::fstat(&file).map_err(|error| {
            PocError::io(
                "stat anchored activation temporary",
                &temporary_path,
                std::io::Error::from(error),
            )
        })?;
        drop(file);
        let mut installed_new = false;
        match rustix::fs::linkat(parent, &temporary_name, parent, file_name, AtFlags::empty()) {
            Ok(()) => installed_new = true,
            Err(rustix::io::Errno::EXIST) => {
                let observed = read_optional_json_at::<T>(parent, path)?.ok_or_else(|| {
                    PocError::RecoveryRequired(format!(
                        "immutable activation control record disappeared: {}",
                        path.display()
                    ))
                })?;
                if &observed != value {
                    return Err(PocError::RecoveryRequired(format!(
                        "immutable activation control-record collision at {}",
                        path.display()
                    )));
                }
            }
            Err(error) => {
                return Err(PocError::io(
                    "install immutable activation control record",
                    path,
                    std::io::Error::from(error),
                ));
            }
        }
        let installed_metadata = rustix::fs::statat(parent, file_name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| {
            PocError::io(
                "verify immutable activation control-record binding",
                path,
                std::io::Error::from(error),
            )
        })?;
        if installed_new
            && (installed_metadata.st_dev != temporary_metadata.st_dev
                || installed_metadata.st_ino != temporary_metadata.st_ino
                || raw_mode_file_type(installed_metadata.st_mode as rustix::fs::RawMode)
                    != rustix::fs::FileType::RegularFile)
        {
            return Err(PocError::RecoveryRequired(format!(
                "immutable activation control record was rebound during installation: {}",
                path.display()
            )));
        }
        rustix::fs::unlinkat(parent, &temporary_name, AtFlags::empty()).map_err(|error| {
            PocError::io(
                "remove anchored activation temporary",
                &temporary_path,
                std::io::Error::from(error),
            )
        })?;
        fsync_activation_anchor(parent, path.parent().unwrap_or(path))?;
        let installed = read_optional_json_at::<T>(parent, path)?.ok_or_else(|| {
            PocError::RecoveryRequired(format!(
                "immutable activation control record disappeared after installation: {}",
                path.display()
            ))
        })?;
        if &installed != value {
            return Err(PocError::RecoveryRequired(format!(
                "immutable activation control record changed after installation: {}",
                path.display()
            )));
        }
        if installed_new {
            let final_metadata = rustix::fs::statat(parent, file_name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|error| {
                PocError::io(
                    "revalidate immutable activation control-record binding",
                    path,
                    std::io::Error::from(error),
                )
            })?;
            if final_metadata.st_dev != temporary_metadata.st_dev
                || final_metadata.st_ino != temporary_metadata.st_ino
                || raw_mode_file_type(final_metadata.st_mode as rustix::fs::RawMode)
                    != rustix::fs::FileType::RegularFile
            {
                return Err(PocError::RecoveryRequired(format!(
                    "immutable activation control record was rebound after installation: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, &temporary_name, AtFlags::empty());
    }
    result
}

fn replace_json_at<T: Serialize>(parent: &OwnedFd, path: &Path, value: &T) -> PocResult<()> {
    let file_name = path.file_name().ok_or_else(|| {
        PocError::RecoveryRequired("activation control record has no file name".to_owned())
    })?;
    require_single_component_os("activation control-record name", file_name)?;
    let temporary_name = OsString::from(format!(
        ".{}.{}.tmp",
        file_name
            .to_str()
            .ok_or_else(|| PocError::RecoveryRequired(
                "activation control record has a non-UTF-8 name".to_owned()
            ))?,
        uuid::Uuid::new_v4()
    ));
    let temporary_path = path.with_file_name(&temporary_name);
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
            "create anchored activation replacement",
            &temporary_path,
            std::io::Error::from(error),
        )
    })?;
    let mut file = File::from(temporary_fd);
    let result = (|| {
        file.write_all(&bytes).map_err(|error| {
            PocError::io("write activation control record", &temporary_path, error)
        })?;
        file.sync_all().map_err(|error| {
            PocError::io("fsync activation control record", &temporary_path, error)
        })?;
        drop(file);
        rustix::fs::renameat(parent, &temporary_name, parent, file_name).map_err(|error| {
            PocError::io(
                "replace anchored activation control record",
                path,
                std::io::Error::from(error),
            )
        })?;
        fsync_activation_anchor(parent, path.parent().unwrap_or(path))
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, &temporary_name, AtFlags::empty());
    }
    result
}

fn read_optional_json<T: DeserializeOwned>(path: &Path) -> PocResult<Option<T>> {
    let Some(file) = open_optional_real_regular_file(path, "activation state")? else {
        return Ok(None);
    };
    let length = file
        .metadata()
        .map_err(|source| PocError::io("stat activation state", path, source))?
        .len();
    if length > MAX_ACTIVATION_JSON_BYTES {
        return Err(PocError::RecoveryRequired(format!(
            "activation state exceeds {MAX_ACTIVATION_JSON_BYTES} bytes: {}",
            path.display()
        )));
    }
    serde_json::from_reader(file)
        .map(Some)
        .map_err(PocError::from)
}

fn real_regular_file_exists(path: &Path, label: &str) -> PocResult<bool> {
    open_optional_real_regular_file(path, label).map(|file| file.is_some())
}

fn open_optional_real_regular_file(path: &Path, label: &str) -> PocResult<Option<File>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PocError::io("open activation state", path, source)),
    };
    let metadata = file
        .metadata()
        .map_err(|source| PocError::io("stat activation state", path, source))?;
    if !metadata.is_file() {
        return Err(PocError::RecoveryRequired(format!(
            "{label} is not a real regular file: {}",
            path.display()
        )));
    }
    Ok(Some(file))
}

fn real_directory_exists(path: &Path, label: &str) -> PocResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(PocError::RecoveryRequired(format!(
                    "{label} is not a real directory: {}",
                    path.display()
                )));
            }
            Ok(true)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PocError::io("stat activation directory", path, source)),
    }
}

fn validate_payload_handles(payloads: &[AllocationHandle]) -> PocResult<()> {
    open_validated_payload_handles(payloads).map(|_| ())
}

fn open_validated_payload_handles(
    payloads: &[AllocationHandle],
) -> PocResult<Vec<AllocationHandle>> {
    pin_validated_payload_allocations(payloads).map(|pinned| {
        pinned
            .into_iter()
            .map(|allocation| allocation.supplied)
            .collect()
    })
}

fn pin_validated_payload_allocations(
    payloads: &[AllocationHandle],
) -> PocResult<Vec<PinnedAllocation>> {
    payloads
        .iter()
        .map(|payload| {
            if payload.descriptor.schema_version != SCHEMA_VERSION {
                return Err(PocError::RecoveryRequired(format!(
                    "payload allocation {} has a mismatched schema",
                    payload.descriptor.allocation_id
                )));
            }
            let prefix = payload.allocation_root.parent().ok_or_else(|| {
                PocError::RecoveryRequired("payload allocation has no prefix root".to_owned())
            })?;
            let arena = prefix.parent().ok_or_else(|| {
                PocError::RecoveryRequired("payload allocation has no arena root".to_owned())
            })?;
            pin_activation_allocation(arena, payload)
        })
        .collect()
}

fn pinned_payload_handles(payloads: &[PinnedAllocation]) -> Vec<AllocationHandle> {
    payloads
        .iter()
        .map(|payload| payload.supplied.clone())
        .collect()
}

fn pinned_payload_identities(payloads: &[PinnedAllocation]) -> Vec<AllocationPhysicalIdentity> {
    payloads
        .iter()
        .map(|payload| payload.identity.clone())
        .collect()
}

fn revalidate_pinned_allocation(allocation: &PinnedAllocation) -> PocResult<()> {
    let prefix_name = allocation
        .supplied
        .descriptor
        .allocation_id
        .as_str()
        .get(..2)
        .ok_or_else(|| PocError::Integrity("activation allocation ID is too short".to_owned()))?;
    let prefix_path = allocation
        .supplied
        .allocation_root
        .parent()
        .ok_or_else(|| PocError::Integrity("activation allocation has no prefix".to_owned()))?;
    require_directory_entry_matches(
        &allocation.arena,
        OsStr::new(prefix_name),
        &allocation.prefix,
        prefix_path,
        "pinned activation allocation prefix",
    )?;
    let allocation_name = OsStr::new(allocation.supplied.descriptor.allocation_id.as_str());
    require_directory_entry_matches(
        &allocation.prefix,
        allocation_name,
        &allocation.allocation,
        &allocation.supplied.allocation_root,
        "pinned activation allocation",
    )?;
    for (name, directory, display, label) in [
        (
            OsStr::new("upper"),
            &allocation.upper,
            &allocation.supplied.upper_dir,
            "pinned activation upper",
        ),
        (
            OsStr::new("work"),
            &allocation.work,
            &allocation.supplied.work_dir,
            "pinned activation work",
        ),
        (
            OsStr::new("owner"),
            &allocation.owner,
            &allocation.supplied.owner_dir,
            "pinned activation owner",
        ),
    ] {
        require_directory_entry_matches(&allocation.allocation, name, directory, display, label)?;
    }
    Ok(())
}

fn selected_pinned_payload_owners(
    payloads: &[PinnedAllocation],
) -> PocResult<Vec<OwnerGeneration>> {
    payloads
        .iter()
        .map(|payload| {
            let _owner_lock = lock_pinned_allocation_owner(payload, &payload.supplied)?;
            crate::owner::selected_owner_locked_anchored(
                &payload.handle.allocation_root,
                &payload.owner,
            )?
            .ok_or_else(|| {
                PocError::RecoveryRequired(format!(
                    "selected payload {} has no durable owner selector",
                    payload.supplied.descriptor.allocation_id
                ))
            })
        })
        .collect()
}

fn selected_payload_owners(payloads: &[AllocationHandle]) -> PocResult<Vec<OwnerGeneration>> {
    payloads
        .iter()
        .map(|payload| {
            let _owner_lock = durable::FileLock::shared(&crate::owner::owner_lock_path(
                &payload.allocation_root,
            ))?;
            crate::owner::selected_owner_locked(&payload.allocation_root)?.ok_or_else(|| {
                PocError::RecoveryRequired(format!(
                    "selected payload {} has no durable owner selector",
                    payload.descriptor.allocation_id
                ))
            })
        })
        .collect()
}

fn allocation_root_for(arena: &Path, allocation_id: &AllocationId) -> PocResult<PathBuf> {
    let prefix = allocation_id.as_str().get(..2).ok_or_else(|| {
        PocError::Integrity(format!("AllocationId is too short: {allocation_id}"))
    })?;
    Ok(arena.join(prefix).join(allocation_id.as_str()))
}

fn read_real_directories(path: &Path, label: &str) -> PocResult<Vec<PathBuf>> {
    require_real_directory(path, label)?;
    let mut directories = Vec::new();
    let entries = std::fs::read_dir(path)
        .map_err(|source| PocError::io("read activation directory", path, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| PocError::io("read activation entry", path, source))?;
        let entry_path = entry.path();
        let metadata = std::fs::symlink_metadata(&entry_path)
            .map_err(|source| PocError::io("stat activation entry", &entry_path, source))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(PocError::RecoveryRequired(format!(
                "{label} contains a non-directory entry: {}",
                entry_path.display()
            )));
        }
        directories.push(entry_path);
    }
    directories.sort();
    Ok(directories)
}

fn require_real_directory(path: &Path, label: &str) -> PocResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat activation directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PocError::RecoveryRequired(format!(
            "{label} is not a real directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn require_real_directory_chain(path: &Path, label: &str) -> PocResult<()> {
    if !path.is_absolute() {
        return Err(PocError::RecoveryRequired(format!(
            "{label} must be an absolute no-symlink path: {}",
            path.display()
        )));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::RootDir => current.push(Path::new("/")),
            std::path::Component::Normal(component) => current.push(component),
            _ => {
                return Err(PocError::RecoveryRequired(format!(
                    "{label} is not a normalized absolute path: {}",
                    path.display()
                )));
            }
        }
        require_real_directory(&current, label)?;
    }
    Ok(())
}

fn require_real_regular_file(path: &Path, label: &str) -> PocResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat activation file", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PocError::RecoveryRequired(format!(
            "{label} is not a real regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_identifier_component(value: &str, label: &str) -> PocResult<()> {
    let mut components = Path::new(value).components();
    let valid = !value.is_empty()
        && matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none();
    if !valid {
        return Err(PocError::Integrity(format!(
            "{label} must be one normalized path component"
        )));
    }
    Ok(())
}

fn duration_ns(duration: Duration) -> PocResult<u64> {
    u64::try_from(duration.as_nanos())
        .map_err(|_| PocError::Integrity("activation readiness timeout exceeds u64 ns".to_owned()))
}

fn phase(name: &str, elapsed_ns: u64) -> ActivationPhaseSpan {
    ActivationPhaseSpan {
        phase: name.to_owned(),
        elapsed_ns,
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn validate_request(request: &ExactActivationRequest) -> PocResult<()> {
    validate_identifier_component(
        request.activation_operation_id.as_str(),
        "activation operation ID",
    )?;
    validate_identifier_component(
        request.allocation_operation_id.as_str(),
        "activation allocation operation ID",
    )?;
    duration_ns(request.readiness_timeout)?;
    request.recipe.validate()?;
    if request.readiness_path.as_os_str().is_empty()
        || request.readiness_path.is_absolute()
        || !request
            .readiness_path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return Err(PocError::Integrity(format!(
            "readiness path must be a normalized relative path: {}",
            request.readiness_path.display()
        )));
    }
    if request
        .readiness_contains
        .as_deref()
        .is_some_and(<[u8]>::is_empty)
    {
        return Err(PocError::Integrity(
            "readiness content sentinel must not be empty".to_owned(),
        ));
    }
    if request.selected_ref.roots != request.recipe.roots {
        return Err(PocError::Integrity(
            "selected ref roots differ from projection roots".to_owned(),
        ));
    }
    let supplied: BTreeSet<_> = request
        .payload_allocations
        .iter()
        .map(|allocation| &allocation.descriptor.allocation_id)
        .collect();
    if supplied.len() != request.payload_allocations.len() {
        return Err(PocError::Integrity(
            "activation supplied duplicate allocation handles".to_owned(),
        ));
    }
    for allocation_id in request.recipe.lower_allocation_ids_newest_first() {
        if !supplied.contains(allocation_id) {
            return Err(PocError::Integrity(format!(
                "projection allocation {allocation_id} is not supplied"
            )));
        }
    }
    validate_payload_handles(&request.payload_allocations)
}

fn directory_is_empty(path: &Path) -> PocResult<bool> {
    let mut entries = std::fs::read_dir(path)
        .map_err(|source| PocError::io("read activation upper", path, source))?;
    match entries.next() {
        None => Ok(true),
        Some(Ok(_)) => Ok(false),
        Some(Err(source)) => Err(PocError::io("read activation upper entry", path, source)),
    }
}

#[cfg(target_os = "linux")]
fn inherit_projection_root_metadata_anchored(
    source: &OwnedFd,
    source_path: &Path,
    target: &OwnedFd,
    target_path: &Path,
) -> PocResult<()> {
    let source_metadata = rustix::fs::fstat(source).map_err(|error| {
        PocError::io(
            "stat pinned selected projection root",
            source_path,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    if raw_mode_file_type(source_metadata.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::Directory
    {
        return Err(PocError::Integrity(format!(
            "pinned selected projection root is not a directory: {}",
            source_path.display()
        )));
    }
    let target_metadata = rustix::fs::fstat(target).map_err(|error| {
        PocError::io(
            "stat pinned fresh activation root",
            target_path,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    if raw_mode_file_type(target_metadata.st_mode as rustix::fs::RawMode)
        != rustix::fs::FileType::Directory
    {
        return Err(PocError::Integrity(format!(
            "pinned fresh activation root is not a directory: {}",
            target_path.display()
        )));
    }

    if source_metadata.st_uid != target_metadata.st_uid
        || source_metadata.st_gid != target_metadata.st_gid
    {
        // SAFETY: fstat returned owner IDs for an existing inode.
        let source_uid = unsafe { rustix::fs::Uid::from_raw(source_metadata.st_uid) };
        // SAFETY: fstat returned group IDs for an existing inode.
        let source_gid = unsafe { rustix::fs::Gid::from_raw(source_metadata.st_gid) };
        rustix::fs::fchown(target, Some(source_uid), Some(source_gid)).map_err(|error| {
            PocError::io(
                "inherit pinned activation root ownership",
                target_path,
                std::io::Error::from_raw_os_error(error.raw_os_error()),
            )
        })?;
    }
    rustix::fs::fchmod(
        target,
        rustix::fs::Mode::from_raw_mode((source_metadata.st_mode as rustix::fs::RawMode) & 0o7777),
    )
    .map_err(|error| {
        PocError::io(
            "inherit pinned activation root permissions",
            target_path,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    let timestamps = Timestamps {
        last_access: Timespec {
            tv_sec: source_metadata.st_atime as _,
            tv_nsec: source_metadata.st_atime_nsec as _,
        },
        last_modification: Timespec {
            tv_sec: source_metadata.st_mtime as _,
            tv_nsec: source_metadata.st_mtime_nsec as _,
        },
    };
    rustix::fs::futimens(target, &timestamps).map_err(|error| {
        PocError::io(
            "inherit pinned activation root timestamps",
            target_path,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    rustix::fs::fsync(target).map_err(|error| {
        PocError::io(
            "sync inherited pinned activation root",
            target_path,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })
}

#[cfg(not(target_os = "linux"))]
fn inherit_projection_root_metadata_anchored(
    _source: &OwnedFd,
    _source_path: &Path,
    _target: &OwnedFd,
    _target_path: &Path,
) -> PocResult<()> {
    Err(PocError::Unsupported(
        "pinned activation root metadata inheritance requires Linux".to_owned(),
    ))
}

/// Copies the selected projection root's semantic metadata onto a fresh upper.
pub fn inherit_projection_root_metadata(source: &Path, target: &Path) -> PocResult<()> {
    let source_metadata = std::fs::symlink_metadata(source)
        .map_err(|error| PocError::io("stat selected projection root", source, error))?;
    if !source_metadata.is_dir() {
        return Err(PocError::Integrity(format!(
            "selected projection root is not a directory: {}",
            source.display()
        )));
    }
    let target_metadata = std::fs::symlink_metadata(target)
        .map_err(|error| PocError::io("stat fresh activation root", target, error))?;
    if !target_metadata.is_dir() {
        return Err(PocError::Integrity(format!(
            "fresh activation root is not a directory: {}",
            target.display()
        )));
    }

    if source_metadata.uid() != target_metadata.uid()
        || source_metadata.gid() != target_metadata.gid()
    {
        std::os::unix::fs::chown(
            target,
            Some(source_metadata.uid()),
            Some(source_metadata.gid()),
        )
        .map_err(|error| PocError::io("inherit activation root ownership", target, error))?;
    }
    std::fs::set_permissions(
        target,
        std::fs::Permissions::from_mode(source_metadata.permissions().mode() & 0o7777),
    )
    .map_err(|error| PocError::io("inherit activation root permissions", target, error))?;
    let timestamps = Timestamps {
        last_access: Timespec {
            tv_sec: source_metadata.atime(),
            tv_nsec: source_metadata.atime_nsec(),
        },
        last_modification: Timespec {
            tv_sec: source_metadata.mtime(),
            tv_nsec: source_metadata.mtime_nsec(),
        },
    };
    rustix::fs::utimensat(CWD, target, &timestamps, AtFlags::empty()).map_err(|error| {
        PocError::io(
            "inherit activation root timestamps",
            target,
            std::io::Error::from_raw_os_error(error.raw_os_error()),
        )
    })?;
    File::open(target)
        .and_then(|directory| crate::durable::sync_all(&directory))
        .map_err(|error| PocError::io("sync inherited activation root", target, error))
}
