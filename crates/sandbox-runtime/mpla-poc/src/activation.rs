use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, Timespec, Timestamps, CWD};
use serde::{Deserialize, Serialize};

use crate::projection::{select_exact, ExactProjectionReceipt, ProjectionRecipe};
use crate::recovery::reach_real_operation;
use crate::{
    allocation, durable, lease, ActivationOperationId, AllocationHandle, AllocationId,
    CommandReceipt, MplaSession, NamedFaultInjector, NamedFaultPoint, OperationId, PairedRefValue,
    PocError, PocResult, SessionId, SCHEMA_VERSION,
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
    let activation_directory = request
        .control_root
        .join("activations")
        .join(request.activation_operation_id.as_str());
    let locator_pin_path = activation_directory.join("LOCATOR_PIN.json");
    let mut named_faults = NamedFaultInjector::default().with_physical_context(
        operation_id.as_str(),
        [
            locator_pin_path.clone(),
            activation_directory.join("SESSION_BOUND.json"),
            activation_directory.join("OUTCOME.json"),
        ],
    );
    let projection = select_exact(&request.recipe)?;
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
    durable::replace_json(
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
    let selected_root = lower_dirs.first().ok_or_else(|| {
        PocError::Integrity("activation projection selected no payload root".to_owned())
    })?;
    inherit_projection_root_metadata(selected_root, &fresh.upper_dir)?;
    let allocation_elapsed_ns = elapsed_ns(allocation_started);

    let lease_started = Instant::now();
    let session_id = SessionId::new();
    let mutable_lease =
        lease::issue_workspace_lease(&fresh, session_id.clone(), &request.allocation_operation_id)?;
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
    let mut session = MplaSession::open(
        &request.control_root,
        fresh,
        mutable_lease.clone(),
        lower_dirs,
        request.cgroup_procs_path,
    )?;
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
        &request.readiness_path,
        request.readiness_contains.as_deref(),
        request.readiness_timeout,
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
    durable::replace_json(&session_binding_path, &binding)?;
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
    let receipt = ActivationReceipt {
        schema_version: SCHEMA_VERSION,
        activation_operation_id: request.activation_operation_id.clone(),
        session_id,
        fresh_allocation_id: session.allocation().descriptor.allocation_id.clone(),
        selected_payload_allocation_ids: projection.lower_allocation_ids_newest_first.clone(),
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
    durable::replace_json(&outcome_path, &receipt)?;
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
