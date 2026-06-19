use crate::error::LayerStackError;
use crate::fs::{
    allocate_layer_dirs, fsync_dir, fsync_tree_files, resolve_layer_path, storage_bytes,
    write_layer_digest,
};
use crate::model::{manifest_root_hash, LayerRef, Manifest};
use crate::stack::LayerStack;
use crate::LAYERS_DIR;

impl LayerStack {
    pub(in crate::stack) fn build_copy_through_checkpoint(
        &self,
        manifest: &Manifest,
    ) -> Result<LayerRef, LayerStackError> {
        self.build_projected_checkpoint(manifest)
    }

    pub(in crate::stack) fn build_projected_checkpoint(
        &self,
        manifest: &Manifest,
    ) -> Result<LayerRef, LayerStackError> {
        let next_version = manifest.version.saturating_add(1);
        let (layer_id, staging_dir, layer_dir) =
            allocate_layer_dirs(&self.storage_root, 'B', next_version)?;
        if let Err(err) = self.view.project(&staging_dir, manifest) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err);
        }
        if let Some(parent) = layer_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(err) = std::fs::rename(&staging_dir, &layer_dir) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err.into());
        }
        fsync_tree_files(&layer_dir)?;
        if let Some(parent) = layer_dir.parent() {
            fsync_dir(parent)?;
        }
        write_layer_digest(&self.storage_root, &layer_id, &manifest_root_hash(manifest))?;
        Ok(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        })
    }

    pub(in crate::stack) fn layer_payload_sum(
        &self,
        layers: &[LayerRef],
    ) -> Result<u64, LayerStackError> {
        layers.iter().try_fold(0_u64, |total, layer| {
            Ok(total + storage_bytes(&resolve_layer_path(&self.storage_root, &layer.path))?)
        })
    }
}
