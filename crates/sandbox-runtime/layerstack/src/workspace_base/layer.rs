use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::LayerStackError;
use crate::fs::{join_layer_path, remove_path};
use crate::model::{hex_lower, LayerRef};
use crate::{LAYERS_DIR, STAGING_DIR};

use super::collect::{base_root_hash, format_path_sample, relative_path, BaseEntry};

const WORKSPACE_BASE_LAYER_ID: &str = "B000001-base";
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

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
        let mut entries = Vec::new();
        let mut special = Vec::new();
        let mut unstable = Vec::new();
        let mut stats = BuildStats::new();
        stats.emit(
            "workspace_base.copy",
            "started",
            format!("copying workspace {} into base layer", workspace.display()),
        );
        copy_workspace_dir(
            workspace,
            workspace,
            &staging_dir,
            &mut entries,
            &mut special,
            &mut unstable,
            &mut stats,
        )?;
        if !special.is_empty() || !unstable.is_empty() {
            special.sort();
            unstable.sort();
            stats.emit(
                "workspace_base.copy",
                "failed",
                format!(
                    "workspace changed or contains unsupported files: special={} unstable={}",
                    special.len(),
                    unstable.len()
                ),
            );
            return Err(LayerStackError::Storage(format!(
                "workspace base must be a full copy; special={} [{}], unstable={} [{}]",
                special.len(),
                format_path_sample(&special),
                unstable.len(),
                format_path_sample(&unstable)
            )));
        }
        stats.emit("workspace_base.copy", "completed", stats.summary());
        stats.emit(
            "workspace_base.manifest",
            "started",
            format!("hashing base manifest entries={}", entries.len()),
        );
        let root_hash = base_root_hash(&mut entries);
        stats.emit(
            "workspace_base.manifest",
            "completed",
            format!("base manifest root_hash={root_hash}"),
        );
        if let Some(parent) = layer_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&staging_dir, &layer_dir)?;
        stats.emit(
            "workspace_base.layer",
            "completed",
            format!("base layer ready at {}", layer_dir.display()),
        );
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

fn copy_workspace_dir(
    workspace: &Path,
    current: &Path,
    staging_dir: &Path,
    entries: &mut Vec<BaseEntry>,
    special: &mut Vec<String>,
    unstable: &mut Vec<String>,
    stats: &mut BuildStats,
) -> Result<(), LayerStackError> {
    let mut children = match std::fs::read_dir(current) {
        Ok(read_dir) => read_dir.collect::<Result<Vec<_>, _>>()?,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            unstable.push(relative_path(workspace, current));
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    children.sort_by_key(std::fs::DirEntry::file_name);

    for child in children {
        let source = child.path();
        let rel = relative_path(workspace, &source);
        let target = join_layer_path(staging_dir, &rel);
        let meta = match std::fs::symlink_metadata(&source) {
            Ok(meta) => meta,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                unstable.push(rel);
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            let Ok(link_target) = std::fs::read_link(&source) else {
                special.push(rel);
                continue;
            };
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            remove_path(&target)?;
            std::os::unix::fs::symlink(&link_target, &target)?;
            entries.push(BaseEntry::Symlink {
                path: rel,
                link_target: link_target.to_string_lossy().into_owned(),
            });
            stats.symlinks += 1;
            stats.maybe_emit();
        } else if meta.is_dir() {
            std::fs::create_dir_all(&target)?;
            entries.push(BaseEntry::Directory { path: rel });
            stats.directories += 1;
            stats.maybe_emit();
            copy_workspace_dir(
                workspace,
                &source,
                staging_dir,
                entries,
                special,
                unstable,
                stats,
            )?;
        } else if meta.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            remove_path(&target)?;
            match copy_file_with_hash(&source, &target, &meta) {
                Ok(copied) => {
                    let size = copied.size;
                    entries.push(BaseEntry::File {
                        path: rel,
                        size,
                        content_hash: copied.content_hash,
                    });
                    stats.files += 1;
                    stats.bytes += size;
                    stats.maybe_emit();
                }
                Err(CopyFileError::SourceNotFound) => unstable.push(rel),
                Err(CopyFileError::SourceUnreadable) => special.push(rel),
                Err(CopyFileError::Target(err)) => return Err(err.into()),
            }
        } else {
            special.push(rel);
        }
    }
    Ok(())
}

struct BuildStats {
    started: Instant,
    last_emit: Instant,
    files: u64,
    directories: u64,
    symlinks: u64,
    bytes: u64,
}

impl BuildStats {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last_emit: now,
            files: 0,
            directories: 0,
            symlinks: 0,
            bytes: 0,
        }
    }

    fn summary(&self) -> String {
        format!(
            "copied files={} dirs={} symlinks={} bytes={}",
            self.files, self.directories, self.symlinks, self.bytes
        )
    }

    fn maybe_emit(&mut self) {
        if self.last_emit.elapsed() >= PROGRESS_INTERVAL {
            self.emit("workspace_base.copy", "running", self.summary());
        }
    }

    fn emit(&mut self, phase: &str, state: &str, message: impl Into<String>) {
        self.last_emit = Instant::now();
        emit_progress(phase, state, message, self.started.elapsed().as_millis());
    }
}

fn emit_progress(phase: &str, state: &str, message: impl Into<String>, elapsed_ms: u128) {
    let mut event = json!({
        "event": "progress",
        "op": "layerstack.setup",
        "phase": phase,
        "state": state,
        "message": message.into(),
        "elapsed_ms": elapsed_ms,
    });
    if let Some(sandbox_id) = daemon_sandbox_id() {
        event["sandbox_id"] = json!(sandbox_id);
    }
    eprintln!("{event}");
}

fn daemon_sandbox_id() -> Option<String> {
    std::env::var("SANDBOX_DAEMON_SANDBOX_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
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
    source_meta: &std::fs::Metadata,
) -> Result<CopiedFile, CopyFileError> {
    let mut input = std::fs::File::open(source).map_err(map_source_error)?;
    let mut output = std::fs::File::create(target).map_err(CopyFileError::Target)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();

    loop {
        let count = input.read(&mut buffer).map_err(map_source_error)?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(CopyFileError::Target)?;
        digest.update(&buffer[..count]);
        size += count as u64;
    }

    std::fs::set_permissions(target, source_meta.permissions()).map_err(CopyFileError::Target)?;
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
