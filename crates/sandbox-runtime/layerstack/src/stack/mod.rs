use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::LayerStackError;
use crate::fs::{read_manifest, resolve_layer_path};
use crate::lock::StorageWriterLockLease;
use crate::model::{manifest_root_hash, Manifest};
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, STAGING_DIR};

pub(crate) mod candidate;
pub(crate) mod dir_list;
pub(crate) mod file_read;
mod layer;
pub(crate) mod lease;
pub(crate) mod observation;
mod ops;
pub(crate) mod projection;
pub mod publish;
pub(crate) mod squash;

#[doc(hidden)]
pub use candidate::publication::{HiddenValidationOutcome, HiddenValidationPublication};
use lease::release_lease_locked;
use lease::{
    lock_shared_registry, lock_shared_registry_recover, shared_registry_for_root, LeaseRegistry,
};
pub use lease::{RewrittenLease, SweepReport};
#[doc(hidden)]
pub use observation::{
    HiddenQueuedWork, HiddenTaskWork, HiddenValidationObservation, HiddenWorkerGuard,
};
pub use squash::{SquashOutcome, SquashPhase, SquashPhaseObserver, SquashedBlock};

pub use projection::{
    delta_layer_refs, describe_layer_delta, emit_delta_stream, fold_delta_winners, DeltaFold,
    DeltaStreamStats, DeltaWinner, LayerDeltaDescription, LayerDeltaEntry, LayerDeltaEntryKind,
    MergedView,
};

pub(crate) fn reset_shared_registries_for_tests() {
    lease::reset_shared_registries_for_tests();
    lease::reset_shared_substitutions_for_tests();
    observation::reset_shared_observation_states_for_tests();
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub lease_id: String,
    pub manifest: Manifest,
    pub layer_paths: Vec<PathBuf>,
}

impl Lease {
    #[must_use]
    pub fn manifest_version(&self) -> i64 {
        self.manifest.version
    }

    #[must_use]
    pub fn root_hash(&self) -> String {
        manifest_root_hash(&self.manifest)
    }
}

#[derive(Debug)]
pub struct LayerStack {
    pub(in crate::stack) storage_root: PathBuf,
    pub(crate) writer_lock: StorageWriterLockLease,
    pub(crate) supervisor: Arc<crate::supervisor::StorageSupervisor>,
    pub(crate) leases: Arc<Mutex<LeaseRegistry>>,
    pub(crate) observation: Arc<observation::StorageObservationState>,
    pub(in crate::stack) substitutions: lease::rewrite::SubstitutionMap,
    pub(in crate::stack) view: MergedView,
}

#[derive(Clone)]
pub struct ActiveLeaseCounter {
    leases: Arc<Mutex<LeaseRegistry>>,
}

impl ActiveLeaseCounter {
    #[must_use]
    pub fn active_lease_count(&self) -> usize {
        lock_shared_registry_recover(&self.leases).active_count()
    }
}

impl LayerStack {
    pub fn open(storage_root: PathBuf) -> Result<Self, LayerStackError> {
        std::fs::create_dir_all(storage_root.join(LAYERS_DIR))?;
        std::fs::create_dir_all(storage_root.join(STAGING_DIR))?;
        let writer_lock = StorageWriterLockLease::acquire(&storage_root)?;
        let supervisor = crate::supervisor::shared_supervisor_for_root(&storage_root)
            .map_err(|error| LayerStackError::Storage(error.to_string()))?;
        let leases = shared_registry_for_root(&storage_root)?;
        let observation = observation::shared_observation_state_for_root(&storage_root)?;
        let substitutions = lease::rewrite::shared_substitutions_for_root(&storage_root);
        let view = MergedView::new(storage_root.clone());
        Ok(Self {
            storage_root,
            writer_lock,
            supervisor,
            leases,
            observation,
            substitutions,
            view,
        })
    }

    /// Stops admission, waits for all storage-owned materialization work to
    /// return its permits, and joins the owned worker pool.
    ///
    /// Consuming the stack makes the shutdown boundary explicit: no operation
    /// can be started through this handle after the join completes.
    pub fn shutdown(self, timeout: Duration) -> Result<(), LayerStackError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| LayerStackError::Storage("shutdown deadline overflow".to_owned()))?;
        self.supervisor
            .shutdown(deadline, &AtomicBool::new(false))
            .map_err(|error| LayerStackError::Storage(error.to_string()))
    }

    pub fn read_active_manifest(&self) -> Result<Manifest, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        self.read_active_manifest_unlocked()
    }

    pub(crate) fn read_active_manifest_unlocked(&self) -> Result<Manifest, LayerStackError> {
        read_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE))
    }

    pub fn acquire_snapshot(&self, owner_request_id: &str) -> Result<Lease, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        self.acquire_snapshot_unlocked(owner_request_id)
    }

    pub(crate) fn acquire_snapshot_unlocked(
        &self,
        owner_request_id: &str,
    ) -> Result<Lease, LayerStackError> {
        let manifest = self.read_active_manifest_unlocked()?;
        let lease = {
            let mut leases = lock_shared_registry(&self.leases)?;
            leases.acquire(manifest.clone(), owner_request_id)?
        };
        self.observation
            .record_active_leases(self.active_lease_count());
        let layer_paths = manifest
            .layers
            .iter()
            .map(|layer| resolve_layer_path(&self.storage_root, &layer.path))
            .collect();
        Ok(Lease {
            lease_id: lease.lease_id,
            manifest,
            layer_paths,
        })
    }

    pub fn release_lease(&mut self, lease_id: &str) -> Result<bool, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let mut leases = lock_shared_registry(&self.leases)?;
        let released = release_lease_locked(&self.storage_root, &mut leases, lease_id)?.is_some();
        self.observation.record_active_leases(leases.active_count());
        Ok(released)
    }

    /// Fail-closed boot storage sweep to the active manifest's keep-set.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the writer lock is unavailable or a
    /// deletion fails; a missing or unreadable manifest is not an error and
    /// reports a skip instead.
    pub fn sweep_storage(&mut self) -> Result<lease::SweepReport, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        lease::sweep_storage_locked(&self.storage_root)
    }

    #[must_use]
    pub fn active_lease_count(&self) -> usize {
        self.active_lease_counter().active_lease_count()
    }

    #[must_use]
    pub fn active_lease_counter(&self) -> ActiveLeaseCounter {
        ActiveLeaseCounter {
            leases: Arc::clone(&self.leases),
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn hidden_validation_observation(&self) -> HiddenValidationObservation {
        HiddenValidationObservation::new(Arc::clone(&self.observation))
    }
}
