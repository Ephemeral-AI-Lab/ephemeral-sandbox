//! The `LayerStack` storage facade, its merged read view, and the snapshot
//! lease value type.
//!
//! `LayerStack` coordinates the SINGLE linearization point: one mutable
//! `manifest.json` over immutable content-addressed layer directories, swapped
//! atomically. A snapshot is O(1) — it acquires a lease and returns the
//! EXISTING `layer_paths`, NEVER a rendered tree (rendering is the caller's
//! overlay/projection concern).
//! `// PORT backend/src/sandbox/layer_stack/stack.py:73-393 — LayerStack`
//! `// PORT backend/src/sandbox/layer_stack/view.py:44 — MergedView`

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde_json::{json, Value};

use eos_protocol::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, LayerChange, LayerPath, LayerRef,
    Manifest, MANIFEST_SCHEMA_VERSION,
};

use crate::error::LayerStackError;
use crate::lease::{
    lock_shared_registry, lock_shared_registry_recover, shared_registry_for_root, LeaseRegistry,
    SharedLeaseRegistry,
};
use crate::squash::{manifest_prefix_before_plan, LayerCheckpointSquasher, SquashPlanEntry};
use crate::storage_lock::{StorageWriterLockLease, STORAGE_WRITER_LOCK_FILE};
use crate::workspace_base::build_workspace_base;
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, LAYER_METADATA_DIR, STAGING_DIR};

const LOGICAL_WHITEOUT_PREFIX: &str = ".wh.";
const OPAQUE_MARKER: &str = ".wh..wh..opq";
const TRUSTED_OVERLAY_WHITEOUT_XATTR: &str = "trusted.overlay.whiteout";
const USER_OVERLAY_WHITEOUT_XATTR: &str = "user.overlay.whiteout";
#[cfg(target_os = "linux")]
const WHITEOUT_DEVICE_MAJOR: u32 = 0;
#[cfg(target_os = "linux")]
const WHITEOUT_DEVICE_MINOR: u32 = 0;

/// Immutable result of an O(1) snapshot: a lease id + the pinned manifest's
/// existing on-disk layer paths. NEVER a rendered tree.
///
/// `// PORT backend/src/sandbox/layer_stack/stack.py:52-70 — LayerStackSnapshotLease`
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
/// `// PORT backend/src/sandbox/layer_stack/view.py:44-* — MergedView`
#[derive(Debug)]
pub struct MergedView {
    storage_root: PathBuf,
}

impl MergedView {
    /// Bind a merged view to a storage root.
    /// `// PORT backend/src/sandbox/layer_stack/view.py:45-* — MergedView.__init__`
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
    /// `// PORT backend/src/sandbox/layer_stack/view.py:66 — read_bytes`
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
    /// `// PORT backend/src/sandbox/layer_stack/view.py:195 — project`
    pub fn project(&self, destination: &Path, manifest: &Manifest) -> Result<(), LayerStackError> {
        remove_path(destination)?;
        std::fs::create_dir_all(destination)?;
        for layer in manifest.layers.iter().rev() {
            Self::apply_layer(&self.layer_dir(layer)?, destination)?;
        }
        Ok(())
    }

    fn layer_dir(&self, layer: &LayerRef) -> Result<PathBuf, LayerStackError> {
        validate_layer_ref(layer)?;
        let path = PathBuf::from(&layer.path);
        let path = if path.is_absolute() {
            path
        } else {
            self.storage_root.join(path)
        };
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

    fn apply_layer(layer_dir: &Path, destination: &Path) -> Result<(), LayerStackError> {
        let mut entries = collect_project_entries(layer_dir)?;
        entries.sort_by(|left, right| left.rel.cmp(&right.rel));
        for entry in entries
            .iter()
            .filter(|entry| matches!(entry.kind, ProjectEntryKind::Opaque))
        {
            let dir = entry
                .rel
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map_or_else(
                    || destination.to_path_buf(),
                    |parent| destination.join(parent),
                );
            clear_directory(&dir)?;
        }
        for entry in entries.iter().filter(|entry| {
            matches!(
                entry.kind,
                ProjectEntryKind::LogicalWhiteout | ProjectEntryKind::KernelWhiteout
            )
        }) {
            let target = match entry.kind {
                ProjectEntryKind::LogicalWhiteout => {
                    let Some(name) = entry.rel.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    let target_name = name.trim_start_matches(LOGICAL_WHITEOUT_PREFIX);
                    entry
                        .rel
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .map_or_else(
                            || destination.join(target_name),
                            |parent| destination.join(parent).join(target_name),
                        )
                }
                ProjectEntryKind::KernelWhiteout => destination.join(&entry.rel),
                _ => continue,
            };
            remove_path(&target)?;
        }
        for entry in entries.into_iter().filter(|entry| {
            matches!(
                entry.kind,
                ProjectEntryKind::Directory | ProjectEntryKind::File | ProjectEntryKind::Symlink
            )
        }) {
            let target = destination.join(&entry.rel);
            match entry.kind {
                ProjectEntryKind::Directory => ensure_directory(&target)?,
                ProjectEntryKind::File => {
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    remove_path(&target)?;
                    std::fs::copy(entry.path, target)?;
                }
                ProjectEntryKind::Symlink => {
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    remove_path(&target)?;
                    let link_target = std::fs::read_link(entry.path)?;
                    std::os::unix::fs::symlink(link_target, target)?;
                }
                ProjectEntryKind::Opaque
                | ProjectEntryKind::LogicalWhiteout
                | ProjectEntryKind::KernelWhiteout => {}
            }
        }
        Ok(())
    }
}

/// Durable storage facade for one layer-stack root.
///
/// Owns the manifest pointer, the lease registry, the merged read view, the
/// publisher, and the squasher. Holds the dual-layer storage-writer lease for
/// its lifetime (acquired in [`LayerStack::open`]).
/// `// PORT backend/src/sandbox/layer_stack/stack.py:73-96 — LayerStack.__init__`
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
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:76-96 — __init__`
    pub fn open(storage_root: PathBuf) -> Result<Self, LayerStackError> {
        // PORT backend/src/sandbox/layer_stack/stack.py:80-96 — mkdir storage/layers/staging, acquire writer lock, seed empty manifest
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
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:98-99 — read_active_manifest`
    pub fn read_active_manifest(&self) -> Result<Manifest, LayerStackError> {
        read_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE))
    }

    /// O(1) snapshot: acquire a lease over the active manifest and return its
    /// existing layer paths. NEVER renders a tree.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the writer lock, active manifest, or
    /// lease registry cannot be acquired.
    ///
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:108-135 — acquire_snapshot`
    pub fn acquire_snapshot(&mut self, owner_request_id: &str) -> Result<Lease, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let manifest = self.read_active_manifest()?;
        let lease = {
            let mut leases = lock_shared_registry(&self.leases)?;
            leases.acquire(manifest.clone(), owner_request_id)?
        };
        let layer_paths = manifest
            .layers
            .iter()
            .map(|layer| {
                let path = PathBuf::from(&layer.path);
                if path.is_absolute() {
                    path
                } else {
                    self.storage_root.join(path)
                }
                .to_string_lossy()
                .into_owned()
            })
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
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:137-149 — release_lease`
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
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:157-168 — can_squash`
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
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:236-298 — squash`
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
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:151-152 — leased_layers`
    #[must_use]
    pub fn leased_layers(&self) -> Vec<LayerRef> {
        lock_shared_registry_recover(&self.leases).leased_layers()
    }

    /// Squash-keep barrier set. DISTINCT from the GC retention set.
    /// `// PORT backend/src/sandbox/layer_stack/lease.py:68-85 — lease_head_layers`
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
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:300-359 — commit_to_workspace`
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
    /// `// PORT backend/src/sandbox/layer_stack/publisher.py:49-138 — publish_layer`
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

fn replace_workspace_contents(destination: &Path, source: &Path) -> Result<(), LayerStackError> {
    std::fs::create_dir_all(destination)?;
    for child in std::fs::read_dir(destination)? {
        remove_path(&child?.path())?;
    }
    for child in std::fs::read_dir(source)? {
        let child = child?;
        move_path(&child.path(), &destination.join(child.file_name()))?;
    }
    Ok(())
}

fn move_path(source: &Path, destination: &Path) -> Result<(), LayerStackError> {
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(err) if err.raw_os_error() == Some(18) => {
            copy_path(source, destination)?;
            remove_path(source)
        }
        Err(err) => Err(err.into()),
    }
}

fn copy_path(source: &Path, destination: &Path) -> Result<(), LayerStackError> {
    let meta = std::fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let target = std::fs::read_link(source)?;
        remove_path(destination)?;
        std::os::unix::fs::symlink(target, destination)?;
    } else if meta.is_dir() {
        std::fs::create_dir_all(destination)?;
        for child in std::fs::read_dir(source)? {
            let child = child?;
            copy_path(&child.path(), &destination.join(child.file_name()))?;
        }
    } else if meta.is_file() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        remove_path(destination)?;
        std::fs::copy(source, destination)?;
    }
    Ok(())
}

fn clear_storage_root_preserving_lock(storage_root: &Path) -> Result<(), LayerStackError> {
    std::fs::create_dir_all(storage_root)?;
    for child in std::fs::read_dir(storage_root)? {
        let child = child?;
        if child.file_name() == STORAGE_WRITER_LOCK_FILE {
            continue;
        }
        remove_path(&child.path())?;
    }
    Ok(())
}

fn record_elapsed(timings: &mut BTreeMap<String, f64>, key: &str, start: Instant) {
    timings.insert(key.to_owned(), start.elapsed().as_secs_f64());
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

#[derive(Debug)]
struct ProjectEntry {
    path: PathBuf,
    rel: PathBuf,
    kind: ProjectEntryKind,
}

#[derive(Debug)]
enum ProjectEntryKind {
    Opaque,
    LogicalWhiteout,
    KernelWhiteout,
    Directory,
    File,
    Symlink,
}

fn collect_project_entries(layer_dir: &Path) -> Result<Vec<ProjectEntry>, LayerStackError> {
    let mut entries = Vec::new();
    let mut stack = vec![layer_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut children = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            children.push(entry?);
        }
        children.sort_by_key(std::fs::DirEntry::path);
        for entry in children {
            let path = entry.path();
            let rel = path
                .strip_prefix(layer_dir)
                .map_err(|err| LayerStackError::Storage(err.to_string()))?
                .to_path_buf();
            let file_type = entry.file_type()?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let meta = std::fs::symlink_metadata(&path)?;
            let kind = if name == OPAQUE_MARKER {
                ProjectEntryKind::Opaque
            } else if name.starts_with(LOGICAL_WHITEOUT_PREFIX) {
                ProjectEntryKind::LogicalWhiteout
            } else if is_kernel_whiteout_meta(&path, &meta) {
                ProjectEntryKind::KernelWhiteout
            } else if file_type.is_symlink() {
                ProjectEntryKind::Symlink
            } else if file_type.is_dir() {
                stack.push(path.clone());
                ProjectEntryKind::Directory
            } else if file_type.is_file() {
                ProjectEntryKind::File
            } else {
                continue;
            };
            entries.push(ProjectEntry { path, rel, kind });
        }
    }
    Ok(entries)
}

fn clear_directory(path: &Path) -> Result<(), LayerStackError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => remove_path(path)?,
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    std::fs::create_dir_all(path)?;
    for entry in std::fs::read_dir(path)? {
        remove_path(&entry?.path())?;
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<(), LayerStackError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => remove_path(path)?,
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    std::fs::create_dir_all(path)?;
    Ok(())
}

fn read_manifest(path: impl AsRef<Path>) -> Result<Manifest, LayerStackError> {
    let path = path.as_ref();
    if !path.exists() {
        return Manifest::new(0, vec![], MANIFEST_SCHEMA_VERSION).map_err(LayerStackError::from);
    }
    let payload = std::fs::read_to_string(path)?;
    let value: Value =
        serde_json::from_str(&payload).map_err(|err| LayerStackError::Manifest(err.to_string()))?;
    let obj = value.as_object().ok_or_else(|| {
        LayerStackError::Manifest("manifest payload must be an object".to_owned())
    })?;
    let version = obj.get("version").and_then(Value::as_i64).ok_or_else(|| {
        LayerStackError::Manifest("manifest payload missing required field: version".to_owned())
    })?;
    let schema_version = obj
        .get("schema_version")
        .and_then(Value::as_i64)
        .unwrap_or(MANIFEST_SCHEMA_VERSION);
    if schema_version > MANIFEST_SCHEMA_VERSION {
        return Err(LayerStackError::Manifest(format!(
            "manifest schema_version is newer than this runtime supports: {schema_version}"
        )));
    }
    let raw_layers = obj.get("layers").and_then(Value::as_array).ok_or_else(|| {
        LayerStackError::Manifest("manifest payload missing required field: layers".to_owned())
    })?;
    let mut layers = Vec::with_capacity(raw_layers.len());
    for item in raw_layers {
        let item = item.as_object().ok_or_else(|| {
            LayerStackError::Manifest("manifest layer entries must be objects".to_owned())
        })?;
        let layer = LayerRef {
            layer_id: item
                .get("layer_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            path: item
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        validate_layer_ref(&layer)?;
        layers.push(layer);
    }
    Manifest::new(version, layers, schema_version).map_err(LayerStackError::from)
}

fn write_manifest(path: impl AsRef<Path>, manifest: &Manifest) -> Result<(), LayerStackError> {
    let value = json!({
        "schema_version": manifest.schema_version,
        "version": manifest.version,
        "layers": manifest
            .layers
            .iter()
            .map(|layer| json!({"layer_id": &layer.layer_id, "path": &layer.path}))
            .collect::<Vec<_>>(),
    });
    let encoded = serde_json::to_vec_pretty(&value)
        .map_err(|err| LayerStackError::Manifest(err.to_string()))?;
    write_atomic(path, &encoded)
}

fn validate_layer_ref(layer: &LayerRef) -> Result<(), LayerStackError> {
    if layer.layer_id.is_empty() {
        return Err(LayerStackError::Manifest(
            "layer_id must not be empty".to_owned(),
        ));
    }
    if layer.path.is_empty() {
        return Err(LayerStackError::Manifest(
            "layer path must not be empty".to_owned(),
        ));
    }
    if layer.path.contains('\0') {
        return Err(LayerStackError::Manifest(format!(
            "layer path must not contain NUL bytes: {:?}",
            layer.path
        )));
    }
    let path = Path::new(&layer.path);
    if path.is_absolute() {
        return Err(LayerStackError::Manifest(format!(
            "layer path must be relative: {}",
            layer.path
        )));
    }
    if path.components().any(|part| part.as_os_str() == "..") {
        return Err(LayerStackError::Manifest(format!(
            "layer path must not contain '..': {}",
            layer.path
        )));
    }
    Ok(())
}

fn join_layer_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |path, part| {
        if part.is_empty() {
            path
        } else {
            path.join(part)
        }
    })
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

#[cfg(target_os = "linux")]
fn write_kernel_whiteout(path: &Path) -> Result<(), LayerStackError> {
    let device = rustix::fs::makedev(WHITEOUT_DEVICE_MAJOR, WHITEOUT_DEVICE_MINOR);
    let mknod = rustix::fs::mknodat(
        rustix::fs::CWD,
        path,
        rustix::fs::FileType::CharacterDevice,
        rustix::fs::Mode::from_raw_mode(0o644),
        device,
    );
    if mknod.is_ok() {
        return Ok(());
    }

    std::fs::write(path, b"")?;
    let trusted = rustix::fs::setxattr(
        path,
        TRUSTED_OVERLAY_WHITEOUT_XATTR,
        b"y",
        rustix::fs::XattrFlags::empty(),
    );
    let user = rustix::fs::setxattr(
        path,
        USER_OVERLAY_WHITEOUT_XATTR,
        b"y",
        rustix::fs::XattrFlags::empty(),
    );
    if trusted.is_err() && user.is_err() {
        let _ = std::fs::remove_file(path);
        return Err(LayerStackError::Storage(format!(
            "failed to mark overlay whiteout {}: mknod={:?}, trusted={:?}, user={:?}",
            path.display(),
            mknod.err(),
            trusted.err(),
            user.err()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn write_kernel_whiteout(path: &Path) -> Result<(), LayerStackError> {
    let logical = logical_whiteout_path_for_target(path);
    if let Some(parent) = logical.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(logical, b"")?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn logical_whiteout_path_for_target(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default();
    let mut whiteout_name = OsString::from(LOGICAL_WHITEOUT_PREFIX);
    whiteout_name.push(name);
    match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => parent.join(whiteout_name),
        None => PathBuf::from(whiteout_name),
    }
}

fn is_kernel_whiteout(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| is_kernel_whiteout_meta(path, &meta))
}

#[cfg(unix)]
fn is_kernel_whiteout_meta(path: &Path, meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_char_device() && meta.rdev() == 0 {
        return true;
    }
    meta.is_file()
        && meta.len() == 0
        && (has_xattr(path, TRUSTED_OVERLAY_WHITEOUT_XATTR)
            || has_xattr(path, USER_OVERLAY_WHITEOUT_XATTR))
}

#[cfg(not(unix))]
fn is_kernel_whiteout_meta(_path: &Path, _meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn has_xattr(path: &Path, name: &str) -> bool {
    let mut value = [0_u8; 1];
    rustix::fs::lgetxattr(path, name, &mut value).is_ok()
}

fn remove_path(path: &Path) -> Result<(), LayerStackError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || meta.is_file() => {
            std::fs::remove_file(path)?;
        }
        Ok(meta) if meta.is_dir() => {
            std::fs::remove_dir_all(path)?;
        }
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

static NEXT_LAYER: AtomicU64 = AtomicU64::new(0);
static NEXT_TMP_WRITE: AtomicU64 = AtomicU64::new(0);

fn write_atomic(path: impl AsRef<Path>, bytes: &[u8]) -> Result<(), LayerStackError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("layerstack"),
        std::process::id(),
        NEXT_TMP_WRITE.fetch_add(1, Ordering::Relaxed)
    ));
    // tmp+rename+fsync(parent): the directory entry is only durable once the
    // parent dir is fsynced after the rename, so a crash cannot leave the
    // manifest/digest pointer-swap half-applied (CAS linearization).
    let result = (|| -> Result<(), LayerStackError> {
        write_bytes_fsynced(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            fsync_dir(parent)?;
        }
        Ok(())
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

/// Write `bytes` to `path` (create/truncate) and fsync the file before
/// returning; the caller fsyncs the parent dir after any rename.
// PORT backend/src/sandbox/layer_stack/paths.py:52-64 — write_bytes_fsynced
fn write_bytes_fsynced(path: &Path, bytes: &[u8]) -> Result<(), LayerStackError> {
    use std::io::Write as _;
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// fsync a directory so a prior create/rename into it is persisted.
// PORT backend/src/sandbox/layer_stack/paths.py:43-49 — fsync_path (dir fd)
pub(crate) fn fsync_dir(path: &Path) -> Result<(), LayerStackError> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// fsync every non-symlink regular file under `root` (the staged layer tree),
/// matching `os.walk(followlinks=False)` + `is_file()` filtering. Special
/// files (char-device whiteouts) and symlinks are skipped — fsync is N/A.
// PORT backend/src/sandbox/layer_stack/publisher.py:161-168 — _fsync_tree_files
fn fsync_tree_files(root: &Path) -> Result<(), LayerStackError> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = std::fs::symlink_metadata(&path)?.file_type();
        if file_type.is_dir() {
            fsync_tree_files(&path)?;
        } else if file_type.is_file() {
            std::fs::File::open(&path)?.sync_all()?;
        }
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn squash_coalesces_layers_and_preserves_merged_reads() -> TestResult {
        let fixture = Fixture::new("squash_basic");
        let mut stack = LayerStack::open(fixture.root.clone())?;
        publish_text(&mut stack, "a.txt", "one\n")?;
        publish_text(&mut stack, "b.txt", "two\n")?;
        publish_text(&mut stack, "a.txt", "three\n")?;

        assert!(stack.can_squash(2)?);
        let squashed = stack
            .squash(2)?
            .ok_or_else(|| std::io::Error::other("squash should produce a manifest"))?;

        assert_eq!(squashed.layers.len(), 1);
        assert_eq!(stack.read_text("a.txt")?.0, "three\n");
        assert_eq!(stack.read_text("b.txt")?.0, "two\n");
        assert!(stack.squash(2)?.is_none());
        Ok(())
    }

    #[test]
    fn release_lease_gcs_squashed_layers_after_retaining_lease_drops() -> TestResult {
        let fixture = Fixture::new("squash_gc");
        let mut stack = LayerStack::open(fixture.root.clone())?;
        publish_text(&mut stack, "a.txt", "one\n")?;
        publish_text(&mut stack, "b.txt", "two\n")?;
        publish_text(&mut stack, "c.txt", "three\n")?;

        let lease = stack.acquire_snapshot("reader")?;
        let old_tail: Vec<LayerRef> = lease.manifest.layers[1..].to_vec();
        let squashed = stack
            .squash(2)?
            .ok_or_else(|| std::io::Error::other("squash should produce a manifest"))?;
        assert_eq!(squashed.layers.len(), 2);
        for layer in &old_tail {
            assert!(fixture.root.join(&layer.path).exists());
        }

        assert!(stack.release_lease(&lease.lease_id)?);
        for layer in &old_tail {
            assert!(!fixture.root.join(&layer.path).exists());
        }
        Ok(())
    }

    #[test]
    fn cross_instance_lease_retains_squashed_layers_until_reopened_release() -> TestResult {
        let fixture = Fixture::new("squash_gc_cross_instance");
        let mut stack = LayerStack::open(fixture.root.clone())?;
        publish_text(&mut stack, "a.txt", "one\n")?;
        publish_text(&mut stack, "b.txt", "two\n")?;
        publish_text(&mut stack, "c.txt", "three\n")?;
        drop(stack);

        let mut lease_stack = LayerStack::open(fixture.root.clone())?;
        let lease = lease_stack.acquire_snapshot("reader")?;
        let old_tail: Vec<LayerRef> = lease.manifest.layers[1..].to_vec();
        assert_eq!(
            LayerStack::open(fixture.root.clone())?.active_lease_count(),
            1
        );

        let mut squash_stack = LayerStack::open(fixture.root.clone())?;
        let squashed = squash_stack
            .squash(2)?
            .ok_or_else(|| std::io::Error::other("squash should produce a manifest"))?;
        assert_eq!(squashed.layers.len(), 2);
        for layer in &old_tail {
            assert!(fixture.root.join(&layer.path).exists());
        }

        assert!(LayerStack::open(fixture.root.clone())?.release_lease(&lease.lease_id)?);
        assert_eq!(
            LayerStack::open(fixture.root.clone())?.active_lease_count(),
            0
        );
        for layer in &old_tail {
            assert!(!fixture.root.join(&layer.path).exists());
        }
        Ok(())
    }

    #[test]
    fn delete_layer_hides_files_in_reads_and_projection() -> TestResult {
        let fixture = Fixture::new("delete_hides");
        let mut stack = LayerStack::open(fixture.root.clone())?;
        publish_text(&mut stack, "dir/a.txt", "one\n")?;
        publish_text(&mut stack, "dir/b.txt", "two\n")?;

        stack.publish_layer(&[LayerChange::Delete {
            path: LayerPath::parse("dir/a.txt")?,
        }])?;

        assert_eq!(stack.read_text("dir/a.txt")?, (String::new(), false));
        assert_eq!(stack.read_text("dir/b.txt")?, ("two\n".to_owned(), true));

        let projected = fixture.root.join("projected");
        stack
            .view
            .project(&projected, &stack.read_active_manifest()?)?;
        assert!(!projected.join("dir/a.txt").exists());
        assert_eq!(
            std::fs::read_to_string(projected.join("dir/b.txt"))?,
            "two\n"
        );
        assert!(
            !projected.join("dir/.wh.a.txt").exists(),
            "logical whiteout marker must not leak into projections"
        );
        Ok(())
    }

    #[test]
    fn commit_to_workspace_projects_active_manifest_and_rebuilds_base() -> TestResult {
        let fixture = Fixture::new("commit_workspace");
        std::fs::create_dir_all(fixture.workspace.join(".git"))?;
        std::fs::write(fixture.workspace.join(".git/config"), "[core]\n")?;
        std::fs::write(fixture.workspace.join("tracked.txt"), "base\n")?;
        build_workspace_base(&fixture.root, &fixture.workspace, false)?;

        let mut stack = LayerStack::open(fixture.root.clone())?;
        publish_text(&mut stack, "tracked.txt", "overlay\n")?;
        publish_text(&mut stack, "new.txt", "new\n")?;

        let (manifest, timings) = stack.commit_to_workspace(&fixture.workspace)?;

        assert_eq!(manifest.version, 1);
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join("tracked.txt"))?,
            "overlay\n"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join("new.txt"))?,
            "new\n"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join(".git/config"))?,
            "[core]\n"
        );
        assert!(timings.contains_key("layer_stack.commit_to_workspace.project_s"));
        assert!(timings.contains_key("layer_stack.commit_to_workspace.replace_workspace_s"));
        assert!(timings.contains_key("layer_stack.commit_to_workspace.rebuild_base_s"));
        assert!(timings.contains_key("layer_stack.commit_to_workspace.total_s"));
        assert_eq!(stack.read_text("tracked.txt")?.0, "overlay\n");
        Ok(())
    }

    fn publish_text(stack: &mut LayerStack, path: &str, content: &str) -> TestResult {
        stack.publish_layer(&[LayerChange::Write {
            path: LayerPath::parse(path)?,
            content: content.as_bytes().to_vec(),
        }])?;
        Ok(())
    }

    struct Fixture {
        root: PathBuf,
        workspace: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "eos-layerstack-{label}-{}-{}",
                std::process::id(),
                NEXT_TMP_WRITE.fetch_add(1, Ordering::Relaxed)
            ));
            let workspace = root.with_extension("workspace");
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(&workspace);
            Self { root, workspace }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
            let _ = std::fs::remove_dir_all(&self.workspace);
        }
    }
}
