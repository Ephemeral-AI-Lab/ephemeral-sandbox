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
    written_unix_ms: u64,
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
    allocation_root.join("owner").join(LEASE_FILE)
}
