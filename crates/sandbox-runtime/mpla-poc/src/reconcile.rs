use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    witness_path: Option<PathBuf>,
    categories: Vec<usize>,
}

pub fn reconcile(
    scope_root: &Path,
    category_roots: &[StorageCategoryRoot],
    leaks: LeakCounts,
) -> PocResult<ReconciliationReceipt> {
    let mut category_paths: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for category_root in category_roots {
        require_within(scope_root, &category_root.root)?;
        category_paths
            .entry(category_root.category.clone())
            .or_default()
            .push(category_root.root.clone());
    }
    let category_names = category_paths.keys().cloned().collect::<Vec<_>>();
    let category_indices = category_names
        .iter()
        .enumerate()
        .map(|(index, category)| (category.as_str(), index))
        .collect::<HashMap<_, _>>();
    let indexed_roots = category_roots
        .iter()
        .map(|category_root| {
            let index = category_indices
                .get(category_root.category.as_str())
                .copied()
                .ok_or_else(|| {
                    PocError::Integrity(format!(
                        "reconciliation category {} has no index",
                        category_root.category
                    ))
                })?;
            Ok(IndexedCategoryRoot {
                index,
                root: &category_root.root,
                recursive: category_root.recursive,
            })
        })
        .collect::<PocResult<Vec<_>>>()?;
    let scope_objects = walk_reconciliation_scope(scope_root, &indexed_roots)?;

    let mut category_totals = vec![(0_u64, 0_u64, 0_u64); category_names.len()];
    let mut unexplained_paths = BTreeSet::new();
    let mut physical_union_allocated_bytes = 0_u64;
    let mut classified_allocated_bytes = 0_u64;
    let mut classified_inodes = 0_u64;
    for object in scope_objects.values() {
        physical_union_allocated_bytes =
            physical_union_allocated_bytes.saturating_add(object.allocated_bytes);
        if let Some(category) = object.categories.first().copied() {
            let totals = &mut category_totals[category];
            totals.0 = totals.0.saturating_add(object.allocated_bytes);
            totals.1 = totals.1.saturating_add(object.logical_bytes);
            totals.2 = totals.2.saturating_add(1);
            classified_allocated_bytes =
                classified_allocated_bytes.saturating_add(object.allocated_bytes);
            classified_inodes = classified_inodes.saturating_add(1);
        } else if let Some(witness_path) = &object.witness_path {
            unexplained_paths.insert(witness_path.clone());
            if unexplained_paths.len() > 64 {
                unexplained_paths.pop_last();
            }
        }
    }
    let categories = category_names
        .into_iter()
        .enumerate()
        .map(|(index, category)| {
            let (allocated_bytes, logical_bytes, unique_inodes) = category_totals[index];
            StorageCategoryReceipt {
                roots: category_paths.remove(&category).unwrap_or_default(),
                category,
                allocated_bytes,
                logical_bytes,
                unique_inodes,
            }
        })
        .collect::<Vec<_>>();

    let unexplained_allocated_bytes =
        physical_union_allocated_bytes.saturating_sub(classified_allocated_bytes);
    let physical_union_inodes = u64::try_from(scope_objects.len()).unwrap_or(u64::MAX);
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
        unexplained_paths: unexplained_paths.into_iter().collect(),
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

#[derive(Clone, Copy)]
struct IndexedCategoryRoot<'a> {
    index: usize,
    root: &'a Path,
    recursive: bool,
}

fn walk_reconciliation_scope(
    root: &Path,
    category_roots: &[IndexedCategoryRoot<'_>],
) -> PocResult<HashMap<(u64, u64), PhysicalObject>> {
    let mut objects = HashMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let is_directory = record_physical_path(&path, category_roots, &mut objects)?;
        if is_directory {
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

fn record_physical_path(
    path: &Path,
    category_roots: &[IndexedCategoryRoot<'_>],
    objects: &mut HashMap<(u64, u64), PhysicalObject>,
) -> PocResult<bool> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat reconciliation entry", path, source))?;
    let key = physical_key(&metadata);
    let mut categories = category_roots
        .iter()
        .filter(|category_root| {
            if category_root.recursive {
                path == category_root.root || path.starts_with(category_root.root)
            } else {
                path == category_root.root
            }
        })
        .map(|category_root| category_root.index)
        .collect::<Vec<_>>();
    categories.sort_unstable();
    categories.dedup();
    match objects.get_mut(&key) {
        Some(object) => {
            if !categories.is_empty() {
                object.categories.extend(categories);
                object.categories.sort_unstable();
                object.categories.dedup();
                object.witness_path = None;
            } else if object.categories.is_empty()
                && object
                    .witness_path
                    .as_ref()
                    .is_none_or(|witness| path < witness)
            {
                object.witness_path = Some(path.to_path_buf());
            }
        }
        None => {
            let witness_path = categories.is_empty().then(|| path.to_path_buf());
            objects.insert(
                key,
                PhysicalObject {
                    allocated_bytes: allocated_bytes(&metadata),
                    logical_bytes: metadata.len(),
                    witness_path,
                    categories,
                },
            );
        }
    }
    Ok(metadata.is_dir() && !metadata.file_type().is_symlink())
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
