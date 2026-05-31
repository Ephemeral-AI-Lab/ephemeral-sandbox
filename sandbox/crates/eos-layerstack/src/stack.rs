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
use std::path::{Path, PathBuf};

use serde_json::Value;

use eos_protocol::{manifest_root_hash, LayerPath, LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};

use crate::error::LayerStackError;
use crate::lease::LeaseRegistry;
use crate::storage_lock::StorageWriterLockLease;
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, STAGING_DIR};

/// Immutable result of an O(1) snapshot: a lease id + the pinned manifest's
/// existing on-disk layer paths. NEVER a rendered tree.
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
    pub fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
    }

    /// Read a path's raw bytes through `manifest`. Returns `(bytes, found)`.
    /// `// PORT backend/src/sandbox/layer_stack/view.py:66 — read_bytes`
    pub fn read_bytes(
        &self,
        path: &str,
        manifest: &Manifest,
    ) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        let rel = LayerPath::parse(path)?;
        for layer in &manifest.layers {
            let layer_dir = self.layer_dir(layer)?;
            if self.is_whiteouted(&layer_dir, rel.as_str()) {
                return Ok((None, false));
            }
            if self.lookup_blocked_by_layer(&layer_dir, rel.as_str()) {
                return Ok((None, false));
            }
            let target = join_layer_path(&layer_dir, rel.as_str());
            match std::fs::symlink_metadata(&target) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    let target = std::fs::read_link(&target)
                        .map_err(|err| stale_layer_error(layer, rel.as_str(), err))?;
                    return Ok((Some(target.to_string_lossy().as_bytes().to_vec()), true));
                }
                Ok(meta) if meta.is_file() => {
                    let bytes = std::fs::read(&target)
                        .map_err(|err| stale_layer_error(layer, rel.as_str(), err))?;
                    return Ok((Some(bytes), true));
                }
                Ok(_) => return Err(stale_layer_error_value(layer, rel.as_str())),
                Err(err) if err.kind() == ErrorKind::NotFound => continue,
                Err(err) => return Err(stale_layer_error(layer, rel.as_str(), err)),
            }
        }
        Ok((None, false))
    }

    /// Project the merged view of `manifest` into `destination` (full render).
    /// `// PORT backend/src/sandbox/layer_stack/view.py:195 — project`
    pub fn project(&self, destination: &Path, manifest: &Manifest) -> Result<(), LayerStackError> {
        let _ = (destination, manifest);
        // PORT backend/src/sandbox/layer_stack/view.py:195-* — materialize merged tree at destination
        todo!("PORT: MergedView.project")
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

    fn is_whiteouted(&self, layer_dir: &Path, rel: &str) -> bool {
        let rel_path = PathBuf::from(rel);
        let Some(name) = rel_path.file_name() else {
            return false;
        };
        let marker_name = {
            let mut marker = OsString::from(".wh.");
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

    fn lookup_blocked_by_layer(&self, layer_dir: &Path, rel: &str) -> bool {
        let parts: Vec<&str> = rel.split('/').collect();
        for index in 1..parts.len() {
            let ancestor = parts[..index].join("/");
            let path = join_layer_path(layer_dir, &ancestor);
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                if meta.is_file() || meta.file_type().is_symlink() {
                    return true;
                }
            }
            if path.join(".wh..wh..opq").exists() {
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
/// `// PORT backend/src/sandbox/layer_stack/stack.py:73-96 — LayerStack.__init__`
#[derive(Debug)]
pub struct LayerStack {
    storage_root: PathBuf,
    _writer_lock: StorageWriterLockLease,
    _leases: LeaseRegistry,
    _view: MergedView,
}

impl LayerStack {
    /// Open (creating dirs as needed) a layer stack at `storage_root`, acquiring
    /// the cross-process writer lease and seeding an empty manifest if absent.
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:76-96 — __init__`
    pub fn open(storage_root: PathBuf) -> Result<Self, LayerStackError> {
        // PORT backend/src/sandbox/layer_stack/stack.py:80-96 — mkdir storage/layers/staging, acquire writer lock, seed empty manifest
        std::fs::create_dir_all(storage_root.join(LAYERS_DIR))?;
        std::fs::create_dir_all(storage_root.join(STAGING_DIR))?;
        let writer_lock = StorageWriterLockLease::acquire(&storage_root)?;
        let view = MergedView::new(storage_root.clone());
        Ok(Self {
            storage_root,
            _writer_lock: writer_lock,
            _leases: LeaseRegistry::new(),
            _view: view,
        })
    }

    /// The storage root this stack manages.
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// Read the current active manifest from `manifest.json`.
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:98-99 — read_active_manifest`
    pub fn read_active_manifest(&self) -> Result<Manifest, LayerStackError> {
        read_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE))
    }

    /// O(1) snapshot: acquire a lease over the active manifest and return its
    /// existing layer paths. NEVER renders a tree.
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:108-135 — acquire_snapshot`
    pub fn acquire_snapshot(&mut self, owner_request_id: &str) -> Result<Lease, LayerStackError> {
        let _guard = self._writer_lock.exclusive()?;
        let manifest = self.read_active_manifest()?;
        let lease = self._leases.acquire(manifest.clone(), owner_request_id);
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
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:137-149 — release_lease`
    pub fn release_lease(&mut self, lease_id: &str) -> Result<bool, LayerStackError> {
        let _guard = self._writer_lock.exclusive()?;
        Ok(self._leases.release(lease_id).is_some())
    }

    /// Whether a squash would reduce manifest depth below `max_depth`.
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:157-168 — can_squash`
    pub fn can_squash(&self, max_depth: usize) -> Result<bool, LayerStackError> {
        let _ = max_depth;
        // PORT backend/src/sandbox/layer_stack/stack.py:157-168 — squasher.plan(..., lease_head_layers, min_reduction=2) is Some
        todo!("PORT: LayerStack.can_squash")
    }

    /// Non-destructively squash foldable runs, swapping a shorter manifest.
    /// Returns the new manifest, or `None` if nothing was foldable.
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:236-298 — squash`
    pub fn squash(&mut self, max_depth: usize) -> Result<Option<Manifest>, LayerStackError> {
        let _ = max_depth;
        // PORT backend/src/sandbox/layer_stack/squash.py:179 — checkpoint id "B{next_version:06}-{uuid8}"
        // PORT backend/src/sandbox/layer_stack/stack.py:236-298 — plan, build checkpoints, atomic pointer-swap, rollback in finally
        todo!("PORT: LayerStack.squash")
    }

    /// Full retention keep-set (GC). DISTINCT from squash barriers.
    /// `// PORT backend/src/sandbox/layer_stack/stack.py:151-152 — leased_layers`
    pub fn leased_layers(&self) -> Vec<LayerRef> {
        self._leases.leased_layers()
    }

    /// Squash-keep barrier set. DISTINCT from the GC retention set.
    /// `// PORT backend/src/sandbox/layer_stack/lease.py:68-85 — lease_head_layers`
    pub fn lease_head_layers(&self) -> Vec<LayerRef> {
        self._leases.lease_head_layers()
    }

    /// Number of active snapshot leases.
    pub fn active_lease_count(&self) -> usize {
        self._leases.active_count()
    }

    /// Read raw bytes through the active manifest.
    pub fn read_bytes(&self, path: &str) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        self._view.read_bytes(path, &self.read_active_manifest()?)
    }

    /// Read UTF-8 text through the active manifest.
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

fn stale_layer_error(layer: &LayerRef, rel: &str, err: std::io::Error) -> LayerStackError {
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
