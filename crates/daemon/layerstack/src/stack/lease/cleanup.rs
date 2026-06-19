use std::io::ErrorKind;
use std::path::Path;

use crate::error::LayerStackError;
use crate::fs::{layer_digest_path, read_manifest, remove_path, validate_layer_ref};
use crate::model::{LayerRef, Manifest};
use crate::ACTIVE_MANIFEST_FILE;

use super::registry::LeaseRegistry;

pub(in crate::stack) fn release_lease_locked(
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

pub(in crate::stack) fn retarget_lease_locked(
    storage_root: &Path,
    leases: &mut LeaseRegistry,
    lease_id: &str,
    manifest: Manifest,
) -> Result<bool, LayerStackError> {
    let Some(old_lease) = leases.retarget(lease_id, manifest) else {
        return Ok(false);
    };
    let active = read_manifest(storage_root.join(ACTIVE_MANIFEST_FILE))?;
    let removable = unreferenced_layers(&old_lease.manifest.layers, &active, leases);
    remove_layers(storage_root, &removable)?;
    Ok(true)
}

pub(in crate::stack) fn remove_unreferenced_layer_candidates_locked(
    storage_root: &Path,
    leases: &LeaseRegistry,
    candidates: &[LayerRef],
) -> Result<Vec<LayerRef>, LayerStackError> {
    let active = read_manifest(storage_root.join(ACTIVE_MANIFEST_FILE))?;
    let removable = unreferenced_layers(candidates, &active, leases);
    remove_layers(storage_root, &removable)?;
    Ok(removable)
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
        match std::fs::remove_file(layer_digest_path(storage_root, &layer.layer_id)) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}
