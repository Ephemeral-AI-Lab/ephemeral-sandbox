use std::path::Path;

use serde::{Deserialize, Serialize};

/// Basic resource stats for a captured upperdir tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeResourceStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
}

/// Timing DTO local to ephemeral workspace policy. Carries the publish phase
/// duration captured during finalize; the daemon reads it as a fallback for the
/// OCC commit timing when the publisher does not report one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EphemeralTimings {
    pub publish_s: Option<f64>,
}

impl TreeResourceStats {
    #[must_use]
    pub fn collect(path: &Path) -> Self {
        let mut stats = Self::default();
        collect_path(path, &mut stats);
        stats
    }
}

fn collect_path(path: &Path, stats: &mut TreeResourceStats) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        stats.symlinks = stats.symlinks.saturating_add(1);
        return;
    }
    if file_type.is_file() {
        stats.files = stats.files.saturating_add(1);
        stats.bytes = stats.bytes.saturating_add(metadata.len());
        return;
    }
    if file_type.is_dir() {
        stats.dirs = stats.dirs.saturating_add(1);
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            collect_path(&entry.path(), stats);
        }
    }
}
