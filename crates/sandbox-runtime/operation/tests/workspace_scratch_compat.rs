#![cfg(unix)]

#[path = "../src/workspace_scratch_compat.rs"]
mod subject;

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use subject::*;

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
        fs::create_dir(root.join(format!("namespace_execution_{index}"))).expect("legacy entry");
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
