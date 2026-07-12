//! Pure on-disk reader for LayerStack storage usage.
//!
//! This is a leaf collector: it depends on `std` + `serde_json` only and never
//! imports `sandbox-runtime-layerstack`. It duplicates the minimal manifest
//! shape it needs rather than crossing that crate boundary. Incomplete walks
//! and malformed state are reported as unavailable (`None`), never as zero.

use std::fs;
use std::path::{Path, PathBuf};

const ACTIVE_MANIFEST_FILE: &str = "manifest.json";
const LAYERS_DIR: &str = "layers";
const STAGING_DIR: &str = "staging";
const LAYER_METADATA_DIR: &str = ".layer-metadata";

use super::WalkBudget;

/// Logical and allocated byte size of one active layer, keyed by its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerBytes {
    pub layer_id: String,
    /// Sum of regular-file lengths. A valid sidecar may provide this even when
    /// the allocated-byte walk is unavailable.
    pub bytes: Option<u64>,
    /// Filesystem allocation (`st_blocks * 512`) where the host supports it.
    pub allocated_bytes: Option<u64>,
}

/// Storage usage for the active manifest and the complete LayerStack root.
///
/// Active-layer totals and storage-root totals intentionally remain separate:
/// the latter includes manifest, metadata, obsolete layers, and staging data
/// needed to observe squash peak allocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerStackBytes {
    pub layers: Vec<LayerBytes>,
    pub total_bytes: Option<u64>,
    pub total_allocated_bytes: Option<u64>,
    pub storage_logical_bytes: Option<u64>,
    pub storage_allocated_bytes: Option<u64>,
    pub staging_entry_count: Option<u64>,
}

/// Sample active layers and the full storage root under `storage_root`.
///
/// A missing logical-size sidecar triggers a budgeted layer walk. Sidecars are
/// repopulated only after a complete walk, so a truncated observation cannot be
/// cached as an authoritative size. The full-root walk uses the same injected
/// budget and publishes no totals when it cannot finish.
#[must_use]
pub fn sample_layerstack(storage_root: &Path, budget: WalkBudget) -> LayerStackBytes {
    let root_usage = walk_usage(storage_root, budget);
    let storage_logical_bytes = root_usage.complete.then_some(root_usage.logical_bytes);
    let storage_allocated_bytes = root_usage
        .complete
        .then_some(root_usage.allocated_bytes)
        .flatten();
    let staging_entry_count = immediate_entry_count(&storage_root.join(STAGING_DIR));

    let Some(layer_ids) = read_manifest_layer_ids(storage_root) else {
        return LayerStackBytes {
            storage_logical_bytes,
            storage_allocated_bytes,
            staging_entry_count,
            ..LayerStackBytes::default()
        };
    };

    let layers = layer_ids
        .into_iter()
        .map(|layer_id| layer_bytes(storage_root, layer_id, budget))
        .collect::<Vec<_>>();
    let total_bytes = checked_total(layers.iter().map(|layer| layer.bytes));
    let total_allocated_bytes = checked_total(layers.iter().map(|layer| layer.allocated_bytes));

    LayerStackBytes {
        layers,
        total_bytes,
        total_allocated_bytes,
        storage_logical_bytes,
        storage_allocated_bytes,
        staging_entry_count,
    }
}

fn read_manifest_layer_ids(storage_root: &Path) -> Option<Vec<String>> {
    let raw = fs::read_to_string(storage_root.join(ACTIVE_MANIFEST_FILE)).ok()?;
    let document: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let entries = document.get("layers")?.as_array()?;
    let mut ids = Vec::with_capacity(entries.len());
    for entry in entries {
        ids.push(entry.get("layer_id")?.as_str()?.to_owned());
    }
    Some(ids)
}

fn layer_bytes(storage_root: &Path, layer_id: String, budget: WalkBudget) -> LayerBytes {
    let cached_bytes = read_bytes_sidecar(storage_root, &layer_id);
    let usage = walk_usage(&storage_root.join(LAYERS_DIR).join(&layer_id), budget);
    let bytes = cached_bytes.or_else(|| usage.complete.then_some(usage.logical_bytes));
    if cached_bytes.is_none() && usage.complete {
        let _ = write_bytes_sidecar(storage_root, &layer_id, usage.logical_bytes);
    }
    LayerBytes {
        layer_id,
        bytes,
        allocated_bytes: usage.complete.then_some(usage.allocated_bytes).flatten(),
    }
}

fn checked_total(mut values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    values.try_fold(0_u64, |total, value| total.checked_add(value?))
}

fn immediate_entry_count(path: &Path) -> Option<u64> {
    let entries = fs::read_dir(path).ok()?;
    let mut count = 0_u64;
    for entry in entries {
        entry.ok()?;
        count = count.checked_add(1)?;
    }
    Some(count)
}

fn bytes_sidecar_path(storage_root: &Path, layer_id: &str) -> PathBuf {
    storage_root
        .join(LAYER_METADATA_DIR)
        .join(format!("{layer_id}.bytes"))
}

fn read_bytes_sidecar(storage_root: &Path, layer_id: &str) -> Option<u64> {
    fs::read_to_string(bytes_sidecar_path(storage_root, layer_id))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

fn write_bytes_sidecar(storage_root: &Path, layer_id: &str, bytes: u64) -> std::io::Result<()> {
    let dir = storage_root.join(LAYER_METADATA_DIR);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(format!("{layer_id}.bytes")), bytes.to_string())
}

#[derive(Debug, Clone, Copy, Default)]
struct WalkUsage {
    logical_bytes: u64,
    allocated_bytes: Option<u64>,
    complete: bool,
}

fn walk_usage(root: &Path, budget: WalkBudget) -> WalkUsage {
    let mut usage = WalkUsage {
        allocated_bytes: allocated_zero(),
        complete: true,
        ..WalkUsage::default()
    };
    let mut stack = vec![(root.to_path_buf(), 0_usize)];
    let mut visited = 0_usize;

    while let Some((current, depth)) = stack.pop() {
        if visited >= budget.max_nodes {
            usage.complete = false;
            break;
        }
        visited += 1;
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(_) => {
                usage.complete = false;
                continue;
            }
        };
        add_allocated(&mut usage.allocated_bytes, allocated_bytes(&metadata));
        let file_type = metadata.file_type();
        if file_type.is_file() {
            usage.logical_bytes = usage.logical_bytes.saturating_add(metadata.len());
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }

        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => {
                usage.complete = false;
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    usage.complete = false;
                    continue;
                }
            };
            if depth >= budget.max_depth || visited.saturating_add(stack.len()) >= budget.max_nodes
            {
                usage.complete = false;
                continue;
            }
            stack.push((entry.path(), depth.saturating_add(1)));
        }
    }
    usage
}

fn add_allocated(total: &mut Option<u64>, amount: Option<u64>) {
    *total = match (*total, amount) {
        (Some(total), Some(amount)) => total.checked_add(amount),
        _ => None,
    };
}

#[cfg(unix)]
fn allocated_zero() -> Option<u64> {
    Some(0)
}

#[cfg(not(unix))]
fn allocated_zero() -> Option<u64> {
    None
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    metadata.blocks().checked_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(_metadata: &fs::Metadata) -> Option<u64> {
    None
}
