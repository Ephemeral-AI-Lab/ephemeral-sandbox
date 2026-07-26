//! Bounded, synchronous compatibility cleanup for the retired global
//! namespace-execution scratch layout.

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, SystemTime};

pub(crate) const LEGACY_REAP_MAX_ENTRIES: usize = 1024;
pub(crate) const LEGACY_REAP_MAX_DEPTH: usize = 3;
pub(crate) const LEGACY_REAP_MIN_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyScratchReapReport {
    pub root_configured: bool,
    pub scanned_entries: usize,
    pub deleted: usize,
    pub skipped_active: usize,
    pub skipped_recent: usize,
    pub skipped_unsafe: usize,
    pub errors: usize,
    pub saturated: bool,
}

pub(crate) fn reap_legacy_execution_scratch(
    root: Option<&Path>,
    active_execution_ids: &HashSet<String>,
    now: SystemTime,
    minimum_age: Duration,
) -> LegacyScratchReapReport {
    let mut report = LegacyScratchReapReport {
        root_configured: root.is_some(),
        ..LegacyScratchReapReport::default()
    };
    let Some(root) = root else {
        return report;
    };
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return report,
        Err(_) => {
            report.errors += 1;
            return report;
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() || !root.is_absolute() {
        report.skipped_unsafe += 1;
        return report;
    }
    let canonical_root = match root.canonicalize() {
        Ok(path) => path,
        Err(_) => {
            report.errors += 1;
            return report;
        }
    };
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => {
            report.errors += 1;
            return report;
        }
    };
    for entry in entries {
        if report.scanned_entries >= LEGACY_REAP_MAX_ENTRIES {
            report.saturated = true;
            break;
        }
        report.scanned_entries += 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                report.errors += 1;
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_legacy_execution_id(&name) {
            report.skipped_unsafe += 1;
            continue;
        }
        if active_execution_ids.contains(&name) {
            report.skipped_active += 1;
            continue;
        }
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                report.errors += 1;
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            report.skipped_unsafe += 1;
            continue;
        }
        if !old_enough(&metadata, now, minimum_age) {
            report.skipped_recent += 1;
            continue;
        }
        if !safe_legacy_leaf(&path, &canonical_root, &mut report) {
            report.skipped_unsafe += 1;
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => report.deleted += 1,
            Err(_) => report.errors += 1,
        }
    }
    report
}

pub(crate) fn is_legacy_execution_id(value: &str) -> bool {
    value
        .strip_prefix("namespace_execution_")
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
                && (suffix == "0" || !suffix.starts_with('0'))
        })
}

fn old_enough(metadata: &fs::Metadata, now: SystemTime, minimum_age: Duration) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= minimum_age)
}

fn safe_legacy_leaf(
    path: &Path,
    canonical_root: &Path,
    report: &mut LegacyScratchReapReport,
) -> bool {
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(_) => return false,
    };
    if !canonical.starts_with(canonical_root) || canonical.parent() != Some(canonical_root) {
        return false;
    }
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries {
        if report.scanned_entries >= LEGACY_REAP_MAX_ENTRIES {
            report.saturated = true;
            return false;
        }
        report.scanned_entries += 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return false,
        };
        let child = entry.path();
        let metadata = match fs::symlink_metadata(&child) {
            Ok(metadata) => metadata,
            Err(_) => return false,
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || child.file_name().and_then(|name| name.to_str()) != Some("transcript.log")
            || path_depth(path, &child) > LEGACY_REAP_MAX_DEPTH
        {
            return false;
        }
    }
    true
}

pub(crate) fn path_depth(root: &Path, child: &Path) -> usize {
    child
        .strip_prefix(root)
        .unwrap_or_else(|_| Path::new(""))
        .components()
        .count()
}
