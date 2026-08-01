use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::durable::{read_json, replace_json, FileLock};
use crate::{
    AllocationDescriptor, AllocationHandle, AllocationId, DeletionCapability, MutableLease,
    OperationId, OwnerGeneration, OwnerSubject, OwnerTransitionRequest, PocError, PocResult,
    SessionId, WriterCapability, SCHEMA_VERSION,
};

const LEASE_FILE: &str = "LEASE";
const INITIAL_EPOCH: u64 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LeaseState {
    schema_version: u32,
    allocation_id: AllocationId,
    session_id: SessionId,
    lease_epoch: u64,
    owner_epoch: u64,
    writer_nonce: String,
    deleter_nonce: String,
    active: bool,
    operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prior_operation_id: Option<OperationId>,
    written_unix_ms: u64,
}

/// Durable proof that restart-only terminal recovery revoked both mutable
/// authorities without selecting a new owner or returning a capability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalLeaseFenceWitness {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub prior_operation_id: OperationId,
    pub allocation_id: AllocationId,
    pub session_id: SessionId,
    pub prior_lease_epoch: u64,
    pub prior_owner_epoch: u64,
    pub fenced_lease_epoch: u64,
    pub fenced_owner_epoch: u64,
    pub writer_revoked: bool,
    pub deleter_revoked: bool,
}

pub fn issue_workspace_lease(
    allocation: &AllocationHandle,
    session_id: SessionId,
    operation_id: &OperationId,
) -> PocResult<MutableLease> {
    validate_allocation_handle(allocation)?;
    let _lock = FileLock::exclusive(&crate::owner::owner_lock_path(&allocation.allocation_root))?;
    let selected = crate::owner::current_owner_locked(&allocation.allocation_root)?;
    let existing = read_lease_optional(&allocation.allocation_root)?;

    match selected {
        Some(owner) => {
            let state = existing.ok_or_else(|| {
                PocError::RecoveryRequired(format!(
                    "selected workspace owner has no lease for allocation {}",
                    allocation.descriptor.allocation_id
                ))
            })?;
            validate_lease_replay(&state, &owner, &session_id, operation_id)?;
            Ok(mutable_lease(&state))
        }
        None => {
            let state = match existing {
                Some(state) => {
                    validate_unselected_lease_recovery(
                        &state,
                        &allocation.descriptor.allocation_id,
                        &session_id,
                        operation_id,
                    )?;
                    state
                }
                None => {
                    let state = LeaseState {
                        schema_version: SCHEMA_VERSION,
                        allocation_id: allocation.descriptor.allocation_id.clone(),
                        session_id: session_id.clone(),
                        lease_epoch: INITIAL_EPOCH,
                        owner_epoch: INITIAL_EPOCH,
                        writer_nonce: Uuid::new_v4().to_string(),
                        deleter_nonce: Uuid::new_v4().to_string(),
                        active: true,
                        operation_id: operation_id.clone(),
                        prior_operation_id: None,
                        written_unix_ms: crate::unix_time_ms()?,
                    };
                    replace_json(&lease_path(&allocation.allocation_root), &state)?;
                    state
                }
            };
            let owner = workspace_owner(&state);
            crate::owner::initialize_workspace_owner_locked(&allocation.allocation_root, owner)?;
            Ok(mutable_lease(&state))
        }
    }
}

pub(crate) fn issue_workspace_lease_anchored(
    allocation: &AllocationHandle,
    owner: &std::os::fd::OwnedFd,
    session_id: SessionId,
    operation_id: &OperationId,
) -> PocResult<MutableLease> {
    crate::owner::with_pinned_owner_directory(&allocation.allocation_root, owner, || {
        issue_workspace_lease(allocation, session_id, operation_id)
    })
}

pub fn validate_writer(allocation_root: &Path, capability: &WriterCapability) -> PocResult<()> {
    validate_capability(
        allocation_root,
        &capability.allocation_id,
        &capability.session_id,
        capability.lease_epoch,
        capability.owner_epoch,
        &capability.nonce,
        CapabilityKind::Writer,
    )
}

pub(crate) fn validate_writer_anchored(
    allocation: &AllocationHandle,
    owner: &std::os::fd::OwnedFd,
    capability: &WriterCapability,
) -> PocResult<()> {
    crate::owner::with_pinned_owner_directory(&allocation.allocation_root, owner, || {
        validate_writer(&allocation.allocation_root, capability)
    })
}

/// Validate the exact durable allocation, lease, selected owner, and writer
/// nonce while the caller holds `owner/LOCK` exclusively.
///
/// Terminal session recovery uses this only as an identity proof.  It returns
/// no capability and performs no lease or owner transition.
pub(crate) fn validate_terminal_session_locked(
    allocation: &AllocationHandle,
    lease: &MutableLease,
) -> PocResult<()> {
    validate_allocation_handle(allocation)?;
    if lease.schema_version != SCHEMA_VERSION
        || lease.allocation_id != allocation.descriptor.allocation_id
        || lease.writer.allocation_id != lease.allocation_id
        || lease.writer.session_id != lease.session_id
        || lease.writer.lease_epoch != lease.lease_epoch
        || lease.writer.owner_epoch != lease.owner_epoch
        || lease.deleter.allocation_id != lease.allocation_id
        || lease.deleter.session_id != lease.session_id
        || lease.deleter.lease_epoch != lease.lease_epoch
        || lease.deleter.owner_epoch != lease.owner_epoch
    {
        return Err(PocError::RecoveryRequired(
            "terminal session lease object is internally inconsistent".to_owned(),
        ));
    }
    validate_capability_locked(
        &allocation.allocation_root,
        &lease.allocation_id,
        &lease.session_id,
        lease.lease_epoch,
        lease.owner_epoch,
        &lease.writer.nonce,
        CapabilityKind::Writer,
    )
    .map_err(|error| {
        PocError::RecoveryRequired(format!(
            "terminal session durable allocation/lease/owner tuple is not exact: {error}"
        ))
    })
}

/// Permanently revoke the exact session's writer and deleter while the caller
/// holds `owner/LOCK` exclusively.  A response-loss replay for the same
/// operation returns the identical witness; any other inactive state or epoch
/// transition fails closed.
pub(crate) fn fence_terminal_session_locked(
    allocation: &AllocationHandle,
    lease: &MutableLease,
    operation_id: &OperationId,
) -> PocResult<TerminalLeaseFenceWitness> {
    validate_terminal_lease_object(allocation, lease)?;
    let selected = exact_terminal_owner(allocation, lease)?;
    let before = read_lease(&allocation.allocation_root)?;
    if before.writer_nonce != lease.writer.nonce || before.deleter_nonce != lease.deleter.nonce {
        return Err(PocError::RecoveryRequired(
            "terminal lease capability nonces differ from the supplied exact lease".to_owned(),
        ));
    }
    fence_or_reaudit_terminal_session_locked(
        allocation,
        &lease.session_id,
        lease.lease_epoch,
        lease.owner_epoch,
        &selected.operation_id,
        operation_id,
    )
}

/// Fence or replay a terminal lease using only durable identity that survives
/// process restart.  `SESSION.json` supplies the session and prior epochs;
/// the caller supplies the immutable operation that originally issued the
/// selected WorkspaceOwned owner.  No capability nonce is needed or exposed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fence_or_reaudit_terminal_session_anchored_locked(
    allocation: &AllocationHandle,
    owner: &std::os::fd::OwnedFd,
    session_id: &SessionId,
    prior_lease_epoch: u64,
    prior_owner_epoch: u64,
    prior_operation_id: &OperationId,
    fence_operation_id: &OperationId,
) -> PocResult<TerminalLeaseFenceWitness> {
    crate::owner::with_pinned_owner_directory(&allocation.allocation_root, owner, || {
        fence_or_reaudit_terminal_session_locked(
            allocation,
            session_id,
            prior_lease_epoch,
            prior_owner_epoch,
            prior_operation_id,
            fence_operation_id,
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fence_or_reaudit_terminal_session_locked(
    allocation: &AllocationHandle,
    session_id: &SessionId,
    prior_lease_epoch: u64,
    prior_owner_epoch: u64,
    prior_operation_id: &OperationId,
    fence_operation_id: &OperationId,
) -> PocResult<TerminalLeaseFenceWitness> {
    validate_allocation_handle(allocation)?;
    exact_terminal_owner_tuple(
        allocation,
        session_id,
        prior_lease_epoch,
        prior_owner_epoch,
        prior_operation_id,
    )?;
    let expected_fenced_lease_epoch = prior_lease_epoch
        .checked_add(1)
        .ok_or_else(|| PocError::RecoveryRequired("terminal lease epoch exhausted".to_owned()))?;
    let expected_fenced_owner_epoch = prior_owner_epoch
        .checked_add(1)
        .ok_or_else(|| PocError::RecoveryRequired("terminal owner epoch exhausted".to_owned()))?;
    let mut state = read_lease(&allocation.allocation_root)?;
    if state.active {
        if state.schema_version != SCHEMA_VERSION
            || state.allocation_id != allocation.descriptor.allocation_id
            || state.session_id != *session_id
            || state.lease_epoch != prior_lease_epoch
            || state.owner_epoch != prior_owner_epoch
            || state.operation_id != *prior_operation_id
            || state.prior_operation_id.is_some()
        {
            return Err(PocError::RecoveryRequired(
                "active terminal lease differs from the durable pre-fence tuple".to_owned(),
            ));
        }
        state.lease_epoch = expected_fenced_lease_epoch;
        state.owner_epoch = expected_fenced_owner_epoch;
        state.active = false;
        state.prior_operation_id = Some(prior_operation_id.clone());
        state.operation_id = fence_operation_id.clone();
        state.written_unix_ms = crate::unix_time_ms()?;
        replace_json(&lease_path(&allocation.allocation_root), &state)?;
    }
    reaudit_terminal_session_fence_tuple_locked(
        allocation,
        session_id,
        prior_lease_epoch,
        prior_owner_epoch,
        prior_operation_id,
        fence_operation_id,
    )
}

/// Fence activation setup using only restart-safe identity.  This additionally
/// handles the crash window where `LEASE` became durable before `CURRENT`
/// selected its WorkspaceOwned owner.  The caller must hold `owner/LOCK`
/// exclusively; no capability or nonce is returned.
pub(crate) fn fence_or_reaudit_private_activation_anchored_locked(
    allocation: &AllocationHandle,
    owner: &std::os::fd::OwnedFd,
    expected_session_id: &SessionId,
    prior_operation_id: &OperationId,
    fence_operation_id: &OperationId,
) -> PocResult<Option<TerminalLeaseFenceWitness>> {
    crate::owner::with_pinned_owner_directory(&allocation.allocation_root, owner, || {
        fence_or_reaudit_private_activation_locked(
            allocation,
            expected_session_id,
            prior_operation_id,
            fence_operation_id,
        )
    })
}

pub(crate) fn fence_or_reaudit_private_activation_locked(
    allocation: &AllocationHandle,
    expected_session_id: &SessionId,
    prior_operation_id: &OperationId,
    fence_operation_id: &OperationId,
) -> PocResult<Option<TerminalLeaseFenceWitness>> {
    validate_allocation_handle(allocation)?;
    require_identity_component("activation session", expected_session_id.as_str())?;
    require_identity_component("prior activation operation", prior_operation_id.as_str())?;
    require_identity_component("activation fence operation", fence_operation_id.as_str())?;
    let selected = crate::owner::selected_owner_locked(&allocation.allocation_root)?;
    let Some(mut state) = read_lease_optional(&allocation.allocation_root)? else {
        if selected.is_some() {
            return Err(PocError::RecoveryRequired(
                "selected activation owner has no durable lease".to_owned(),
            ));
        }
        return Ok(None);
    };
    validate_private_activation_lease_shape(&state, allocation, expected_session_id)?;

    if let Some(owner) = selected {
        let prior_lease_epoch = match &owner.subject {
            OwnerSubject::WorkspaceOwned {
                session_id,
                lease_epoch,
            } if session_id == expected_session_id => *lease_epoch,
            _ => {
                return Err(PocError::RecoveryRequired(
                    "activation owner is not the exact expected private workspace".to_owned(),
                ));
            }
        };
        if owner.schema_version != SCHEMA_VERSION
            || owner.allocation_id != allocation.descriptor.allocation_id
            || owner.operation_id != *prior_operation_id
        {
            return Err(PocError::RecoveryRequired(
                "activation owner differs from the expected pre-fence identity".to_owned(),
            ));
        }
        return fence_or_reaudit_terminal_session_locked(
            allocation,
            expected_session_id,
            prior_lease_epoch,
            owner.owner_epoch,
            prior_operation_id,
            fence_operation_id,
        )
        .map(Some);
    }

    let fenced_epoch = INITIAL_EPOCH.checked_add(1).ok_or_else(|| {
        PocError::RecoveryRequired("initial activation epoch exhausted".to_owned())
    })?;
    if state.active {
        if state.operation_id != *prior_operation_id
            || state.lease_epoch != INITIAL_EPOCH
            || state.owner_epoch != INITIAL_EPOCH
            || state.prior_operation_id.is_some()
        {
            return Err(PocError::RecoveryRequired(
                "unselected activation lease is not the exact initial durable lease".to_owned(),
            ));
        }
        state.lease_epoch = fenced_epoch;
        state.owner_epoch = fenced_epoch;
        state.active = false;
        state.prior_operation_id = Some(prior_operation_id.clone());
        state.operation_id = fence_operation_id.clone();
        state.written_unix_ms = crate::unix_time_ms()?;
        replace_json(&lease_path(&allocation.allocation_root), &state)?;
    }
    if state.active
        || state.operation_id != *fence_operation_id
        || state.prior_operation_id.as_ref() != Some(prior_operation_id)
        || state.lease_epoch != fenced_epoch
        || state.owner_epoch != fenced_epoch
    {
        return Err(PocError::RecoveryRequired(
            "unselected activation lease differs from the exact terminal fence".to_owned(),
        ));
    }
    Ok(Some(TerminalLeaseFenceWitness {
        schema_version: SCHEMA_VERSION,
        operation_id: fence_operation_id.clone(),
        prior_operation_id: prior_operation_id.clone(),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        session_id: expected_session_id.clone(),
        prior_lease_epoch: INITIAL_EPOCH,
        prior_owner_epoch: INITIAL_EPOCH,
        fenced_lease_epoch: fenced_epoch,
        fenced_owner_epoch: fenced_epoch,
        writer_revoked: true,
        deleter_revoked: true,
    }))
}

fn validate_private_activation_lease_shape(
    state: &LeaseState,
    allocation: &AllocationHandle,
    expected_session_id: &SessionId,
) -> PocResult<()> {
    let nonces_are_valid = Uuid::parse_str(&state.writer_nonce).is_ok()
        && Uuid::parse_str(&state.deleter_nonce).is_ok()
        && state.writer_nonce != state.deleter_nonce;
    if state.schema_version == SCHEMA_VERSION
        && state.allocation_id == allocation.descriptor.allocation_id
        && state.session_id == *expected_session_id
        && nonces_are_valid
    {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "private activation lease has a foreign or corrupt durable identity".to_owned(),
        ))
    }
}

fn require_identity_component(label: &str, value: &str) -> PocResult<()> {
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

/// Re-read and prove the exact terminal fence without changing durable state.
/// The historical selected workspace owner must still identify the pre-fence
/// session, while the lease itself must be inactive at exactly the next lease
/// and owner epochs for the same terminal operation.
pub(crate) fn reaudit_terminal_session_fence_locked(
    allocation: &AllocationHandle,
    lease: &MutableLease,
    operation_id: &OperationId,
) -> PocResult<TerminalLeaseFenceWitness> {
    validate_terminal_lease_object(allocation, lease)?;
    let selected = exact_terminal_owner(allocation, lease)?;
    let state = read_lease(&allocation.allocation_root)?;
    if state.writer_nonce != lease.writer.nonce || state.deleter_nonce != lease.deleter.nonce {
        return Err(PocError::RecoveryRequired(
            "terminal fence capability nonces differ from the supplied exact lease".to_owned(),
        ));
    }
    reaudit_terminal_session_fence_tuple_locked(
        allocation,
        &lease.session_id,
        lease.lease_epoch,
        lease.owner_epoch,
        &selected.operation_id,
        operation_id,
    )
}

/// Read-only restart audit for callers that possess only the durable session
/// tuple and original owner operation, not an in-memory `MutableLease`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reaudit_terminal_session_fence_tuple_anchored_locked(
    allocation: &AllocationHandle,
    owner: &std::os::fd::OwnedFd,
    session_id: &SessionId,
    prior_lease_epoch: u64,
    prior_owner_epoch: u64,
    prior_operation_id: &OperationId,
    fence_operation_id: &OperationId,
) -> PocResult<TerminalLeaseFenceWitness> {
    crate::owner::with_pinned_owner_directory(&allocation.allocation_root, owner, || {
        reaudit_terminal_session_fence_tuple_locked(
            allocation,
            session_id,
            prior_lease_epoch,
            prior_owner_epoch,
            prior_operation_id,
            fence_operation_id,
        )
    })
}

pub(crate) fn reaudit_terminal_session_fence_tuple_locked(
    allocation: &AllocationHandle,
    session_id: &SessionId,
    prior_lease_epoch: u64,
    prior_owner_epoch: u64,
    prior_operation_id: &OperationId,
    fence_operation_id: &OperationId,
) -> PocResult<TerminalLeaseFenceWitness> {
    validate_allocation_handle(allocation)?;
    exact_terminal_owner_tuple(
        allocation,
        session_id,
        prior_lease_epoch,
        prior_owner_epoch,
        prior_operation_id,
    )?;
    let expected_fenced_lease_epoch = prior_lease_epoch
        .checked_add(1)
        .ok_or_else(|| PocError::RecoveryRequired("terminal lease epoch exhausted".to_owned()))?;
    let expected_fenced_owner_epoch = prior_owner_epoch
        .checked_add(1)
        .ok_or_else(|| PocError::RecoveryRequired("terminal owner epoch exhausted".to_owned()))?;
    let state = read_lease(&allocation.allocation_root)?;
    if state.schema_version != SCHEMA_VERSION
        || state.allocation_id != allocation.descriptor.allocation_id
        || state.session_id != *session_id
        || state.lease_epoch != expected_fenced_lease_epoch
        || state.owner_epoch != expected_fenced_owner_epoch
        || state.operation_id != *fence_operation_id
        || state.prior_operation_id.as_ref() != Some(prior_operation_id)
        || state.active
    {
        return Err(PocError::RecoveryRequired(
            "durable terminal lease fence differs from the exact requested transition".to_owned(),
        ));
    }
    Ok(TerminalLeaseFenceWitness {
        schema_version: SCHEMA_VERSION,
        operation_id: fence_operation_id.clone(),
        prior_operation_id: prior_operation_id.clone(),
        allocation_id: allocation.descriptor.allocation_id.clone(),
        session_id: session_id.clone(),
        prior_lease_epoch,
        prior_owner_epoch,
        fenced_lease_epoch: expected_fenced_lease_epoch,
        fenced_owner_epoch: expected_fenced_owner_epoch,
        writer_revoked: true,
        deleter_revoked: true,
    })
}

pub fn validate_deleter(allocation_root: &Path, capability: &DeletionCapability) -> PocResult<()> {
    let _lock = FileLock::shared(&crate::owner::owner_lock_path(allocation_root))?;
    validate_deleter_locked(allocation_root, capability)
}

/// Validates the immutable identity of the active MPLA lease for the one
/// storage-administrator path.  This intentionally exposes no capability
/// nonce: callers can prove only the currently selected session, never gain a
/// general writer or deleter authority.
pub fn validate_active_storage_admin_lease(
    allocation_root: &Path,
    allocation_id: &AllocationId,
    session_id: &SessionId,
    lease_id: &str,
    lease_epoch: u64,
) -> PocResult<()> {
    let _lock = FileLock::shared(&crate::owner::owner_lock_path(allocation_root))?;
    let descriptor: AllocationDescriptor = read_json(&allocation_root.join("ALLOCATION.json"))?;
    let state = read_lease(allocation_root)?;
    let selected = crate::owner::selected_owner_locked(allocation_root)?;
    let owner_matches = matches!(
        selected.as_ref().map(|owner| &owner.subject),
        Some(OwnerSubject::WorkspaceOwned {
            session_id: owner_session,
            lease_epoch: owner_lease_epoch,
        }) if owner_session == session_id && *owner_lease_epoch == lease_epoch
    );
    if descriptor.schema_version == SCHEMA_VERSION
        && descriptor.allocation_id == *allocation_id
        && state.schema_version == SCHEMA_VERSION
        && state.allocation_id == *allocation_id
        && state.session_id == *session_id
        && state.operation_id.as_str() == lease_id
        && state.lease_epoch == lease_epoch
        && state.active
        && owner_matches
    {
        return Ok(());
    }
    Err(PocError::RecoveryRequired(
        "storage-admin request is not bound to the selected active MPLA lease".to_owned(),
    ))
}

pub(crate) fn validate_deleter_locked(
    allocation_root: &Path,
    capability: &DeletionCapability,
) -> PocResult<()> {
    validate_capability_locked(
        allocation_root,
        &capability.allocation_id,
        &capability.session_id,
        capability.lease_epoch,
        capability.owner_epoch,
        &capability.nonce,
        CapabilityKind::Deleter,
    )
}

pub(crate) fn fence_for_adoption_locked(
    allocation_root: &Path,
    request: &OwnerTransitionRequest,
) -> PocResult<()> {
    let mut state = read_lease(allocation_root)?;
    if state.schema_version != SCHEMA_VERSION || state.allocation_id != request.allocation_id {
        return Err(PocError::RecoveryRequired(
            "lease identity differs from adoption request".to_owned(),
        ));
    }
    if !state.active {
        let expected_fenced_epoch =
            request.expected_lease_epoch.checked_add(1).ok_or_else(|| {
                PocError::RecoveryRequired("lease epoch exhausted during adoption".to_owned())
            })?;
        let expected_owner_epoch =
            request.expected_owner_epoch.checked_add(1).ok_or_else(|| {
                PocError::RecoveryRequired("owner epoch exhausted during adoption".to_owned())
            })?;
        if state.operation_id == request.operation_id
            && state.session_id == request.session_id
            && state.lease_epoch == expected_fenced_epoch
            && state.owner_epoch == expected_owner_epoch
        {
            return Ok(());
        }
        return Err(PocError::OwnerConflict(
            "allocation lease was fenced by another operation".to_owned(),
        ));
    }
    if state.session_id != request.session_id
        || state.lease_epoch != request.expected_lease_epoch
        || state.owner_epoch != request.expected_owner_epoch
    {
        return Err(PocError::OwnerConflict(
            "active lease does not match adoption compare tuple".to_owned(),
        ));
    }
    state.lease_epoch = state
        .lease_epoch
        .checked_add(1)
        .ok_or_else(|| PocError::RecoveryRequired("lease epoch exhausted".to_owned()))?;
    state.owner_epoch = state
        .owner_epoch
        .checked_add(1)
        .ok_or_else(|| PocError::RecoveryRequired("owner epoch exhausted".to_owned()))?;
    state.active = false;
    state.prior_operation_id = Some(state.operation_id.clone());
    state.operation_id = request.operation_id.clone();
    state.written_unix_ms = crate::unix_time_ms()?;
    replace_json(&lease_path(allocation_root), &state)
}

#[derive(Clone, Copy)]
enum CapabilityKind {
    Writer,
    Deleter,
}

impl CapabilityKind {
    fn label(self) -> &'static str {
        match self {
            Self::Writer => "writer",
            Self::Deleter => "deleter",
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_capability(
    allocation_root: &Path,
    allocation_id: &AllocationId,
    session_id: &SessionId,
    lease_epoch: u64,
    owner_epoch: u64,
    nonce: &str,
    kind: CapabilityKind,
) -> PocResult<()> {
    let _lock = FileLock::shared(&crate::owner::owner_lock_path(allocation_root))?;
    validate_capability_locked(
        allocation_root,
        allocation_id,
        session_id,
        lease_epoch,
        owner_epoch,
        nonce,
        kind,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_capability_locked(
    allocation_root: &Path,
    allocation_id: &AllocationId,
    session_id: &SessionId,
    lease_epoch: u64,
    owner_epoch: u64,
    nonce: &str,
    kind: CapabilityKind,
) -> PocResult<()> {
    let descriptor: AllocationDescriptor = read_json(&allocation_root.join("ALLOCATION.json"))?;
    let state = read_lease(allocation_root)?;
    let selected = crate::owner::selected_owner_locked(allocation_root)?;

    let expected_epoch = state.lease_epoch;
    let nonce_matches = match kind {
        CapabilityKind::Writer => state.writer_nonce == nonce,
        CapabilityKind::Deleter => state.deleter_nonce == nonce,
    };
    let owner_matches = matches!(
        selected.as_ref().map(|owner| &owner.subject),
        Some(OwnerSubject::WorkspaceOwned {
            session_id: owner_session,
            lease_epoch: owner_lease_epoch,
        }) if owner_session == session_id && *owner_lease_epoch == lease_epoch
    );
    if descriptor.schema_version == SCHEMA_VERSION
        && descriptor.allocation_id == *allocation_id
        && state.schema_version == SCHEMA_VERSION
        && state.allocation_id == *allocation_id
        && state.session_id == *session_id
        && state.active
        && state.lease_epoch == lease_epoch
        && state.owner_epoch == owner_epoch
        && selected
            .as_ref()
            .is_some_and(|owner| owner.owner_epoch == owner_epoch)
        && owner_matches
        && nonce_matches
    {
        return Ok(());
    }
    Err(PocError::StaleCapability {
        capability: kind.label(),
        allocation_id: allocation_id.to_string(),
        expected_epoch,
        observed_epoch: lease_epoch,
    })
}

fn validate_allocation_handle(allocation: &AllocationHandle) -> PocResult<()> {
    let descriptor: AllocationDescriptor =
        read_json(&allocation.allocation_root.join("ALLOCATION.json"))?;
    if descriptor != allocation.descriptor
        || allocation.upper_dir != allocation.allocation_root.join("upper")
        || allocation.work_dir != allocation.allocation_root.join("work")
        || allocation.owner_dir != allocation.allocation_root.join("owner")
    {
        return Err(PocError::Integrity(
            "allocation handle does not match permanent allocation metadata".to_owned(),
        ));
    }
    Ok(())
}

fn validate_terminal_lease_object(
    allocation: &AllocationHandle,
    lease: &MutableLease,
) -> PocResult<()> {
    validate_allocation_handle(allocation)?;
    if lease.schema_version != SCHEMA_VERSION
        || lease.allocation_id != allocation.descriptor.allocation_id
        || lease.writer.allocation_id != lease.allocation_id
        || lease.writer.session_id != lease.session_id
        || lease.writer.lease_epoch != lease.lease_epoch
        || lease.writer.owner_epoch != lease.owner_epoch
        || lease.deleter.allocation_id != lease.allocation_id
        || lease.deleter.session_id != lease.session_id
        || lease.deleter.lease_epoch != lease.lease_epoch
        || lease.deleter.owner_epoch != lease.owner_epoch
    {
        return Err(PocError::RecoveryRequired(
            "terminal session lease object is internally inconsistent".to_owned(),
        ));
    }
    Ok(())
}

fn exact_terminal_owner(
    allocation: &AllocationHandle,
    lease: &MutableLease,
) -> PocResult<OwnerGeneration> {
    exact_terminal_owner_tuple(
        allocation,
        &lease.session_id,
        lease.lease_epoch,
        lease.owner_epoch,
        &crate::owner::selected_owner_locked(&allocation.allocation_root)?
            .ok_or_else(|| {
                PocError::RecoveryRequired("terminal session has no selected owner".to_owned())
            })?
            .operation_id,
    )
}

fn exact_terminal_owner_tuple(
    allocation: &AllocationHandle,
    session_id: &SessionId,
    lease_epoch: u64,
    owner_epoch: u64,
    prior_operation_id: &OperationId,
) -> PocResult<OwnerGeneration> {
    let selected =
        crate::owner::selected_owner_locked(&allocation.allocation_root)?.ok_or_else(|| {
            PocError::RecoveryRequired("terminal session has no selected owner".to_owned())
        })?;
    if selected.schema_version == SCHEMA_VERSION
        && selected.allocation_id == allocation.descriptor.allocation_id
        && selected.owner_epoch == owner_epoch
        && selected.operation_id == *prior_operation_id
        && matches!(
            &selected.subject,
            OwnerSubject::WorkspaceOwned {
                session_id: owner_session_id,
                lease_epoch: owner_lease_epoch,
            } if owner_session_id == session_id && *owner_lease_epoch == lease_epoch
        )
    {
        Ok(selected)
    } else {
        Err(PocError::RecoveryRequired(
            "selected owner differs from the exact pre-fence session tuple".to_owned(),
        ))
    }
}

fn validate_lease_replay(
    state: &LeaseState,
    owner: &OwnerGeneration,
    session_id: &SessionId,
    operation_id: &OperationId,
) -> PocResult<()> {
    let exact = state.schema_version == SCHEMA_VERSION
        && state.active
        && state.session_id == *session_id
        && state.operation_id == *operation_id
        && state.owner_epoch == owner.owner_epoch
        && owner.operation_id == *operation_id
        && owner.allocation_id == state.allocation_id
        && matches!(
            &owner.subject,
            OwnerSubject::WorkspaceOwned {
                session_id: owner_session,
                lease_epoch,
            } if owner_session == session_id && *lease_epoch == state.lease_epoch
        );
    if exact {
        Ok(())
    } else {
        Err(PocError::OwnerConflict(
            "workspace lease already belongs to another session or operation".to_owned(),
        ))
    }
}

fn validate_unselected_lease_recovery(
    state: &LeaseState,
    allocation_id: &AllocationId,
    session_id: &SessionId,
    operation_id: &OperationId,
) -> PocResult<()> {
    if state.schema_version == SCHEMA_VERSION
        && state.allocation_id == *allocation_id
        && state.session_id == *session_id
        && state.operation_id == *operation_id
        && state.active
        && state.lease_epoch == INITIAL_EPOCH
        && state.owner_epoch == INITIAL_EPOCH
    {
        Ok(())
    } else {
        Err(PocError::RecoveryRequired(
            "unselected allocation contains a conflicting durable lease".to_owned(),
        ))
    }
}

fn workspace_owner(state: &LeaseState) -> OwnerGeneration {
    OwnerGeneration {
        schema_version: SCHEMA_VERSION,
        allocation_id: state.allocation_id.clone(),
        owner_epoch: state.owner_epoch,
        previous_owner_epoch: None,
        subject: OwnerSubject::WorkspaceOwned {
            session_id: state.session_id.clone(),
            lease_epoch: state.lease_epoch,
        },
        operation_id: state.operation_id.clone(),
        written_unix_ms: state.written_unix_ms,
    }
}

fn mutable_lease(state: &LeaseState) -> MutableLease {
    MutableLease {
        schema_version: SCHEMA_VERSION,
        allocation_id: state.allocation_id.clone(),
        session_id: state.session_id.clone(),
        lease_epoch: state.lease_epoch,
        owner_epoch: state.owner_epoch,
        writer: WriterCapability {
            allocation_id: state.allocation_id.clone(),
            session_id: state.session_id.clone(),
            lease_epoch: state.lease_epoch,
            owner_epoch: state.owner_epoch,
            nonce: state.writer_nonce.clone(),
        },
        deleter: DeletionCapability {
            allocation_id: state.allocation_id.clone(),
            session_id: state.session_id.clone(),
            lease_epoch: state.lease_epoch,
            owner_epoch: state.owner_epoch,
            nonce: state.deleter_nonce.clone(),
        },
    }
}

fn read_lease(allocation_root: &Path) -> PocResult<LeaseState> {
    read_json(&lease_path(allocation_root))
}

fn read_lease_optional(allocation_root: &Path) -> PocResult<Option<LeaseState>> {
    let path = lease_path(allocation_root);
    match read_json(&path) {
        Ok(state) => Ok(Some(state)),
        Err(PocError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn lease_path(allocation_root: &Path) -> std::path::PathBuf {
    crate::owner::owner_dir(allocation_root).join(LEASE_FILE)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn terminal_fence_replays_exactly_and_revokes_both_capabilities() {
        let root = TestDirectory::new("terminal-lease-fence");
        let prior_operation = OperationId::from_string("terminal-lease-prior");
        let fence_operation = OperationId::from_string("terminal-lease-recovery");
        let allocation =
            crate::allocation::create_allocation(&root.0.join("allocations"), &prior_operation)
                .expect("create allocation");
        let lease = issue_workspace_lease(
            &allocation,
            SessionId::from_string("terminal-lease-session"),
            &prior_operation,
        )
        .expect("issue lease");
        let control_root = root.0.join("control");
        let prepared = crate::prepare_external_session(&control_root, &allocation, &lease)
            .expect("prepare session");
        std::fs::write(prepared.session_dir().join("MOUNT.json"), b"{}")
            .expect("record mounted-session marker");

        let fence = {
            let _lock =
                FileLock::exclusive(&crate::owner::owner_lock_path(&allocation.allocation_root))
                    .expect("lock owner");
            let first = fence_terminal_session_locked(&allocation, &lease, &fence_operation)
                .expect("fence terminal lease");
            let replay = fence_terminal_session_locked(&allocation, &lease, &fence_operation)
                .expect("replay terminal fence");
            let restart_reaudit = reaudit_terminal_session_fence_tuple_locked(
                &allocation,
                &lease.session_id,
                lease.lease_epoch,
                lease.owner_epoch,
                &prior_operation,
                &fence_operation,
            )
            .expect("reaudit terminal fence without capabilities");
            assert_eq!(first, replay);
            assert_eq!(first, restart_reaudit);
            assert!(matches!(
                fence_terminal_session_locked(
                    &allocation,
                    &lease,
                    &OperationId::from_string("different-terminal-operation"),
                ),
                Err(PocError::RecoveryRequired(_))
            ));
            first
        };

        assert_eq!(fence.prior_lease_epoch, lease.lease_epoch);
        assert_eq!(fence.fenced_lease_epoch, lease.lease_epoch + 1);
        assert_eq!(fence.prior_owner_epoch, lease.owner_epoch);
        assert_eq!(fence.fenced_owner_epoch, lease.owner_epoch + 1);
        assert!(fence.writer_revoked);
        assert!(fence.deleter_revoked);
        assert!(matches!(
            validate_writer(&allocation.allocation_root, &lease.writer),
            Err(PocError::StaleCapability { .. })
        ));
        assert!(matches!(
            validate_deleter(&allocation.allocation_root, &lease.deleter),
            Err(PocError::StaleCapability { .. })
        ));
        assert!(matches!(
            issue_workspace_lease(&allocation, lease.session_id.clone(), &prior_operation),
            Err(PocError::OwnerConflict(_))
        ));
        assert!(matches!(
            crate::prepare_external_session(&control_root, &allocation, &lease),
            Err(PocError::RecoveryRequired(_))
        ));
    }

    #[test]
    fn terminal_fence_reaudit_rejects_corrupt_inactive_state() {
        let root = TestDirectory::new("terminal-lease-corrupt-fence");
        let prior_operation = OperationId::from_string("terminal-corrupt-prior");
        let fence_operation = OperationId::from_string("terminal-corrupt-recovery");
        let allocation =
            crate::allocation::create_allocation(&root.0.join("allocations"), &prior_operation)
                .expect("create allocation");
        let lease = issue_workspace_lease(
            &allocation,
            SessionId::from_string("terminal-corrupt-session"),
            &prior_operation,
        )
        .expect("issue lease");
        let _lock =
            FileLock::exclusive(&crate::owner::owner_lock_path(&allocation.allocation_root))
                .expect("lock owner");
        fence_terminal_session_locked(&allocation, &lease, &fence_operation)
            .expect("fence terminal lease");
        let mut corrupt = read_lease(&allocation.allocation_root).expect("read fenced lease");
        corrupt.lease_epoch += 1;
        replace_json(&lease_path(&allocation.allocation_root), &corrupt)
            .expect("write corrupt inactive lease");

        assert!(matches!(
            reaudit_terminal_session_fence_tuple_locked(
                &allocation,
                &lease.session_id,
                lease.lease_epoch,
                lease.owner_epoch,
                &prior_operation,
                &fence_operation,
            ),
            Err(PocError::RecoveryRequired(_))
        ));
    }

    #[test]
    fn private_activation_fence_handles_unselected_lease_crash_window() {
        let root = TestDirectory::new("private-activation-lease-fence");
        let allocation_operation = OperationId::from_string("private-allocation-create");
        let prior_operation = OperationId::from_string("private-activation-prior");
        let fence_operation = OperationId::from_string("private-activation-recovery");
        let session_id = SessionId::from_string("private-activation-session");
        let allocation = crate::allocation::create_allocation(
            &root.0.join("allocations"),
            &allocation_operation,
        )
        .expect("create allocation");
        let _lock =
            FileLock::exclusive(&crate::owner::owner_lock_path(&allocation.allocation_root))
                .expect("lock owner");

        assert_eq!(
            fence_or_reaudit_private_activation_locked(
                &allocation,
                &session_id,
                &prior_operation,
                &fence_operation,
            )
            .expect("empty allocation is not fenced"),
            None
        );

        let initial = LeaseState {
            schema_version: SCHEMA_VERSION,
            allocation_id: allocation.descriptor.allocation_id.clone(),
            session_id: session_id.clone(),
            lease_epoch: INITIAL_EPOCH,
            owner_epoch: INITIAL_EPOCH,
            writer_nonce: Uuid::new_v4().to_string(),
            deleter_nonce: Uuid::new_v4().to_string(),
            active: true,
            operation_id: prior_operation.clone(),
            prior_operation_id: None,
            written_unix_ms: crate::unix_time_ms().expect("timestamp"),
        };
        let prior_lease = mutable_lease(&initial);
        replace_json(&lease_path(&allocation.allocation_root), &initial)
            .expect("persist lease without owner selector");

        let first = fence_or_reaudit_private_activation_locked(
            &allocation,
            &session_id,
            &prior_operation,
            &fence_operation,
        )
        .expect("fence unselected activation lease")
        .expect("fence witness");
        let replay = fence_or_reaudit_private_activation_locked(
            &allocation,
            &session_id,
            &prior_operation,
            &fence_operation,
        )
        .expect("replay unselected activation fence")
        .expect("replay witness");

        assert_eq!(first, replay);
        assert_eq!(first.prior_operation_id, prior_operation);
        assert_eq!(first.operation_id, fence_operation);
        assert_eq!(first.prior_lease_epoch, INITIAL_EPOCH);
        assert_eq!(first.fenced_lease_epoch, INITIAL_EPOCH + 1);
        assert!(
            crate::owner::selected_owner_locked(&allocation.allocation_root)
                .expect("read owner")
                .is_none()
        );
        assert!(matches!(
            fence_or_reaudit_private_activation_locked(
                &allocation,
                &session_id,
                &OperationId::from_string("wrong-private-prior"),
                &first.operation_id,
            ),
            Err(PocError::RecoveryRequired(_))
        ));
        drop(_lock);
        assert!(matches!(
            validate_writer(&allocation.allocation_root, &prior_lease.writer),
            Err(PocError::StaleCapability { .. })
        ));
        assert!(matches!(
            validate_deleter(&allocation.allocation_root, &prior_lease.deleter),
            Err(PocError::StaleCapability { .. })
        ));
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!("mpla-poc-{label}-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
