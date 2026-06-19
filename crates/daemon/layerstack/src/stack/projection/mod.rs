use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

mod apply;
mod checkpoint;

use crate::error::LayerStackError;
use crate::fs::{join_layer_path, remove_path, resolve_layer_path, validate_layer_ref};
use crate::model::{LayerPath, LayerRef, Manifest};
use apply::apply_layer;

use crate::whiteout::{is_kernel_whiteout, logical_whiteout_path_for_target, OPAQUE_MARKER};

pub(in crate::stack) use apply::layer_has_boundary_markers;

#[derive(Debug)]
pub struct MergedView {
    storage_root: PathBuf,
}

impl MergedView {
    #[must_use]
    pub const fn new(storage_root: PathBuf) -> Self {
        Self { storage_root }
    }

    pub fn read_bytes(
        &self,
        path: &str,
        manifest: &Manifest,
    ) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        self.read_bytes_limited(path, manifest, usize::MAX)
    }

    pub fn read_bytes_limited(
        &self,
        path: &str,
        manifest: &Manifest,
        max_bytes: usize,
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
                        .map_err(|err| stale_layer_error(layer, rel.as_str(), Some(&err)))?;
                    return Ok((Some(target.to_string_lossy().as_bytes().to_vec()), true));
                }
                Ok(meta) if meta.is_file() => {
                    let bytes = match read_file_limited(&target, &meta, max_bytes) {
                        Ok(bytes) => bytes,
                        Err(err @ LayerStackError::FileTooLarge { .. }) => return Err(err),
                        Err(err) => return Err(stale_layer_error(layer, rel.as_str(), Some(&err))),
                    };
                    return Ok((Some(bytes), true));
                }
                Ok(_) => return Err(stale_layer_error(layer, rel.as_str(), None)),
                Err(err) if err.kind() == ErrorKind::NotFound => {}
                Err(err) => return Err(stale_layer_error(layer, rel.as_str(), Some(&err))),
            }
        }
        Ok((None, false))
    }

    pub fn project(&self, destination: &Path, manifest: &Manifest) -> Result<(), LayerStackError> {
        remove_path(destination)?;
        std::fs::create_dir_all(destination)?;
        for layer in manifest.layers.iter().rev() {
            apply_layer(&self.layer_dir(layer)?, destination)?;
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
        let target = join_layer_path(layer_dir, rel);
        is_kernel_whiteout(&target) || logical_whiteout_path_for_target(&target).exists()
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

fn read_file_limited(
    path: &Path,
    meta: &std::fs::Metadata,
    max_bytes: usize,
) -> Result<Vec<u8>, LayerStackError> {
    let limit = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if meta.len() > limit {
        return Err(LayerStackError::FileTooLarge {
            size: meta.len(),
            limit: max_bytes,
        });
    }
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(LayerStackError::FileTooLarge {
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            limit: max_bytes,
        });
    }
    Ok(bytes)
}

fn stale_layer_error(
    layer: &LayerRef,
    rel: &str,
    err: Option<&dyn std::fmt::Display>,
) -> LayerStackError {
    let detail = err.map(|err| format!(" ({err})")).unwrap_or_default();
    LayerStackError::Storage(format!(
        "layer no longer present while reading {rel}: {}{detail}",
        layer.layer_id
    ))
}
