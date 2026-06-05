//! The `LayerStack` storage facade, its merged read view, and the snapshot
//! lease value type.
//!
//! `LayerStack` coordinates the SINGLE linearization point: one mutable
//! `manifest.json` over immutable content-addressed layer directories, swapped
//! atomically. A snapshot is O(1) — it acquires a lease and returns the
//! EXISTING `layer_paths`, NEVER a rendered tree (rendering is the caller's
//! overlay/projection concern).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use eos_protocol::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, LayerChange, LayerPath, LayerRef,
    Manifest,
};

use crate::error::LayerStackError;
use crate::fsutil::{join_layer_path, record_elapsed, remove_path, resolve_layer_path};
use crate::lease::{
    lock_shared_registry, lock_shared_registry_recover, shared_registry_for_root, LeaseRegistry,
    SharedLeaseRegistry,
};
use crate::squash::{manifest_prefix_before_plan, LayerCheckpointSquasher, SquashPlanEntry};
use crate::storage_lock::StorageWriterLockLease;
use crate::workspace_base::build_workspace_base;
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, LAYER_METADATA_DIR, STAGING_DIR};

mod fs;
mod manifest_io;
mod projection;
mod whiteout;

use fs::{clear_storage_root_preserving_lock, fsync_tree_files, replace_workspace_contents};
pub(crate) use fs::{fsync_dir, write_atomic};
pub(crate) use manifest_io::{read_manifest, validate_layer_ref, write_manifest};
use whiteout::{is_kernel_whiteout, write_kernel_whiteout, LOGICAL_WHITEOUT_PREFIX, OPAQUE_MARKER};

/// Immutable result of an O(1) snapshot: a lease id + the pinned manifest's
/// existing on-disk layer paths. NEVER a rendered tree.
///
// No `Eq`: `timings` holds `f64` (no total ordering).
#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub lease_id: String,
    pub manifest_version: i64,
    pub root_hash: String,
    pub manifest: Manifest,
    /// POSIX paths of the manifest's layer directories, in manifest order.
    pub layer_paths: Vec<String>,
    /// Phase timings keyed `layer_stack.acquire_snapshot.*`.
    pub timings: BTreeMap<String, f64>,
}

/// Layered read view over a storage root's manifest (lowest→highest precedence).
///
/// Reads resolve through the manifest's layer directories without materializing
/// a tree; this is the pure-read sibling of the overlay mount.
#[derive(Debug)]
pub struct MergedView {
    storage_root: PathBuf,
}

impl MergedView {
    /// Bind a merged view to a storage root.
    #[must_use]
    pub const fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
    }

    /// Read a path's raw bytes through `manifest`. Returns `(bytes, found)`.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when `path` is invalid, a manifest layer is
    /// missing, or a referenced file cannot be read.
    ///
    pub fn read_bytes(
        &self,
        path: &str,
        manifest: &Manifest,
    ) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        let rel = LayerPath::parse(path)?;
        for layer in &manifest.layers {
            let layer_dir = self.layer_dir(layer)?;
            if Self::is_whiteouted(&layer_dir, rel.as_str()) {
                return Ok((None, false));
            }
            if Self::lookup_blocked_by_layer(&layer_dir, rel.as_str()) {
                return Ok((None, false));
            }
            let target = join_layer_path(&layer_dir, rel.as_str());
            match std::fs::symlink_metadata(&target) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    let target = std::fs::read_link(&target)
                        .map_err(|err| stale_layer_error(layer, rel.as_str(), &err))?;
                    return Ok((Some(target.to_string_lossy().as_bytes().to_vec()), true));
                }
                Ok(meta) if meta.is_file() => {
                    let bytes = std::fs::read(&target)
                        .map_err(|err| stale_layer_error(layer, rel.as_str(), &err))?;
                    return Ok((Some(bytes), true));
                }
                Ok(_) => return Err(stale_layer_error_value(layer, rel.as_str())),
                Err(err) if err.kind() == ErrorKind::NotFound => {}
                Err(err) => return Err(stale_layer_error(layer, rel.as_str(), &err)),
            }
        }
        Ok((None, false))
    }

    /// Project the merged view of `manifest` into `destination` (full render).
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when destination reset, directory creation,
    /// source layer reads, or file/symlink projection fails.
    ///
    pub fn project(&self, destination: &Path, manifest: &Manifest) -> Result<(), LayerStackError> {
        remove_path(destination)?;
        std::fs::create_dir_all(destination)?;
        for layer in manifest.layers.iter().rev() {
            projection::apply_layer(&self.layer_dir(layer)?, destination)?;
        }
        Ok(())
    }

    fn layer_dir(&self, layer: &LayerRef) -> Result<PathBuf, LayerStackError> {
        validate_layer_ref(layer)?;
        let path = resolve_layer_path(&self.storage_root, &layer.path);
        if !path.is_dir() {
            return Err(LayerStackError::Storage(format!(
                "manifest references missing layer {}: {}",
                layer.layer_id, layer.path
            )));
        }
        Ok(path)
    }

    fn is_whiteouted(layer_dir: &Path, rel: &str) -> bool {
        if is_kernel_whiteout(&join_layer_path(layer_dir, rel)) {
            return true;
        }
        let rel_path = PathBuf::from(rel);
        let Some(name) = rel_path.file_name() else {
            return false;
        };
        let marker_name = {
            let mut marker = OsString::from(LOGICAL_WHITEOUT_PREFIX);
            marker.push(name);
            marker
        };
        let parent = rel_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        let marker = match parent {
            Some(parent) => layer_dir.join(parent).join(marker_name),
            None => layer_dir.join(marker_name),
        };
        marker.exists()
    }

    fn lookup_blocked_by_layer(layer_dir: &Path, rel: &str) -> bool {
        let parts: Vec<&str> = rel.split('/').collect();
        for index in 1..parts.len() {
            let ancestor = parts[..index].join("/");
            let path = join_layer_path(layer_dir, &ancestor);
            if is_kernel_whiteout(&path) {
                return true;
            }
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                if meta.is_file() || meta.file_type().is_symlink() {
                    return true;
                }
            }
            if path.join(OPAQUE_MARKER).exists() {
                return true;
            }
        }
        false
    }
}

/// Durable storage facade for one layer-stack root.
///
/// Owns the manifest pointer, the lease registry, the merged read view, the
/// publisher, and the squasher. Holds the dual-layer storage-writer lease for
/// its lifetime (acquired in [`LayerStack::open`]).
#[derive(Debug)]
pub struct LayerStack {
    storage_root: PathBuf,
    writer_lock: StorageWriterLockLease,
    leases: SharedLeaseRegistry,
    view: MergedView,
}

impl LayerStack {
    /// Open (creating dirs as needed) a layer stack at `storage_root`, acquiring
    /// the cross-process writer lease and seeding an empty manifest if absent.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when storage directories, the writer lock, or
    /// the initial manifest cannot be prepared.
    ///
    pub fn open(storage_root: PathBuf) -> Result<Self, LayerStackError> {
        std::fs::create_dir_all(storage_root.join(LAYERS_DIR))?;
        std::fs::create_dir_all(storage_root.join(STAGING_DIR))?;
        let writer_lock = StorageWriterLockLease::acquire(&storage_root)?;
        let leases = shared_registry_for_root(&storage_root)?;
        let view = MergedView::new(storage_root.clone());
        Ok(Self {
            storage_root,
            writer_lock,
            leases,
            view,
        })
    }

    /// The storage root this stack manages.
    #[must_use]
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// Read the current active manifest from `manifest.json`.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when `manifest.json` cannot be read or
    /// decoded.
    ///
    pub fn read_active_manifest(&self) -> Result<Manifest, LayerStackError> {
        read_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE))
    }

    /// O(1) snapshot: acquire a lease over the active manifest and return its
    /// existing layer paths. NEVER renders a tree.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the storage lock, active manifest, or
    /// lease registry cannot be acquired.
    ///
    pub fn acquire_snapshot(&mut self, owner_request_id: &str) -> Result<Lease, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest()?;
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
            timings: BTreeMap::new(),
        })
    }

    /// Release a snapshot lease by id and GC any now-unreferenced layers.
    /// Returns `false` if the lease id was unknown.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the writer lock cannot be acquired or
    /// unreferenced layer cleanup fails.
    ///
    pub fn release_lease(&mut self, lease_id: &str) -> Result<bool, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let mut leases = lock_shared_registry(&self.leases)?;
        release_lease_locked(&self.storage_root, &mut leases, lease_id)
    }

    /// Whether a squash would reduce manifest depth below `max_depth`.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when manifest reads or squash planning fail.
    ///
    pub fn can_squash(&self, max_depth: usize) -> Result<bool, LayerStackError> {
        let active = self.read_active_manifest()?;
        let squasher = LayerCheckpointSquasher::new(self.storage_root.clone());
        let lease_head_layers = lock_shared_registry(&self.leases)?.lease_head_layers();
        Ok(squasher
            .plan(&active, max_depth, &lease_head_layers, 2)?
            .is_some())
    }

    /// Non-destructively squash foldable runs, swapping a shorter manifest.
    /// Returns the new manifest, or `None` if nothing was foldable.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when locking, planning, checkpoint creation,
    /// manifest swapping, lease release, or rollback cleanup fails.
    ///
    pub fn squash(&mut self, max_depth: usize) -> Result<Option<Manifest>, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest()?;
        let squasher = LayerCheckpointSquasher::new(self.storage_root.clone());
        let lease_head_layers = {
            let leases = lock_shared_registry(&self.leases)?;
            leases.lease_head_layers()
        };
        let Some(plan) = squasher.plan(&active, max_depth, &lease_head_layers, 1)? else {
            return Ok(None);
        };
        let squash_lease = {
            let mut leases = lock_shared_registry(&self.leases)?;
            leases.acquire(
                active,
                &format!("squash-{}", NEXT_LAYER.fetch_add(1, Ordering::Relaxed)),
            )?
        };

        let mut checkpoints = Vec::new();
        let mut committed = false;
        let outcome = (|| {
            for segment in plan.checkpoint_segments() {
                checkpoints.push(squasher.build_checkpoint(segment, plan.active_version)?);
            }

            let current = self.read_active_manifest()?;
            let Some(live_prefix) = manifest_prefix_before_plan(&current, &plan) else {
                return Ok(None);
            };
            let next_version = current.version + 1;
            let mut checkpoint_index = 0;
            let mut new_layers = live_prefix.to_vec();
            for entry in &plan.entries {
                match entry {
                    SquashPlanEntry::Keep(layer) => new_layers.push(layer.clone()),
                    SquashPlanEntry::Segment(_) => {
                        let mut checkpoint = checkpoints[checkpoint_index].clone();
                        let expected_prefix = format!("B{next_version:06}-");
                        if !checkpoint.layer_id.starts_with(&expected_prefix) {
                            checkpoint = squasher.relabel_checkpoint(&checkpoint, next_version)?;
                            checkpoints[checkpoint_index] = checkpoint.clone();
                        }
                        new_layers.push(checkpoint);
                        checkpoint_index += 1;
                    }
                }
            }
            let manifest = Manifest::new(next_version, new_layers, current.schema_version)
                .map_err(LayerStackError::from)?;
            write_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE), &manifest)?;
            committed = true;
            Ok(Some(manifest))
        })();

        if !committed {
            for checkpoint in &checkpoints {
                let _ = squasher.discard_checkpoint(checkpoint);
            }
        }
        let release = {
            let mut leases = lock_shared_registry(&self.leases)?;
            release_lease_locked(&self.storage_root, &mut leases, &squash_lease.lease_id)
        };
        match (outcome, release) {
            (Err(err), _) | (Ok(_), Err(err)) => Err(err),
            (Ok(manifest), Ok(_)) => Ok(manifest),
        }
    }

    /// Full retention keep-set (GC). DISTINCT from squash barriers.
    #[must_use]
    pub fn leased_layers(&self) -> Vec<LayerRef> {
        lock_shared_registry_recover(&self.leases).leased_layers()
    }

    /// Squash-keep barrier set. DISTINCT from the GC retention set.
    #[must_use]
    pub fn lease_head_layers(&self) -> Vec<LayerRef> {
        lock_shared_registry_recover(&self.leases).lease_head_layers()
    }

    /// Number of active snapshot leases.
    #[must_use]
    pub fn active_lease_count(&self) -> usize {
        lock_shared_registry_recover(&self.leases).active_count()
    }

    /// Collapse the active manifest back into the bound workspace base.
    ///
    /// Refuses to run while any snapshot lease is active. The projection
    /// materializes the current merged view into `workspace_root`, resets
    /// layer-stack storage, and rebuilds a fresh base layer from those bytes.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the workspace is invalid, active leases
    /// exist, projection/replacement fails, or base rebuild fails.
    ///
    pub fn commit_to_workspace(
        &mut self,
        workspace_root: &Path,
    ) -> Result<(Manifest, BTreeMap<String, f64>), LayerStackError> {
        let writer_lock = StorageWriterLockLease::acquire(&self.storage_root)?;
        let _guard = writer_lock.exclusive()?;
        let total_start = Instant::now();
        if !workspace_root.is_dir() {
            return Err(LayerStackError::WorkspaceBinding(format!(
                "workspace_root does not exist: {}",
                workspace_root.display()
            )));
        }
        if lock_shared_registry(&self.leases)?.active_count() > 0 {
            return Err(LayerStackError::Storage(
                "commit_to_workspace blocked by active leases".to_owned(),
            ));
        }

        let active = self.read_active_manifest()?;
        let projection = self.commit_projection_dir()?;
        let mut timings = BTreeMap::new();
        let outcome = (|| {
            let project_start = Instant::now();
            self.view.project(&projection, &active)?;
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.project_s",
                project_start,
            );

            let replace_start = Instant::now();
            replace_workspace_contents(workspace_root, &projection)?;
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.replace_workspace_s",
                replace_start,
            );

            let rebuild_start = Instant::now();
            clear_storage_root_preserving_lock(&self.storage_root)?;
            let _ = build_workspace_base(&self.storage_root, workspace_root, false)?;
            self.view = MergedView::new(self.storage_root.clone());
            let new_manifest = self.read_active_manifest()?;
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.rebuild_base_s",
                rebuild_start,
            );
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.total_s",
                total_start,
            );
            Ok(new_manifest)
        })();
        let _ = remove_path(&projection);
        outcome.map(|manifest| (manifest, timings))
    }

    /// Read raw bytes through the active manifest.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the active manifest cannot be read or
    /// the merged read fails.
    ///
    pub fn read_bytes(&self, path: &str) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        self.view.read_bytes(path, &self.read_active_manifest()?)
    }

    /// Read UTF-8 text through the active manifest.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when bytes cannot be read or decoded as
    /// UTF-8.
    ///
    pub fn read_text(&self, path: &str) -> Result<(String, bool), LayerStackError> {
        let (bytes, exists) = self.read_bytes(path)?;
        if !exists {
            return Ok((String::new(), false));
        }
        let bytes = bytes.unwrap_or_default();
        let text =
            String::from_utf8(bytes).map_err(|err| LayerStackError::Storage(err.to_string()))?;
        Ok((text, true))
    }

    /// Publish accepted changes as one immutable layer under the storage-writer
    /// guard, returning the active manifest after publish.
    ///
    /// This is the policy-blind `LayerStack` half of Phase 3: callers are
    /// responsible for OCC route/conflict decisions before they hand changes
    /// here. The CAS byte-identity pieces come from `eos-protocol`.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when locking, staging, layer persistence,
    /// digest persistence, CAS validation, or manifest writes fail.
    ///
    pub fn publish_layer(&mut self, changes: &[LayerChange]) -> Result<Manifest, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest()?;
        if changes.is_empty() {
            return Ok(active);
        }

        let digest = layer_digest(changes);
        if self.head_layer_digest(&active)? == Some(digest.clone()) {
            return Ok(active);
        }

        let next_version = active.version + 1;
        let (layer_id, staging_dir, layer_dir) = self.allocate_layer_paths(next_version)?;
        std::fs::create_dir_all(&staging_dir)?;
        // Persist the staged layer (files, then the staging dir) BEFORE the
        // rename so the renamed layer dir never references unflushed contents.
        if let Err(err) = write_layer_changes(&staging_dir, changes)
            .and_then(|()| fsync_tree_files(&staging_dir))
            .and_then(|()| fsync_dir(&staging_dir))
        {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err);
        }

        if let Err(err) = std::fs::rename(&staging_dir, &layer_dir) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err.into());
        }
        // fsync the layers/ parent so the renamed layer dir entry is durable.
        if let Some(parent) = layer_dir.parent() {
            fsync_dir(parent)?;
        }

        if let Err(err) = self.write_layer_digest(&layer_id, &digest) {
            let _ = remove_path(&layer_dir);
            return Err(err);
        }

        let latest = self.read_active_manifest()?;
        if latest != active {
            let _ = remove_path(&layer_dir);
            let _ = std::fs::remove_file(self.layer_digest_path(&layer_id));
            return Err(LayerStackError::ManifestConflict {
                expected: active.version,
                found: latest.version,
            });
        }

        let mut layers = Vec::with_capacity(active.layers.len() + 1);
        layers.push(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        });
        layers.extend(active.layers);
        let manifest = Manifest::new(next_version, layers, active.schema_version)
            .map_err(LayerStackError::from)?;
        if let Err(err) = write_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE), &manifest) {
            let _ = remove_path(&layer_dir);
            let _ = std::fs::remove_file(self.layer_digest_path(&layer_id));
            return Err(err);
        }
        Ok(manifest)
    }

    fn allocate_layer_paths(
        &self,
        next_version: i64,
    ) -> Result<(String, PathBuf, PathBuf), LayerStackError> {
        for _ in 0..100 {
            let unique = NEXT_LAYER.fetch_add(1, Ordering::Relaxed);
            let layer_id = format!("L{next_version:06}-{unique:08x}");
            let staging_dir = self
                .storage_root
                .join(STAGING_DIR)
                .join(format!("{layer_id}.staging"));
            let layer_dir = self.storage_root.join(LAYERS_DIR).join(&layer_id);
            if !staging_dir.exists() && !layer_dir.exists() {
                return Ok((layer_id, staging_dir, layer_dir));
            }
        }
        Err(LayerStackError::LayerIdAllocation)
    }

    fn layer_digest_path(&self, layer_id: &str) -> PathBuf {
        self.storage_root
            .join(LAYER_METADATA_DIR)
            .join(format!("{layer_id}.digest"))
    }

    fn head_layer_digest(&self, manifest: &Manifest) -> Result<Option<String>, LayerStackError> {
        let Some(head) = manifest.layers.first() else {
            return Ok(None);
        };
        let path = self.layer_digest_path(&head.layer_id);
        match std::fs::read_to_string(path) {
            Ok(value) => Ok(Some(value)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn write_layer_digest(&self, layer_id: &str, digest: &str) -> Result<(), LayerStackError> {
        let path = self.layer_digest_path(layer_id);
        write_atomic(path, digest.as_bytes())
    }

    fn commit_projection_dir(&self) -> Result<PathBuf, LayerStackError> {
        let parent = self.storage_root.join("runtime").join("commit");
        std::fs::create_dir_all(&parent)?;
        for _ in 0..100 {
            let candidate = parent.join(format!(
                "projected-{}-{}",
                std::process::id(),
                NEXT_TMP_WRITE.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.into()),
            }
        }
        Err(LayerStackError::Storage(
            "could not allocate commit projection directory".to_owned(),
        ))
    }
}

fn release_lease_locked(
    storage_root: &Path,
    leases: &mut LeaseRegistry,
    lease_id: &str,
) -> Result<bool, LayerStackError> {
    let Some(lease) = leases.release(lease_id) else {
        return Ok(false);
    };
    let active = read_manifest(storage_root.join(ACTIVE_MANIFEST_FILE))?;
    let removable = unreferenced_layers(&lease.manifest.layers, &active, leases);
    remove_layers(storage_root, &removable)?;
    Ok(true)
}

fn unreferenced_layers(
    candidates: &[LayerRef],
    active: &Manifest,
    leases: &LeaseRegistry,
) -> Vec<LayerRef> {
    let retained_layers = leases.leased_layers();
    candidates
        .iter()
        .filter(|layer| !active.layers.contains(layer) && !retained_layers.contains(layer))
        .cloned()
        .collect()
}

fn remove_layers(storage_root: &Path, layers: &[LayerRef]) -> Result<(), LayerStackError> {
    for layer in layers {
        validate_layer_ref(layer)?;
        remove_path(&storage_root.join(&layer.path))?;
        match std::fs::remove_file(layer_digest_path_at(storage_root, &layer.layer_id)) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

fn layer_digest_path_at(storage_root: &Path, layer_id: &str) -> PathBuf {
    storage_root
        .join(LAYER_METADATA_DIR)
        .join(format!("{layer_id}.digest"))
}

fn write_layer_changes(layer_dir: &Path, changes: &[LayerChange]) -> Result<(), LayerStackError> {
    for change in aggregate_layer_changes(changes) {
        match change {
            LayerChange::Write { path, content } => {
                let target = join_layer_path(layer_dir, path.as_str());
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                remove_path(&target)?;
                std::fs::write(target, content)?;
            }
            LayerChange::Delete { path } => {
                let target = join_layer_path(layer_dir, path.as_str());
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                remove_path(&target)?;
                write_kernel_whiteout(&target)?;
            }
            LayerChange::Symlink { path, source_path } => {
                let target = join_layer_path(layer_dir, path.as_str());
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                remove_path(&target)?;
                std::os::unix::fs::symlink(source_path, target)?;
            }
            LayerChange::OpaqueDir { path } => {
                let marker = join_layer_path(layer_dir, path.as_str()).join(OPAQUE_MARKER);
                if let Some(parent) = marker.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(marker, b"")?;
            }
        }
    }
    Ok(())
}

static NEXT_LAYER: AtomicU64 = AtomicU64::new(0);
static NEXT_TMP_WRITE: AtomicU64 = AtomicU64::new(0);

fn stale_layer_error(layer: &LayerRef, rel: &str, err: &std::io::Error) -> LayerStackError {
    LayerStackError::Storage(format!(
        "layer no longer present while reading {rel}: {} ({err})",
        layer.layer_id
    ))
}

fn stale_layer_error_value(layer: &LayerRef, rel: &str) -> LayerStackError {
    LayerStackError::Storage(format!(
        "layer no longer present while reading {rel}: {}",
        layer.layer_id
    ))
}
