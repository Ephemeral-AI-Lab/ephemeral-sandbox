use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::projection::{select_exact, ExactProjectionReceipt, ProjectionRecipe};
use crate::{
    allocation, durable, lease, ActivationOperationId, AllocationHandle, AllocationId,
    CommandReceipt, MplaSession, OperationId, PairedRefValue, PocError, PocResult, SessionId,
    SCHEMA_VERSION,
};

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
    pub readiness_program: PathBuf,
    pub readiness_arguments: Vec<String>,
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
    pub projection: ExactProjectionReceipt,
    pub fresh_upper_empty_before_mount: bool,
    pub readiness: CommandReceipt,
    pub elapsed_ns: u64,
    pub session_binding_path: PathBuf,
    pub session_binding_parent_synced: bool,
}

#[derive(Debug)]
pub struct ActivatedSession {
    pub session: MplaSession,
    pub receipt: ActivationReceipt,
}

pub fn activate_exact(request: ExactActivationRequest) -> PocResult<ActivatedSession> {
    let started = Instant::now();
    validate_request(&request)?;
    let projection = select_exact(&request.recipe)?;
    let payload_by_id: BTreeMap<_, _> = request
        .payload_allocations
        .iter()
        .map(|allocation| {
            (
                allocation.descriptor.allocation_id.clone(),
                allocation.upper_dir.clone(),
            )
        })
        .collect();
    let lower_dirs = projection
        .lower_allocation_ids_newest_first
        .iter()
        .map(|allocation_id| {
            payload_by_id.get(allocation_id).cloned().ok_or_else(|| {
                PocError::Integrity(format!(
                    "projection allocation {allocation_id} has no validated handle"
                ))
            })
        })
        .collect::<PocResult<Vec<_>>>()?;

    let fresh =
        allocation::create_allocation(&request.arena_root, &request.allocation_operation_id)?;
    if projection
        .lower_allocation_ids_newest_first
        .contains(&fresh.descriptor.allocation_id)
    {
        return Err(PocError::Integrity(
            "activation fresh allocation aliases selected payload".to_owned(),
        ));
    }
    let fresh_upper_empty_before_mount = directory_is_empty(&fresh.upper_dir)?;
    if !fresh_upper_empty_before_mount {
        return Err(PocError::Integrity(format!(
            "activation upper is not empty: {}",
            fresh.upper_dir.display()
        )));
    }

    let session_id = SessionId::new();
    let mutable_lease =
        lease::issue_workspace_lease(&fresh, session_id.clone(), &request.allocation_operation_id)?;
    let mut session = MplaSession::open(
        &request.control_root,
        fresh,
        mutable_lease.clone(),
        lower_dirs,
        request.cgroup_procs_path,
    )?;
    let readiness = session.execute(
        &mutable_lease.writer,
        &request.readiness_program,
        &request.readiness_arguments,
        request.readiness_timeout,
    )?;
    if !readiness.success {
        return Err(PocError::Integrity(
            "external activation readiness probe failed".to_owned(),
        ));
    }

    let activation_directory = request
        .control_root
        .join("activations")
        .join(request.activation_operation_id.as_str());
    let session_binding_path = activation_directory.join("SESSION_BOUND.json");
    let binding = ActivationBinding {
        schema_version: SCHEMA_VERSION,
        activation_operation_id: request.activation_operation_id.clone(),
        session_id: session_id.clone(),
        fresh_allocation_id: session.allocation().descriptor.allocation_id.clone(),
        selected_ref: request.selected_ref,
        projection: projection.clone(),
        bound_unix_ms: crate::unix_time_ms()?,
    };
    durable::replace_json(&session_binding_path, &binding)?;
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let receipt = ActivationReceipt {
        schema_version: SCHEMA_VERSION,
        activation_operation_id: request.activation_operation_id,
        session_id,
        fresh_allocation_id: session.allocation().descriptor.allocation_id.clone(),
        selected_payload_allocation_ids: projection.lower_allocation_ids_newest_first.clone(),
        projection,
        fresh_upper_empty_before_mount,
        readiness,
        elapsed_ns,
        session_binding_path,
        session_binding_parent_synced: true,
    };
    Ok(ActivatedSession { session, receipt })
}

fn validate_request(request: &ExactActivationRequest) -> PocResult<()> {
    request.recipe.validate()?;
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
    Ok(())
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
