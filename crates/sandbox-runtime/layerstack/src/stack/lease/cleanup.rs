use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::Path;

use crate::error::LayerStackError;
use crate::fs::{
    layer_bytes_path, layer_digest_path, read_manifest, remove_path, validate_layer_ref,
};
use crate::model::{LayerRef, Manifest};
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, LAYER_METADATA_DIR, STAGING_DIR};

use super::registry::LeaseRegistry;

const BASE_LAYER_PREFIX: char = 'B';

/// Release `lease_id` and garbage-collect the layers only it referenced.
/// Returns `None` when the lease is unknown, otherwise the removed set —
/// the commit-time GC contract for squash's plan-lease release.
pub(in crate::stack) fn release_lease_locked(
    storage_root: &Path,
    leases: &mut LeaseRegistry,
    lease_id: &str,
) -> Result<Option<Vec<LayerRef>>, LayerStackError> {
    let Some(lease) = leases.release(lease_id) else {
        return Ok(None);
    };
    let active = read_manifest(storage_root.join(ACTIVE_MANIFEST_FILE))?;
    let removable = unreferenced_layers(&lease.manifest.layers, &active, leases);
    remove_layers(storage_root, &removable)?;
    Ok(Some(removable))
}

fn unreferenced_layers(
    candidates: &[LayerRef],
    active: &Manifest,
    leases: &LeaseRegistry,
) -> Vec<LayerRef> {
    let retained = leases.leased_layers();
    let retained: BTreeSet<&LayerRef> = retained.iter().collect();
    let active: BTreeSet<&LayerRef> = active.layers.iter().collect();
    candidates
        .iter()
        .filter(|layer| !active.contains(layer) && !retained.contains(layer))
        .cloned()
        .collect()
}

/// The one deletion routine shared by lease-release GC and the boot sweep:
/// the layer directory plus its `.digest` and `.bytes` sidecars.
fn remove_layers(storage_root: &Path, layers: &[LayerRef]) -> Result<(), LayerStackError> {
    for layer in layers {
        validate_layer_ref(layer)?;
        remove_path(&storage_root.join(&layer.path))?;
        remove_metadata_file(&layer_digest_path(storage_root, &layer.layer_id))?;
        remove_metadata_file(&layer_bytes_path(storage_root, &layer.layer_id))?;
    }
    Ok(())
}

fn remove_metadata_file(path: &Path) -> Result<(), LayerStackError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Boot storage sweep outcome: what was deleted, or why nothing was.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub removed_layer_ids: Vec<String>,
    pub removed_staging_entries: usize,
    pub skipped_reason: Option<String>,
}

/// Fail-closed boot storage sweep: with a parsed active manifest
/// (`version ≥ 1`, non-empty layers) as the keep-set, delete `staging/*`,
/// unreferenced `layers/*`, and orphan metadata sidecars through
/// [`remove_layers`]. `B*` ids are never deleted. A missing, unparsable, or
/// degenerate manifest deletes nothing and reports why.
pub(in crate::stack) fn sweep_storage_locked(
    storage_root: &Path,
) -> Result<SweepReport, LayerStackError> {
    let manifest_path = storage_root.join(ACTIVE_MANIFEST_FILE);
    if !manifest_path.exists() {
        return Ok(skipped("manifest.json is missing"));
    }
    let manifest = match read_manifest(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => return Ok(skipped(&format!("manifest.json unreadable: {error}"))),
    };
    if manifest.version < 1 || manifest.layers.is_empty() {
        return Ok(skipped("manifest is empty or pre-versioned"));
    }
    let keep: BTreeSet<&str> = manifest
        .layers
        .iter()
        .map(|layer| layer.layer_id.as_str())
        .collect();

    let mut candidate_ids: BTreeSet<String> = BTreeSet::new();
    for name in list_names(&storage_root.join(LAYERS_DIR))? {
        candidate_ids.insert(name);
    }
    for name in list_names(&storage_root.join(LAYER_METADATA_DIR))? {
        let id = name
            .strip_suffix(".digest")
            .or_else(|| name.strip_suffix(".bytes"));
        if let Some(id) = id {
            candidate_ids.insert(id.to_owned());
        }
    }
    let removable: Vec<LayerRef> = candidate_ids
        .into_iter()
        .filter(|id| !keep.contains(id.as_str()) && !id.starts_with(BASE_LAYER_PREFIX))
        .map(|id| LayerRef {
            path: format!("{LAYERS_DIR}/{id}"),
            layer_id: id,
        })
        .collect();
    remove_layers(storage_root, &removable)?;

    let mut removed_staging_entries = 0;
    let staging = storage_root.join(STAGING_DIR);
    if staging.is_dir() {
        for entry in std::fs::read_dir(&staging)? {
            remove_path(&entry?.path())?;
            removed_staging_entries += 1;
        }
    }
    Ok(SweepReport {
        removed_layer_ids: removable.into_iter().map(|layer| layer.layer_id).collect(),
        removed_staging_entries,
        skipped_reason: None,
    })
}

fn skipped(reason: &str) -> SweepReport {
    SweepReport {
        skipped_reason: Some(reason.to_owned()),
        ..SweepReport::default()
    }
}

fn list_names(dir: &Path) -> Result<Vec<String>, LayerStackError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}
