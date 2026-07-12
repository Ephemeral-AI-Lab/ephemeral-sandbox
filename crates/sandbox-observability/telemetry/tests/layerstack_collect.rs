use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_observability_telemetry::{sample_layerstack, LayerBytes, WalkBudget};

type TestResult = Result<(), Box<dyn Error>>;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TempStorage {
    root: PathBuf,
}

impl TempStorage {
    fn new(label: &str) -> std::io::Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "sandbox-obs-layerstack-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn write_manifest(&self, layer_ids: &[&str]) -> std::io::Result<()> {
        let entries: Vec<String> = layer_ids
            .iter()
            .map(|id| format!("{{\"layer_id\":\"{id}\",\"path\":\"layers/{id}\"}}"))
            .collect();
        let body = format!(
            "{{\"schema_version\":1,\"version\":1,\"layers\":[{}]}}",
            entries.join(",")
        );
        fs::write(self.root.join("manifest.json"), body)
    }

    fn write_layer_file(&self, layer_id: &str, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        let dir = self.root.join("layers").join(layer_id);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(name), bytes)
    }

    fn write_sidecar(&self, layer_id: &str, bytes: u64) -> std::io::Result<()> {
        let dir = self.root.join(".layer-metadata");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(format!("{layer_id}.bytes")), bytes.to_string())
    }

    fn sidecar(&self, layer_id: &str) -> Option<String> {
        fs::read_to_string(
            self.root
                .join(".layer-metadata")
                .join(format!("{layer_id}.bytes")),
        )
        .ok()
    }
}

impl Drop for TempStorage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn sidecar_size_is_used_without_walking() -> TestResult {
    let storage = TempStorage::new("sidecar-only")?;
    storage.write_manifest(&["L1"])?;
    // Sidecar present but the layer directory is absent: a walk would yield 0,
    // so a 5000-byte result proves the sidecar was used.
    storage.write_sidecar("L1", 5000)?;

    let observed = sample_layerstack(storage.root(), WalkBudget::default());

    assert_eq!(observed.total_bytes, Some(5000));
    assert_eq!(observed.total_allocated_bytes, None);
    assert_eq!(
        observed.layers,
        vec![LayerBytes {
            layer_id: "L1".to_owned(),
            bytes: Some(5000),
            allocated_bytes: None,
        }]
    );
    Ok(())
}

#[test]
fn missing_sidecar_walks_and_repopulates() -> TestResult {
    let storage = TempStorage::new("walk-fallback")?;
    storage.write_manifest(&["L2"])?;
    storage.write_layer_file("L2", "a.txt", &[0_u8; 10])?;
    assert!(
        storage.sidecar("L2").is_none(),
        "sidecar absent before sample"
    );

    let observed = sample_layerstack(storage.root(), WalkBudget::default());

    assert_eq!(observed.total_bytes, Some(10));
    assert_eq!(
        observed.layers,
        vec![LayerBytes {
            layer_id: "L2".to_owned(),
            bytes: Some(10),
            allocated_bytes: observed.layers[0].allocated_bytes,
        }]
    );
    #[cfg(unix)]
    assert!(observed.total_allocated_bytes.is_some());
    assert_eq!(
        storage.sidecar("L2").as_deref(),
        Some("10"),
        "walk repopulates the sidecar"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn complete_walk_reports_exact_active_and_full_root_allocations() -> TestResult {
    let storage = TempStorage::new("complete-allocated")?;
    storage.write_manifest(&["L5"])?;
    storage.write_layer_file("L5", "active.bin", &[0_u8; 4_097])?;
    storage.write_sidecar("L5", 4_097)?;
    storage.write_layer_file("obsolete", "retained.bin", &[0_u8; 2_111])?;
    let staging = storage.root().join("staging");
    fs::create_dir_all(&staging)?;
    fs::write(staging.join("pending.bin"), [0_u8; 17])?;

    let active_usage = unix_tree_usage(&storage.root().join("layers/L5"))?;
    let root_usage = unix_tree_usage(storage.root())?;
    let observed = sample_layerstack(storage.root(), WalkBudget::default());

    assert_eq!(observed.layers[0].bytes, Some(4_097));
    assert_eq!(observed.layers[0].allocated_bytes, Some(active_usage.1));
    assert_eq!(observed.total_bytes, Some(4_097));
    assert_eq!(observed.total_allocated_bytes, Some(active_usage.1));
    assert_eq!(observed.storage_logical_bytes, Some(root_usage.0));
    assert_eq!(observed.storage_allocated_bytes, Some(root_usage.1));
    assert!(
        root_usage.0 > 4_097,
        "full root includes retained/staging metadata"
    );
    assert!(
        root_usage.1 >= active_usage.1,
        "full-root allocation contains the active layer allocation"
    );
    assert_eq!(observed.staging_entry_count, Some(1));
    Ok(())
}

#[test]
fn cached_sidecar_keeps_layer_sized_once() -> TestResult {
    let storage = TempStorage::new("sized-once")?;
    storage.write_manifest(&["L3"])?;
    storage.write_layer_file("L3", "a.txt", &[0_u8; 10])?;

    let first = sample_layerstack(storage.root(), WalkBudget::default());
    assert_eq!(first.total_bytes, Some(10));

    // Grow the layer on disk; the cached sidecar must still win, so the layer is
    // sized exactly once.
    storage.write_layer_file("L3", "b.txt", &[0_u8; 90])?;
    let second = sample_layerstack(storage.root(), WalkBudget::default());
    assert_eq!(second.total_bytes, Some(10));
    Ok(())
}

#[test]
fn half_written_manifest_is_skipped_without_panic() -> TestResult {
    let storage = TempStorage::new("half-written")?;
    let malformed = "{\"layers\":[ {\"lay";
    fs::write(storage.root().join("manifest.json"), malformed)?;

    let observed = sample_layerstack(storage.root(), WalkBudget::default());

    assert!(observed.layers.is_empty());
    assert_eq!(observed.total_bytes, None);
    assert_eq!(observed.total_allocated_bytes, None);
    assert_eq!(observed.storage_logical_bytes, Some(malformed.len() as u64));
    assert_eq!(observed.staging_entry_count, None);
    #[cfg(unix)]
    assert_eq!(
        observed.storage_allocated_bytes,
        Some(unix_tree_usage(storage.root())?.1)
    );
    #[cfg(not(unix))]
    assert_eq!(observed.storage_allocated_bytes, None);
    Ok(())
}

#[test]
fn missing_manifest_is_empty() -> TestResult {
    let storage = TempStorage::new("missing-manifest")?;

    let observed = sample_layerstack(storage.root(), WalkBudget::default());

    assert!(observed.layers.is_empty());
    assert_eq!(observed.total_bytes, None);
    assert_eq!(observed.total_allocated_bytes, None);
    assert_eq!(observed.storage_logical_bytes, Some(0));
    assert_eq!(observed.staging_entry_count, None);
    #[cfg(unix)]
    assert_eq!(
        observed.storage_allocated_bytes,
        Some(unix_tree_usage(storage.root())?.1)
    );
    #[cfg(not(unix))]
    assert_eq!(observed.storage_allocated_bytes, None);
    Ok(())
}

#[test]
fn truncated_walk_is_unavailable_and_is_not_cached() -> TestResult {
    let storage = TempStorage::new("truncated")?;
    storage.write_manifest(&["L4"])?;
    storage.write_layer_file("L4", "a.txt", &[0_u8; 10])?;

    let observed = sample_layerstack(
        storage.root(),
        WalkBudget {
            max_nodes: 1,
            max_depth: 64,
        },
    );

    assert_eq!(observed.layers[0].bytes, None);
    assert_eq!(observed.layers[0].allocated_bytes, None);
    assert_eq!(observed.total_bytes, None);
    assert_eq!(observed.storage_allocated_bytes, None);
    assert!(storage.sidecar("L4").is_none());
    Ok(())
}

#[test]
fn staging_residue_is_counted_without_collapsing_missing_directory_to_zero() -> TestResult {
    let storage = TempStorage::new("staging")?;
    storage.write_manifest(&[])?;

    let missing = sample_layerstack(storage.root(), WalkBudget::default());
    assert_eq!(missing.staging_entry_count, None);

    let staging = storage.root().join("staging");
    fs::create_dir_all(&staging)?;
    fs::write(staging.join("one.staging"), b"x")?;
    fs::create_dir(staging.join("two.staging"))?;

    let observed = sample_layerstack(storage.root(), WalkBudget::default());
    assert_eq!(observed.staging_entry_count, Some(2));
    assert_eq!(observed.total_bytes, Some(0));
    assert_eq!(observed.total_allocated_bytes, Some(0));
    Ok(())
}

#[cfg(unix)]
fn unix_tree_usage(root: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    let mut logical = 0_u64;
    let mut allocated = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        allocated = allocated
            .checked_add(metadata.blocks().checked_mul(512).expect("block bytes"))
            .expect("allocated total");
        if metadata.is_file() {
            logical = logical.checked_add(metadata.len()).expect("logical total");
        } else if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok((logical, allocated))
}
