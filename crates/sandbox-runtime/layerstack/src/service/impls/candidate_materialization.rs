use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::lock::StorageWriterLockLease;
use crate::stack::candidate::generation::{
    GenerationLease, GenerationSelection, GenerationStore, MaterializationKey,
};
use crate::stack::candidate::materialization::{
    MaterializationCoordinator, MaterializationDisposition, MaterializationError,
    MaterializationRequest,
};
use crate::stack::candidate::native_backend::NativeBackend;
use crate::stack::observation::{shared_observation_state_for_root, HiddenValidationObservation};
use crate::{LayerStack, LayerStackError, Lease};

use super::super::model::{
    CandidateGenerationAdmission, CandidateGenerationLease, CandidateGenerationSelection,
    CandidateMaterializationDisposition, CandidateMaterializationResult,
};

/// Explicitly build or reuse the hidden-validation root's native candidate.
///
/// Warm workspace admission never calls this function.
#[doc(hidden)]
pub fn materialize_hidden_candidate(
    root: &Path,
    timeout: Duration,
) -> Result<CandidateMaterializationResult, LayerStackError> {
    let stack = LayerStack::open(root.to_path_buf())?;
    let observation = stack.hidden_validation_observation();
    observation.record_native_materialization();
    let logical_root = stack.hidden_validation_root()?.ok_or_else(|| {
        LayerStackError::Storage(
            "strict candidate materialization requires a hidden-validation root".to_owned(),
        )
    })?;
    let request =
        MaterializationRequest::new(MaterializationKey::linux_overlayfs(logical_root), timeout);
    let coordinator = MaterializationCoordinator::new_observed(root.to_path_buf(), observation)
        .map_err(candidate_error("initialize materializer"))?;
    let outcome = coordinator
        .materialize(&request, &stack.writer_lock)
        .map_err(candidate_error("materialize hidden candidate"))?;
    Ok(CandidateMaterializationResult {
        disposition: match outcome.disposition {
            MaterializationDisposition::Built => CandidateMaterializationDisposition::Built,
            MaterializationDisposition::Reused => CandidateMaterializationDisposition::Reused,
            MaterializationDisposition::Shared => CandidateMaterializationDisposition::Shared,
        },
        selection: selection_model(outcome.selection),
        maximum_buffer_bytes: outcome.maximum_buffer_bytes,
    })
}

/// Resolve and verify the currently selected prebuilt native generation.
///
/// Missing or invalid candidate state is returned as a candidate error; this
/// function does not consult v1 layer paths.
#[doc(hidden)]
pub fn lookup_hidden_candidate_generation(
    root: &Path,
) -> Result<Option<CandidateGenerationSelection>, LayerStackError> {
    let stack = LayerStack::open(root.to_path_buf())?;
    let observation = stack.hidden_validation_observation();
    observation.record_native_lookup_validation();
    let Some(logical_root) = stack.hidden_validation_root()? else {
        return Ok(None);
    };
    let coordinator = MaterializationCoordinator::new(root.to_path_buf())
        .map_err(candidate_error("initialize materializer"))?;
    coordinator
        .lookup_warm(&MaterializationKey::linux_overlayfs(logical_root))
        .map(|selection| selection.map(selection_model))
        .map_err(candidate_error("lookup hidden candidate"))
}

/// Atomically resolve the current prebuilt generation and durably lease its
/// exact materialization/generation/fence tuple before workspace mutation.
#[doc(hidden)]
pub fn acquire_hidden_candidate_generation(
    root: &Path,
    owner: &str,
    session_id: &str,
    lease_ttl: Duration,
) -> Result<CandidateGenerationAdmission, LayerStackError> {
    let stack = LayerStack::open(root.to_path_buf())?;
    let observation = stack.hidden_validation_observation();
    let logical_root = stack.hidden_validation_root()?.ok_or_else(|| {
        LayerStackError::Storage(
            "strict candidate admission requires a hidden-validation root".to_owned(),
        )
    })?;
    let key = MaterializationKey::linux_overlayfs(logical_root);
    let store = GenerationStore::new(root.to_path_buf())
        .map_err(candidate_error("initialize generation store"))?;
    let now = unix_now()?;
    let expires = now
        .checked_add(lease_ttl.as_secs())
        .filter(|expires| *expires > now)
        .ok_or_else(|| {
            LayerStackError::Storage(
                "candidate generation lease duration must be at least one second".to_owned(),
            )
        })?;
    let _guard = stack.writer_lock.exclusive()?;
    observation.record_native_lookup_validation();
    let selection = lookup_warm_from_store(&store, &key)
        .map_err(candidate_error("resolve strict candidate CURRENT"))?
        .ok_or_else(|| {
            LayerStackError::Storage(
                "strict candidate admission requires a prebuilt native generation".to_owned(),
            )
        })?;
    let lease = store
        .acquire_lease(&key, &selection, owner, session_id, now, expires)
        .map_err(candidate_error("acquire exact candidate generation lease"))?;
    observation.record_native_admission();
    Ok(CandidateGenerationAdmission {
        selection: selection_model(selection),
        lease: lease_model(lease),
    })
}

/// Atomically acquire the exact candidate generation and the legacy snapshot
/// metadata needed by workspace lifecycle through one LayerStack instance.
///
/// The selector writer lock stays held across both captures so their
/// authorities cannot drift between calls. If snapshot capture fails, the
/// candidate lease is removed before the error is returned.
#[doc(hidden)]
pub fn acquire_hidden_candidate_generation_with_snapshot(
    root: &Path,
    owner: &str,
    session_id: &str,
    lease_ttl: Duration,
) -> Result<(CandidateGenerationAdmission, Lease), LayerStackError> {
    let stack = LayerStack::open(root.to_path_buf())?;
    let observation = stack.hidden_validation_observation();
    let logical_root = stack.hidden_validation_root()?.ok_or_else(|| {
        LayerStackError::Storage(
            "strict candidate admission requires a hidden-validation root".to_owned(),
        )
    })?;
    let key = MaterializationKey::linux_overlayfs(logical_root);
    let store = GenerationStore::new(root.to_path_buf())
        .map_err(candidate_error("initialize generation store"))?;
    let now = unix_now()?;
    let expires = now
        .checked_add(lease_ttl.as_secs())
        .filter(|expires| *expires > now)
        .ok_or_else(|| {
            LayerStackError::Storage(
                "candidate generation lease duration must be at least one second".to_owned(),
            )
        })?;
    let _guard = stack.writer_lock.exclusive()?;
    observation.record_native_lookup_validation();
    let selection = lookup_warm_from_store(&store, &key)
        .map_err(candidate_error("resolve strict candidate CURRENT"))?
        .ok_or_else(|| {
            LayerStackError::Storage(
                "strict candidate admission requires a prebuilt native generation".to_owned(),
            )
        })?;
    let candidate_lease = store
        .acquire_lease(&key, &selection, owner, session_id, now, expires)
        .map_err(candidate_error("acquire exact candidate generation lease"))?;
    let snapshot = match stack.acquire_snapshot_unlocked(owner) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let cleanup = match store.release_lease(&candidate_lease) {
                Ok(true) => String::new(),
                Ok(false) => "; candidate lease rollback did not find the exact lease".to_owned(),
                Err(cleanup) => format!("; candidate lease rollback failed: {cleanup}"),
            };
            return Err(LayerStackError::Storage(format!(
                "acquire legacy snapshot for strict admission failed: {error}{cleanup}"
            )));
        }
    };
    observation.record_native_admission();
    Ok((
        CandidateGenerationAdmission {
            selection: selection_model(selection),
            lease: lease_model(candidate_lease),
        },
        snapshot,
    ))
}

/// Record completion of the native-provider mount for a strict candidate
/// session. The call is intentionally separate from lease admission because
/// mount failure must not be reported as success.
#[doc(hidden)]
pub fn record_hidden_candidate_mount(root: &Path) -> Result<(), LayerStackError> {
    let stack = LayerStack::open(root.to_path_buf())?;
    stack.hidden_validation_observation().record_native_mount();
    Ok(())
}

#[doc(hidden)]
pub fn renew_candidate_generation_lease(
    root: &Path,
    lease: &CandidateGenerationLease,
    lease_ttl: Duration,
) -> Result<CandidateGenerationLease, LayerStackError> {
    let stack = LayerStack::open(root.to_path_buf())?;
    let store = GenerationStore::new(root.to_path_buf())
        .map_err(candidate_error("initialize generation store"))?;
    let now = unix_now()?;
    let expires = now
        .checked_add(lease_ttl.as_secs())
        .filter(|expires| *expires > now)
        .ok_or_else(|| {
            LayerStackError::Storage(
                "candidate generation lease duration must be at least one second".to_owned(),
            )
        })?;
    let _guard = stack.writer_lock.exclusive()?;
    store
        .renew_lease(&lease_record(lease), now, expires)
        .map(lease_model)
        .map_err(candidate_error("renew exact candidate generation lease"))
}

/// Record a completed native mount and renew its exact generation lease for
/// the live session using one LayerStack open. The caller must persist the
/// returned lease in its workspace-recovery record before exposing success.
#[doc(hidden)]
pub fn finalize_hidden_candidate_session(
    root: &Path,
    lease: &CandidateGenerationLease,
    lease_ttl: Duration,
) -> Result<CandidateGenerationLease, LayerStackError> {
    let writer_lock = StorageWriterLockLease::acquire(root)?;
    let observation = HiddenValidationObservation::new(shared_observation_state_for_root(root)?);
    observation.record_native_mount();
    let store = GenerationStore::new(root.to_path_buf())
        .map_err(candidate_error("initialize generation store"))?;
    let now = unix_now()?;
    let expires = now
        .checked_add(lease_ttl.as_secs())
        .filter(|expires| *expires > now)
        .ok_or_else(|| {
            LayerStackError::Storage(
                "candidate generation lease duration must be at least one second".to_owned(),
            )
        })?;
    let _guard = writer_lock.exclusive()?;
    store
        .renew_lease(&lease_record(lease), now, expires)
        .map(lease_model)
        .map_err(candidate_error("finalize exact candidate generation lease"))
}

#[doc(hidden)]
pub fn release_candidate_generation_lease(
    root: &Path,
    lease: &CandidateGenerationLease,
) -> Result<bool, LayerStackError> {
    let stack = LayerStack::open(root.to_path_buf())?;
    let store = GenerationStore::new(root.to_path_buf())
        .map_err(candidate_error("initialize generation store"))?;
    let _guard = stack.writer_lock.exclusive()?;
    store
        .release_lease(&lease_record(lease))
        .map_err(candidate_error("release exact candidate generation lease"))
}

fn lookup_warm_from_store(
    store: &GenerationStore,
    key: &MaterializationKey,
) -> Result<Option<GenerationSelection>, MaterializationError> {
    key.validate()?;
    let Some(selection) = store.lookup_current(key)? else {
        return Ok(None);
    };
    NativeBackend::new().validate_warm_capabilities(
        &selection.manifest.required_capabilities,
        &selection.manifest.provided_capabilities,
    )?;
    Ok(Some(selection))
}

fn selection_model(selection: GenerationSelection) -> CandidateGenerationSelection {
    CandidateGenerationSelection {
        materialization_id: selection.manifest.materialization_id,
        root_id: selection.manifest.root_id,
        backend_kind: selection.manifest.backend_kind,
        backend_format_version: selection.manifest.backend_format_version,
        target_profile: selection.manifest.target_profile,
        generation: selection.manifest.generation,
        fence: selection.manifest.fence,
        manifest_sha256: selection.manifest_sha256,
        carrier_path: selection.carrier_path,
        native_tree_sha256: selection.manifest.native_tree_sha256,
        build_operation_id: selection.manifest.build_operation_id,
    }
}

fn lease_model(lease: GenerationLease) -> CandidateGenerationLease {
    CandidateGenerationLease {
        lease_id: lease.lease_id,
        materialization_id: lease.materialization_id,
        generation: lease.generation,
        fence: lease.fence,
        owner: lease.owner,
        session_id: lease.session_id,
        acquired_unix_seconds: lease.acquired_unix_seconds,
        renewed_unix_seconds: lease.renewed_unix_seconds,
        expires_unix_seconds: lease.expires_unix_seconds,
        checksum_sha256: lease.checksum_sha256,
    }
}

fn lease_record(lease: &CandidateGenerationLease) -> GenerationLease {
    GenerationLease {
        schema: "layerstack-materialization-lease-v1".to_owned(),
        schema_version: 1,
        lease_id: lease.lease_id.clone(),
        materialization_id: lease.materialization_id.clone(),
        generation: lease.generation,
        fence: lease.fence,
        owner: lease.owner.clone(),
        session_id: lease.session_id.clone(),
        acquired_unix_seconds: lease.acquired_unix_seconds,
        renewed_unix_seconds: lease.renewed_unix_seconds,
        expires_unix_seconds: lease.expires_unix_seconds,
        checksum_sha256: lease.checksum_sha256.clone(),
    }
}

fn unix_now() -> Result<u64, LayerStackError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| LayerStackError::Storage(error.to_string()))
}

fn candidate_error<E: std::fmt::Display>(
    operation: &'static str,
) -> impl FnOnce(E) -> LayerStackError {
    move |error| LayerStackError::Storage(format!("{operation} failed: {error}"))
}
