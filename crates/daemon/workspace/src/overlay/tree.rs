use std::path::Path;

pub const DEFAULT_TREE_WALK_ENTRY_LIMIT: usize = 50_000;

/// Basic resource stats for a captured upperdir tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeResourceStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
    pub truncated: bool,
    pub read_error_count: u64,
    pub first_error_path: Option<String>,
}

impl TreeResourceStats {
    #[must_use]
    pub fn collect(path: &Path) -> Self {
        Self::collect_with_entry_limit(path, DEFAULT_TREE_WALK_ENTRY_LIMIT)
    }

    #[must_use]
    pub fn collect_with_entry_limit(path: &Path, max_entries: usize) -> Self {
        let mut stats = Self::default();
        let mut remaining_entries = max_entries;
        collect_path(path, &mut stats, &mut remaining_entries);
        stats
    }
}

impl From<layerstack::CaptureStats> for TreeResourceStats {
    fn from(stats: layerstack::CaptureStats) -> Self {
        Self {
            files: stats.files,
            dirs: stats.dirs,
            symlinks: stats.symlinks,
            bytes: stats.bytes,
            truncated: stats.truncated,
            read_error_count: stats.read_error_count,
            first_error_path: stats.first_error_path,
        }
    }
}

/// Count regular-file bytes in a directory tree.
#[must_use]
pub fn directory_file_bytes(path: &Path) -> u64 {
    TreeResourceStats::collect(path).bytes
}

fn collect_path(path: &Path, stats: &mut TreeResourceStats, remaining_entries: &mut usize) {
    if *remaining_entries == 0 {
        stats.truncated = true;
        return;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            record_read_error(stats, path);
            return;
        }
    };
    *remaining_entries = remaining_entries.saturating_sub(1);
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        stats.symlinks = stats.symlinks.saturating_add(1);
    } else if file_type.is_file() {
        stats.files = stats.files.saturating_add(1);
        stats.bytes = stats.bytes.saturating_add(metadata.len());
    } else if file_type.is_dir() {
        stats.dirs = stats.dirs.saturating_add(1);
        match std::fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => collect_path(&entry.path(), stats, remaining_entries),
                        Err(_) => record_read_error(stats, path),
                    }
                }
            }
            Err(_) => record_read_error(stats, path),
        }
    }
}

fn record_read_error(stats: &mut TreeResourceStats, path: &Path) {
    stats.read_error_count = stats.read_error_count.saturating_add(1);
    if stats.first_error_path.is_none() {
        stats.first_error_path = Some(path.display().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::TreeResourceStats;

    #[test]
    fn collect_with_entry_limit_marks_real_truncation() {
        let root =
            std::env::temp_dir().join(format!("workspace-tree-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dir")).expect("create tree");
        std::fs::write(root.join("a.txt"), b"a").expect("write file");
        std::fs::write(root.join("dir").join("b.txt"), b"b").expect("write nested file");

        let stats = TreeResourceStats::collect_with_entry_limit(&root, 2);

        assert!(stats.truncated);
        assert!(stats.dirs >= 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn collect_records_first_failing_path() {
        let root =
            std::env::temp_dir().join(format!("workspace-tree-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let stats = TreeResourceStats::collect(&root);

        assert_eq!(stats.read_error_count, 1);
        assert_eq!(stats.first_error_path, Some(root.display().to_string()));
    }
}
