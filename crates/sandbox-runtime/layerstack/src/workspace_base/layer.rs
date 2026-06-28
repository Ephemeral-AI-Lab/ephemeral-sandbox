use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::error::LayerStackError;
use crate::fs::{join_layer_path, remove_path};
use crate::model::{hex_lower, LayerRef};
use crate::{LAYERS_DIR, STAGING_DIR};

use super::collect::{base_root_hash, format_path_sample, relative_path, BaseEntry};

const WORKSPACE_BASE_LAYER_ID: &str = "B000001-base";
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
const COPY_BUFFER_BYTES: usize = 1024 * 1024;

/// Fixed worker count for the base-layer build (directory walk and file copy).
/// Both phases are bound by per-file `readdir`/`open`/`read` latency on the
/// bind-mounted workspace, not by CPU, so this is sized for I/O concurrency
/// (overlapping in-flight syscalls) rather than core count; the cores stay
/// idle-waiting while the syscalls round-trip.
const BASE_BUILD_WORKER_THREADS: usize = 32;

pub(super) fn build_base_layer(
    stack: &Path,
    workspace: &Path,
) -> Result<(LayerRef, String), LayerStackError> {
    let layer_id = WORKSPACE_BASE_LAYER_ID;
    let layer_dir = stack.join(LAYERS_DIR).join(layer_id);
    let staging_dir = stack.join(STAGING_DIR).join(format!("{layer_id}.staging"));
    if layer_dir.exists() || staging_dir.exists() {
        return Err(LayerStackError::Storage(format!(
            "base layer already exists: {}",
            layer_dir.display()
        )));
    }
    std::fs::create_dir_all(&staging_dir)?;
    let result = (|| {
        let stats = BuildStats::new();
        stats.emit(format!(
            "copying workspace {} into base layer",
            workspace.display()
        ));
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(BASE_BUILD_WORKER_THREADS)
            .build()
            .map_err(|err| {
                LayerStackError::Storage(format!("failed to build base-build thread pool: {err}"))
            })?;
        let Collected {
            mut entries,
            file_tasks,
            mut special,
            mut unstable,
        } = pool.install(|| collect_subtree(workspace, workspace, &staging_dir, &stats))?;
        merge_file_outcomes(
            pool.install(|| copy_files(&file_tasks, &stats))?,
            &mut entries,
            &mut special,
            &mut unstable,
        );
        if !special.is_empty() || !unstable.is_empty() {
            special.sort();
            unstable.sort();
            stats.emit(format!(
                "workspace changed or contains unsupported files: special={} unstable={}",
                special.len(),
                unstable.len()
            ));
            return Err(LayerStackError::Storage(format!(
                "workspace base must be a full copy; special={} [{}], unstable={} [{}]",
                special.len(),
                format_path_sample(&special),
                unstable.len(),
                format_path_sample(&unstable)
            )));
        }
        stats.emit(stats.summary());
        stats.emit(format!("hashing base manifest entries={}", entries.len()));
        let root_hash = base_root_hash(&mut entries);
        stats.emit(format!("base manifest root_hash={root_hash}"));
        if let Some(parent) = layer_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&staging_dir, &layer_dir)?;
        stats.emit(format!("base layer ready at {}", layer_dir.display()));
        Ok::<String, LayerStackError>(root_hash)
    })();
    let root_hash = match result {
        Ok(root_hash) => root_hash,
        Err(err) => {
            let _ = remove_path(&staging_dir);
            let _ = remove_path(&layer_dir);
            return Err(err);
        }
    };
    Ok((
        LayerRef {
            layer_id: layer_id.to_owned(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        },
        root_hash,
    ))
}

struct FileTask {
    source: PathBuf,
    target: PathBuf,
    rel: String,
}

enum FileOutcome {
    Copied(BaseEntry),
    Unstable(String),
    Special(String),
}

#[derive(Default)]
struct Collected {
    entries: Vec<BaseEntry>,
    file_tasks: Vec<FileTask>,
    special: Vec<String>,
    unstable: Vec<String>,
}

impl Collected {
    fn merge(&mut self, other: Collected) {
        self.entries.extend(other.entries);
        self.file_tasks.extend(other.file_tasks);
        self.special.extend(other.special);
        self.unstable.extend(other.unstable);
    }
}

fn collect_subtree(
    workspace: &Path,
    current: &Path,
    staging_dir: &Path,
    stats: &BuildStats,
) -> Result<Collected, LayerStackError> {
    let mut collected = Collected::default();
    let mut children = match std::fs::read_dir(current) {
        Ok(read_dir) => read_dir.collect::<Result<Vec<_>, _>>()?,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            collected.unstable.push(relative_path(workspace, current));
            return Ok(collected);
        }
        Err(err) => return Err(err.into()),
    };
    children.sort_by_key(std::fs::DirEntry::file_name);

    let mut subdirs = Vec::new();
    for child in children {
        let source = child.path();
        let rel = relative_path(workspace, &source);
        let target = join_layer_path(staging_dir, &rel);
        let file_type = match child.file_type() {
            Ok(file_type) => file_type,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                collected.unstable.push(rel);
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        if file_type.is_symlink() {
            let Ok(link_target) = std::fs::read_link(&source) else {
                collected.special.push(rel);
                continue;
            };
            std::os::unix::fs::symlink(&link_target, &target)?;
            collected.entries.push(BaseEntry::Symlink {
                path: rel,
                link_target: link_target.to_string_lossy().into_owned(),
            });
            stats.record_symlink();
        } else if file_type.is_dir() {
            std::fs::create_dir_all(&target)?;
            collected.entries.push(BaseEntry::Directory { path: rel });
            stats.record_directory();
            subdirs.push(source);
        } else if file_type.is_file() {
            collected.file_tasks.push(FileTask {
                source,
                target,
                rel,
            });
        } else {
            collected.special.push(rel);
        }
    }

    let child_results = subdirs
        .par_iter()
        .map(|sub| collect_subtree(workspace, sub, staging_dir, stats))
        .collect::<Result<Vec<_>, LayerStackError>>()?;
    for child in child_results {
        collected.merge(child);
    }
    Ok(collected)
}

fn copy_files(tasks: &[FileTask], stats: &BuildStats) -> Result<Vec<FileOutcome>, LayerStackError> {
    tasks
        .par_iter()
        .map_init(
            || vec![0_u8; COPY_BUFFER_BYTES].into_boxed_slice(),
            |buffer, task| -> Result<FileOutcome, LayerStackError> {
                let outcome = copy_one_file(task, buffer)?;
                if let FileOutcome::Copied(BaseEntry::File { size, .. }) = &outcome {
                    stats.record_file(*size);
                }
                Ok(outcome)
            },
        )
        .collect()
}

fn merge_file_outcomes(
    outcomes: Vec<FileOutcome>,
    entries: &mut Vec<BaseEntry>,
    special: &mut Vec<String>,
    unstable: &mut Vec<String>,
) {
    for outcome in outcomes {
        match outcome {
            FileOutcome::Copied(entry) => entries.push(entry),
            FileOutcome::Unstable(rel) => unstable.push(rel),
            FileOutcome::Special(rel) => special.push(rel),
        }
    }
}

fn copy_one_file(task: &FileTask, buffer: &mut [u8]) -> Result<FileOutcome, LayerStackError> {
    match copy_file_with_hash(&task.source, &task.target, buffer) {
        Ok(copied) => Ok(FileOutcome::Copied(BaseEntry::File {
            path: task.rel.clone(),
            size: copied.size,
            content_hash: copied.content_hash,
        })),
        Err(CopyFileError::SourceNotFound) => Ok(FileOutcome::Unstable(task.rel.clone())),
        Err(CopyFileError::SourceUnreadable) => Ok(FileOutcome::Special(task.rel.clone())),
        Err(CopyFileError::Target(err)) => Err(err.into()),
    }
}

struct BuildStats {
    started: Instant,
    last_emit_ms: AtomicU64,
    files: AtomicU64,
    directories: AtomicU64,
    symlinks: AtomicU64,
    bytes: AtomicU64,
}

impl BuildStats {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            last_emit_ms: AtomicU64::new(0),
            files: AtomicU64::new(0),
            directories: AtomicU64::new(0),
            symlinks: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    fn summary(&self) -> String {
        format!(
            "copied files={} dirs={} symlinks={} bytes={}",
            self.files.load(Ordering::Relaxed),
            self.directories.load(Ordering::Relaxed),
            self.symlinks.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }

    fn record_file(&self, size: u64) {
        self.files.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(size, Ordering::Relaxed);
        self.maybe_emit();
    }

    fn record_directory(&self) {
        self.directories.fetch_add(1, Ordering::Relaxed);
        self.maybe_emit();
    }

    fn record_symlink(&self) {
        self.symlinks.fetch_add(1, Ordering::Relaxed);
        self.maybe_emit();
    }

    fn maybe_emit(&self) {
        let elapsed = self.started.elapsed().as_millis() as u64;
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        if elapsed.saturating_sub(last) < PROGRESS_INTERVAL.as_millis() as u64 {
            return;
        }
        if self
            .last_emit_ms
            .compare_exchange(last, elapsed, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            cli_log(self.summary());
        }
    }

    fn emit(&self, message: impl AsRef<str>) {
        self.last_emit_ms
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
        cli_log(message);
    }
}

fn cli_log(message: impl AsRef<str>) {
    let escaped = serde_json::to_string(message.as_ref()).unwrap_or_else(|_| "\"\"".to_owned());
    eprintln!("cli_log({escaped})");
}

struct CopiedFile {
    size: u64,
    content_hash: String,
}

enum CopyFileError {
    SourceNotFound,
    SourceUnreadable,
    Target(std::io::Error),
}

fn copy_file_with_hash(
    source: &Path,
    target: &Path,
    buffer: &mut [u8],
) -> Result<CopiedFile, CopyFileError> {
    let mut input = std::fs::File::open(source).map_err(map_source_error)?;
    let metadata = input.metadata().map_err(map_source_error)?;
    let permissions = metadata.permissions();
    let mut output = std::fs::File::create(target).map_err(CopyFileError::Target)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;

    let mut remaining = metadata.len();
    while remaining > 0 {
        let cap = (remaining.min(buffer.len() as u64)) as usize;
        let count = input.read(&mut buffer[..cap]).map_err(map_source_error)?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(CopyFileError::Target)?;
        digest.update(&buffer[..count]);
        size += count as u64;
        remaining -= count as u64;
    }

    output
        .set_permissions(permissions)
        .map_err(CopyFileError::Target)?;
    Ok(CopiedFile {
        size,
        content_hash: hex_lower(digest.finalize()),
    })
}

fn map_source_error(err: std::io::Error) -> CopyFileError {
    if err.kind() == ErrorKind::NotFound {
        CopyFileError::SourceNotFound
    } else {
        CopyFileError::SourceUnreadable
    }
}
