use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{PocError, PocResult, SCHEMA_VERSION};

#[derive(Clone, Debug)]
pub struct StorageCategoryRoot {
    pub category: String,
    pub root: PathBuf,
    /// When false, classify only the root inode (useful for the scope
    /// directory itself without masking unknown descendants).
    pub recursive: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageCategoryReceipt {
    pub category: String,
    pub roots: Vec<PathBuf>,
    pub allocated_bytes: u64,
    pub logical_bytes: u64,
    pub unique_inodes: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeakCounts {
    pub active_leases: u64,
    pub active_mounts: u64,
    pub writable_payload_fds: u64,
    pub locator_readers: u64,
    pub retirement_debt_objects: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReconciliationReceipt {
    pub schema_version: u32,
    pub scope_root: PathBuf,
    pub categories: Vec<StorageCategoryReceipt>,
    pub physical_union_allocated_bytes: u64,
    pub classified_allocated_bytes: u64,
    pub unexplained_allocated_bytes: u64,
    pub physical_union_inodes: u64,
    pub classified_inodes: u64,
    pub unexplained_inodes: u64,
    pub unexplained_paths: Vec<PathBuf>,
    pub leaks: LeakCounts,
    pub balanced: bool,
}

#[derive(Clone, Debug)]
struct PhysicalObject {
    allocated_bytes: u64,
    logical_bytes: u64,
    witness_path: PathBuf,
}

pub fn reconcile(
    scope_root: &Path,
    category_roots: &[StorageCategoryRoot],
    leaks: LeakCounts,
) -> PocResult<ReconciliationReceipt> {
    let scope_objects = walk_physical_union(scope_root, true)?;
    let mut object_categories: BTreeMap<(u64, u64), BTreeSet<String>> = BTreeMap::new();
    let mut category_paths: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for category_root in category_roots {
        require_within(scope_root, &category_root.root)?;
        category_paths
            .entry(category_root.category.clone())
            .or_default()
            .push(category_root.root.clone());
        for key in walk_physical_union(&category_root.root, category_root.recursive)?.keys() {
            object_categories
                .entry(*key)
                .or_default()
                .insert(category_root.category.clone());
        }
    }

    let mut category_totals: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    let mut classified_keys = BTreeSet::new();
    let mut unexplained_paths = Vec::new();
    for (key, object) in &scope_objects {
        if let Some(categories) = object_categories.get(key) {
            let category = categories.iter().next().expect("non-empty category set");
            let totals = category_totals.entry(category.clone()).or_default();
            totals.0 = totals.0.saturating_add(object.allocated_bytes);
            totals.1 = totals.1.saturating_add(object.logical_bytes);
            totals.2 = totals.2.saturating_add(1);
            classified_keys.insert(*key);
        } else if unexplained_paths.len() < 64 {
            unexplained_paths.push(object.witness_path.clone());
        }
    }
    let categories = category_paths
        .into_iter()
        .map(|(category, roots)| {
            let (allocated_bytes, logical_bytes, unique_inodes) =
                category_totals.get(&category).copied().unwrap_or_default();
            StorageCategoryReceipt {
                category,
                roots,
                allocated_bytes,
                logical_bytes,
                unique_inodes,
            }
        })
        .collect::<Vec<_>>();

    let physical_union_allocated_bytes = sum_allocated(scope_objects.values());
    let classified_allocated_bytes = classified_keys
        .iter()
        .filter_map(|key| scope_objects.get(key))
        .map(|object| object.allocated_bytes)
        .sum();
    let unexplained_allocated_bytes =
        physical_union_allocated_bytes.saturating_sub(classified_allocated_bytes);
    let physical_union_inodes = u64::try_from(scope_objects.len()).unwrap_or(u64::MAX);
    let classified_inodes = u64::try_from(classified_keys.len()).unwrap_or(u64::MAX);
    let unexplained_inodes = physical_union_inodes.saturating_sub(classified_inodes);
    let no_leaks = leaks == LeakCounts::default();
    Ok(ReconciliationReceipt {
        schema_version: SCHEMA_VERSION,
        scope_root: scope_root.to_path_buf(),
        categories,
        physical_union_allocated_bytes,
        classified_allocated_bytes,
        unexplained_allocated_bytes,
        physical_union_inodes,
        classified_inodes,
        unexplained_inodes,
        unexplained_paths,
        leaks,
        balanced: unexplained_allocated_bytes == 0 && unexplained_inodes == 0 && no_leaks,
    })
}

fn require_within(scope_root: &Path, category_root: &Path) -> PocResult<()> {
    let scope = std::fs::canonicalize(scope_root)
        .map_err(|source| PocError::io("canonicalize reconciliation scope", scope_root, source))?;
    let category = std::fs::canonicalize(category_root).map_err(|source| {
        PocError::io(
            "canonicalize reconciliation category",
            category_root,
            source,
        )
    })?;
    if category != scope && !category.starts_with(&scope) {
        return Err(PocError::Integrity(format!(
            "reconciliation category {} escapes scope {}",
            category.display(),
            scope.display()
        )));
    }
    Ok(())
}

fn walk_physical_union(
    root: &Path,
    recursive: bool,
) -> PocResult<BTreeMap<(u64, u64), PhysicalObject>> {
    let mut objects = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|source| PocError::io("stat reconciliation entry", &path, source))?;
        let key = physical_key(&metadata);
        objects.entry(key).or_insert_with(|| PhysicalObject {
            allocated_bytes: allocated_bytes(&metadata),
            logical_bytes: metadata.len(),
            witness_path: path.clone(),
        });
        if recursive && metadata.is_dir() && !metadata.file_type().is_symlink() {
            let entries = std::fs::read_dir(&path)
                .map_err(|source| PocError::io("read reconciliation directory", &path, source))?;
            for entry in entries {
                let entry = entry
                    .map_err(|source| PocError::io("read reconciliation entry", &path, source))?;
                pending.push(entry.path());
            }
        }
    }
    Ok(objects)
}

fn sum_allocated<'a>(objects: impl Iterator<Item = &'a PhysicalObject>) -> u64 {
    objects
        .map(|object| object.allocated_bytes)
        .fold(0_u64, u64::saturating_add)
}

#[cfg(unix)]
fn physical_key(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn physical_key(metadata: &std::fs::Metadata) -> (u64, u64) {
    (metadata.len(), metadata.is_dir().into())
}

#[cfg(unix)]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
}
