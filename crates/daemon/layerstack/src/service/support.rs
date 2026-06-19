use std::path::{Path, PathBuf};

use crate::commit::CommitError;
use crate::model::{manifest_root_hash, LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};
use crate::{LayerStackError, Lease};

use super::model::{LeasedSnapshot, Snapshot};

pub(crate) fn snapshot_manifest(
    root: &Path,
    version: i64,
    layer_paths: &[PathBuf],
) -> Result<Manifest, CommitError> {
    snapshot_manifest_with_layer_ids(root, version, layer_paths, false)
}

pub(crate) fn snapshot_manifest_preserving_layer_ids(
    root: &Path,
    version: i64,
    layer_paths: &[PathBuf],
) -> Result<Manifest, CommitError> {
    snapshot_manifest_with_layer_ids(root, version, layer_paths, true)
}

fn snapshot_manifest_with_layer_ids(
    root: &Path,
    version: i64,
    layer_paths: &[PathBuf],
    preserve_layer_ids: bool,
) -> Result<Manifest, CommitError> {
    let layers = layer_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let relative = match path.strip_prefix(root) {
                Ok(relative) => relative,
                Err(_) if path.is_relative() => path,
                Err(_) => {
                    return Err(CommitError::Storage(LayerStackError::Manifest(format!(
                        "snapshot layer path {} is outside {}",
                        path.display(),
                        root.display()
                    ))));
                }
            };
            let relative_path = relative.to_string_lossy().into_owned();
            let layer_id = if preserve_layer_ids {
                layer_id_from_relative_path(relative).unwrap_or_else(|| format!("snapshot-{index}"))
            } else {
                format!("snapshot-{index}")
            };
            Ok(LayerRef {
                layer_id,
                path: relative_path,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Manifest::new(version, layers, MANIFEST_SCHEMA_VERSION)?)
}

fn layer_id_from_relative_path(relative: &Path) -> Option<String> {
    let mut components = relative.components();
    let first = components.next()?.as_os_str();
    if first != std::ffi::OsStr::new(crate::LAYERS_DIR) {
        return None;
    }
    let layer_id = components
        .next()?
        .as_os_str()
        .to_string_lossy()
        .into_owned();
    if components.next().is_some() || layer_id.is_empty() {
        return None;
    }
    Some(layer_id)
}

pub(super) fn snapshot_from_manifest(root: &Path, manifest: Manifest) -> Snapshot {
    Snapshot {
        manifest_version: manifest.version,
        root_hash: manifest_root_hash(&manifest),
        layer_paths: manifest
            .layers
            .iter()
            .map(|layer| crate::fs::resolve_layer_path(root, &layer.path))
            .collect(),
    }
}

pub(super) fn snapshot_from_lease(lease: Lease) -> LeasedSnapshot {
    LeasedSnapshot {
        lease_id: lease.lease_id,
        manifest_version: lease.manifest_version,
        root_hash: lease.root_hash,
        layer_paths: lease.layer_paths.into_iter().map(PathBuf::from).collect(),
    }
}

pub(super) fn manifest_version_u64(version: i64) -> Result<u64, LayerStackError> {
    u64::try_from(version).map_err(|_| {
        LayerStackError::Manifest(format!("manifest version must be non-negative: {version}"))
    })
}
