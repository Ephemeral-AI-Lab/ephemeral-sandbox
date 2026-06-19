use std::path::PathBuf;

use crate::error::LayerStackError;
use crate::fs::{count_dirs, read_manifest, resolve_layer_path, storage_bytes};
use crate::lock::StorageWriterLockLease;
use crate::model::{manifest_root_hash, LayerRef, Manifest};
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, STAGING_DIR};

mod checkpoint;
mod layer_read;
mod layer_write;
pub(crate) mod lease_aware;
mod lease_aware_ops;
mod lease_cleanup;
mod leases;
mod publish;
mod read;
pub(crate) mod squash;
mod squash_ops;
mod view;
mod workspace_commit;

use lease_cleanup::{release_lease_locked, retarget_lease_locked};
pub(crate) use leases::reset_shared_registries_for_tests;
use leases::{
    lock_shared_registry, lock_shared_registry_recover, shared_registry_for_root,
    SharedLeaseRegistry,
};
use workspace_commit::recover_commit_to_workspace;
#[allow(unused_imports)]
pub(crate) use workspace_commit::*;

pub use view::MergedView;

#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub lease_id: String,
    pub manifest_version: i64,
    pub root_hash: String,
    pub manifest: Manifest,
    pub layer_paths: Vec<String>,
}

#[derive(Debug)]
pub struct SquashOutcome {
    pub manifest: Option<Manifest>,
    pub lease_release_error: Option<LayerStackError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerStackStorageMetrics {
    pub layer_dirs: usize,
    pub staging_dirs: usize,
    pub storage_bytes: u64,
}

#[derive(Debug)]
pub struct LayerStack {
    pub(in crate::stack) storage_root: PathBuf,
    pub(crate) writer_lock: StorageWriterLockLease,
    pub(in crate::stack) leases: SharedLeaseRegistry,
    pub(in crate::stack) view: MergedView,
}

impl LayerStack {
    pub fn open(storage_root: PathBuf) -> Result<Self, LayerStackError> {
        std::fs::create_dir_all(storage_root.join(LAYERS_DIR))?;
        std::fs::create_dir_all(storage_root.join(STAGING_DIR))?;
        let writer_lock = StorageWriterLockLease::acquire(&storage_root)?;
        recover_commit_to_workspace(&storage_root)?;
        let leases = shared_registry_for_root(&storage_root)?;
        let view = MergedView::new(storage_root.clone());
        Ok(Self {
            storage_root,
            writer_lock,
            leases,
            view,
        })
    }

    pub fn read_active_manifest(&self) -> Result<Manifest, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        self.read_active_manifest_unlocked()
    }

    pub(in crate::stack) fn read_active_manifest_unlocked(
        &self,
    ) -> Result<Manifest, LayerStackError> {
        read_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE))
    }

    pub(crate) fn with_active_manifest<T>(
        &self,
        f: impl FnOnce(&Manifest) -> Result<T, LayerStackError>,
    ) -> Result<T, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest_unlocked()?;
        f(&manifest)
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
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        Ok(Lease {
            lease_id: lease.lease_id,
            manifest_version: manifest.version,
            root_hash: manifest_root_hash(&manifest),
            manifest,
            layer_paths,
        })
    }

    pub fn release_lease(&mut self, lease_id: &str) -> Result<bool, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let mut leases = lock_shared_registry(&self.leases)?;
        release_lease_locked(&self.storage_root, &mut leases, lease_id)
    }

    #[doc(hidden)]
    pub fn retarget_lease_manifest(
        &mut self,
        lease_id: &str,
        manifest: Manifest,
    ) -> Result<bool, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let mut leases = lock_shared_registry(&self.leases)?;
        retarget_lease_locked(&self.storage_root, &mut leases, lease_id, manifest)
    }

    #[must_use]
    pub fn leased_layers(&self) -> Vec<LayerRef> {
        lock_shared_registry_recover(&self.leases).leased_layers()
    }

    #[must_use]
    pub fn active_lease_count(&self) -> usize {
        lock_shared_registry_recover(&self.leases).active_count()
    }

    pub fn storage_metrics(&self) -> Result<LayerStackStorageMetrics, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let root = &self.storage_root;
        Ok(LayerStackStorageMetrics {
            layer_dirs: count_dirs(&root.join(LAYERS_DIR))?,
            staging_dirs: count_dirs(&root.join(STAGING_DIR))?,
            storage_bytes: storage_bytes(root)?,
        })
    }
}
