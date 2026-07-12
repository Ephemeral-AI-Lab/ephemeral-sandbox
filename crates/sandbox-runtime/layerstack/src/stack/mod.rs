use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::error::LayerStackError;
use crate::fs::{read_manifest, resolve_layer_path};
use crate::lock::StorageWriterLockLease;
use crate::model::{manifest_root_hash, Manifest};
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, STAGING_DIR};

pub(crate) mod dir_list;
pub(crate) mod file_read;
mod layer;
pub(crate) mod lease;
mod ops;
pub(crate) mod projection;
pub mod publish;
pub(crate) mod squash;

use lease::release_lease_locked;
use lease::{
    lock_shared_registry, lock_shared_registry_recover, shared_registry_for_root, LeaseRegistry,
};
pub use lease::{RewrittenLease, SweepReport};
pub use squash::{SquashOutcome, SquashPhase, SquashPhaseObserver, SquashedBlock};

pub use projection::{
    delta_layer_refs, describe_layer_delta, emit_delta_stream, fold_delta_winners, DeltaFold,
    DeltaStreamStats, DeltaWinner, LayerDeltaDescription, LayerDeltaEntry, LayerDeltaEntryKind,
    MergedView,
};

pub(crate) fn reset_shared_registries_for_tests() {
    lease::reset_shared_registries_for_tests();
    lease::reset_shared_substitutions_for_tests();
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
    pub(crate) leases: Arc<Mutex<LeaseRegistry>>,
    pub(in crate::stack) substitutions: lease::rewrite::SubstitutionMap,
    pub(in crate::stack) view: MergedView,
}

impl LayerStack {
    pub fn open(storage_root: PathBuf) -> Result<Self, LayerStackError> {
        std::fs::create_dir_all(storage_root.join(LAYERS_DIR))?;
        std::fs::create_dir_all(storage_root.join(STAGING_DIR))?;
        let writer_lock = StorageWriterLockLease::acquire(&storage_root)?;
        let leases = shared_registry_for_root(&storage_root)?;
        let substitutions = lease::rewrite::shared_substitutions_for_root(&storage_root);
        let view = MergedView::new(storage_root.clone());
        Ok(Self {
            storage_root,
            writer_lock,
            leases,
            substitutions,
            view,
        })
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
        let manifest = self.read_active_manifest_unlocked()?;
        let lease = {
            let mut leases = lock_shared_registry(&self.leases)?;
            leases.acquire(manifest.clone(), owner_request_id)?
        };
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
        Ok(release_lease_locked(&self.storage_root, &mut leases, lease_id)?.is_some())
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
        lock_shared_registry_recover(&self.leases).active_count()
    }
}
