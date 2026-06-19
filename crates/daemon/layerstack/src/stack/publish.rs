use std::io::ErrorKind;

use super::layer_write::write_layer_changes;
use super::LayerStack;
use crate::error::LayerStackError;
use crate::fs::{
    allocate_layer_dirs, fsync_dir, fsync_tree_files, layer_digest_path, remove_path,
    write_layer_digest, write_manifest,
};
use crate::model::{try_layer_digest, LayerChange, LayerRef, Manifest};
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, LAYER_METADATA_DIR};

const FAIL_NEXT_PUBLISH_MARKER_FILE: &str = "fail-next-publish";
const ENABLE_TEST_FAILPOINTS_ENV: &str = "EOS_LAYERSTACK_ENABLE_TEST_FAILPOINTS";

impl LayerStack {
    pub fn publish_layer(&mut self, changes: &[LayerChange]) -> Result<Manifest, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        if changes.is_empty() {
            return Ok(active);
        }

        let digest = try_layer_digest(changes)?;
        if self.head_layer_digest(&active)? == Some(digest.clone()) {
            return Ok(active);
        }

        self.take_publish_failpoint_marker()?;

        let next_version = active.version + 1;
        let (layer_id, staging_dir, layer_dir) =
            allocate_layer_dirs(&self.storage_root, 'L', next_version)?;
        std::fs::create_dir_all(&staging_dir)?;
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
        if let Some(parent) = layer_dir.parent() {
            fsync_dir(parent)?;
        }

        if let Err(err) = write_layer_digest(&self.storage_root, &layer_id, &digest) {
            let _ = remove_path(&layer_dir);
            return Err(err);
        }

        let latest = self.read_active_manifest_unlocked()?;
        if latest != active {
            let _ = remove_path(&layer_dir);
            let _ = std::fs::remove_file(layer_digest_path(&self.storage_root, &layer_id));
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
            let _ = std::fs::remove_file(layer_digest_path(&self.storage_root, &layer_id));
            return Err(err);
        }
        Ok(manifest)
    }

    fn head_layer_digest(&self, manifest: &Manifest) -> Result<Option<String>, LayerStackError> {
        let Some(head) = manifest.layers.first() else {
            return Ok(None);
        };
        let path = layer_digest_path(&self.storage_root, &head.layer_id);
        match std::fs::read_to_string(path) {
            Ok(value) => Ok(Some(value)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn take_publish_failpoint_marker(&self) -> Result<(), LayerStackError> {
        if std::env::var(ENABLE_TEST_FAILPOINTS_ENV).ok().as_deref() != Some("1") {
            return Ok(());
        }
        let marker = self
            .storage_root
            .join(LAYER_METADATA_DIR)
            .join(FAIL_NEXT_PUBLISH_MARKER_FILE);
        match std::fs::remove_file(&marker) {
            Ok(()) => Err(LayerStackError::Storage(
                "injected layerstack publish failure".to_owned(),
            )),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}
