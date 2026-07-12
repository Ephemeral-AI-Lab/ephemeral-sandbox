//! Pure upperdir disk reader: a budgeted DFS that returns a `DiskSample`. A leaf
//! collector — `std` only. The daemon packs the result into `Sample.metrics`.

use std::fs;
use std::path::{Path, PathBuf};

use super::WalkBudget;

/// Byte/entry-count totals of an upperdir walk, with budget-truncation and the
/// first read error encountered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskSample {
    pub upperdir_bytes: Option<i64>,
    /// Filesystem allocation (`st_blocks * 512`) for the complete walk. This
    /// is `None` on unsupported platforms and after any read/budget failure;
    /// a partial allocation is never presented as an authoritative zero/total.
    pub upperdir_allocated_bytes: Option<i64>,
    pub file_count: Option<i64>,
    pub dir_count: Option<i64>,
    pub symlink_count: Option<i64>,
    pub truncated: Option<bool>,
    pub read_error_count: Option<i64>,
    pub first_error_path: Option<String>,
}

impl DiskSample {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Walk `path` (budgeted DFS) and total its bytes and entry counts.
#[must_use]
pub fn sample_upperdir(path: &Path, budget: WalkBudget) -> DiskSample {
    let mut sample = DiskSample {
        upperdir_bytes: Some(0),
        upperdir_allocated_bytes: allocated_zero(),
        file_count: Some(0),
        dir_count: Some(0),
        symlink_count: Some(0),
        truncated: Some(false),
        read_error_count: Some(0),
        first_error_path: None,
    };
    let mut stack = vec![(path.to_path_buf(), 0usize)];
    let mut visited_nodes = 0usize;

    'walk: while let Some((current, depth)) = stack.pop() {
        if visited_nodes >= budget.max_nodes {
            mark_truncated(&mut sample);
            break;
        }
        visited_nodes += 1;
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) => {
                record_error(&mut sample, &current, error);
                continue;
            }
        };
        add_allocated(
            &mut sample.upperdir_allocated_bytes,
            allocated_bytes(&metadata),
        );
        let file_type = metadata.file_type();
        if file_type.is_file() {
            add(
                &mut sample.upperdir_bytes,
                i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            );
            add(&mut sample.file_count, 1);
        } else if file_type.is_dir() {
            add(&mut sample.dir_count, 1);
            if depth >= budget.max_depth {
                mark_truncated(&mut sample);
                continue;
            }
            let entries = match fs::read_dir(&current) {
                Ok(entries) => entries,
                Err(error) => {
                    record_error(&mut sample, &current, error);
                    continue;
                }
            };
            for entry in entries {
                match entry {
                    Ok(entry) => {
                        if visited_nodes.saturating_add(stack.len()) >= budget.max_nodes {
                            mark_truncated(&mut sample);
                            break 'walk;
                        }
                        stack.push((entry.path(), depth.saturating_add(1)));
                    }
                    Err(error) => record_error(&mut sample, &current, error),
                }
            }
        } else if file_type.is_symlink() {
            add(&mut sample.symlink_count, 1);
        }
    }

    sample
}

fn add(value: &mut Option<i64>, amount: i64) {
    *value = Some(value.unwrap_or_default().saturating_add(amount));
}

fn add_allocated(total: &mut Option<i64>, amount: Option<i64>) {
    *total = match (*total, amount) {
        (Some(total), Some(amount)) => total.checked_add(amount),
        _ => None,
    };
}

fn mark_truncated(sample: &mut DiskSample) {
    sample.truncated = Some(true);
    sample.upperdir_allocated_bytes = None;
}

fn record_error(sample: &mut DiskSample, path: &Path, error: std::io::Error) {
    add(&mut sample.read_error_count, 1);
    sample.upperdir_allocated_bytes = None;
    if sample.first_error_path.is_none() {
        sample.first_error_path = Some(first_error(path, error));
    }
}

fn first_error(path: &Path, error: std::io::Error) -> String {
    let path = path_string(path);
    format!("{path}: {error}")
}

fn path_string(path: &Path) -> String {
    PathBuf::from(path).to_string_lossy().into_owned()
}

#[cfg(unix)]
fn allocated_zero() -> Option<i64> {
    Some(0)
}

#[cfg(not(unix))]
fn allocated_zero() -> Option<i64> {
    None
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;

    let blocks = i64::try_from(metadata.blocks()).ok()?;
    blocks.checked_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(_metadata: &fs::Metadata) -> Option<i64> {
    None
}
