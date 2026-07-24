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

fn is_legacy_execution_id(value: &str) -> bool {
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

fn path_depth(root: &Path, child: &Path) -> usize {
    child
        .strip_prefix(root)
        .unwrap_or_else(|_| Path::new(""))
        .components()
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    fn test_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "legacy-scratch-reaper-{}-{label}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn legacy_id_is_canonical() {
        assert!(is_legacy_execution_id("namespace_execution_42"));
        assert!(!is_legacy_execution_id("namespace_execution_"));
        assert!(!is_legacy_execution_id("namespace_execution_01"));
        assert!(!is_legacy_execution_id("other_42"));
        assert!(!is_legacy_execution_id("namespace_execution_4/2"));
    }

    #[test]
    fn depth_is_bounded() {
        assert_eq!(
            path_depth(
                Path::new("/legacy/namespace_execution_1"),
                Path::new("/legacy/namespace_execution_1/transcript.log")
            ),
            1
        );
    }

    #[test]
    fn bounded_reaper_deletes_only_old_unowned_canonical_leaves() {
        let root = test_root("classification");
        let outside = test_root("outside");
        fs::create_dir_all(&root).expect("legacy root");
        fs::create_dir_all(&outside).expect("outside");

        let eligible = root.join("namespace_execution_1");
        fs::create_dir(&eligible).expect("eligible");
        fs::write(eligible.join("transcript.log"), b"old").expect("eligible transcript");

        let active = root.join("namespace_execution_2");
        fs::create_dir(&active).expect("active");
        let recent = root.join("namespace_execution_3");
        let symlink_entry = root.join("namespace_execution_4");
        symlink(&outside, &symlink_entry).expect("unsafe symlink");
        fs::create_dir(root.join("foreign")).expect("foreign");
        let nested = root.join("namespace_execution_5");
        fs::create_dir_all(nested.join("nested")).expect("nested foreign content");

        let active_ids = HashSet::from(["namespace_execution_2".to_owned()]);
        let future = SystemTime::now() + Duration::from_secs(2 * 60 * 60);
        let report =
            reap_legacy_execution_scratch(Some(&root), &active_ids, future, LEGACY_REAP_MIN_AGE);

        assert_eq!(report.deleted, 1);
        assert_eq!(report.skipped_active, 1);
        assert!(report.skipped_unsafe >= 3);
        assert!(!eligible.exists());
        assert!(active.exists());
        assert!(symlink_entry.exists());
        assert!(nested.exists());

        fs::create_dir(&recent).expect("recent");
        let recent_report = reap_legacy_execution_scratch(
            Some(&root),
            &HashSet::new(),
            SystemTime::now(),
            LEGACY_REAP_MIN_AGE,
        );
        assert!(recent_report.skipped_recent >= 2);

        fs::remove_dir_all(&root).expect("cleanup root");
        fs::remove_dir_all(&outside).expect("cleanup outside");
    }

    #[test]
    fn bounded_reaper_stops_after_1024_scanned_entries() {
        let root = test_root("bound");
        fs::create_dir_all(&root).expect("legacy root");
        for index in 0..=LEGACY_REAP_MAX_ENTRIES {
            fs::create_dir(root.join(format!("namespace_execution_{index}")))
                .expect("legacy entry");
        }
        let report = reap_legacy_execution_scratch(
            Some(&root),
            &HashSet::new(),
            SystemTime::now() + Duration::from_secs(2 * 60 * 60),
            LEGACY_REAP_MIN_AGE,
        );
        assert!(report.saturated);
        assert_eq!(report.scanned_entries, LEGACY_REAP_MAX_ENTRIES);
        assert!(fs::read_dir(&root)
            .expect("remaining entries")
            .next()
            .is_some());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
