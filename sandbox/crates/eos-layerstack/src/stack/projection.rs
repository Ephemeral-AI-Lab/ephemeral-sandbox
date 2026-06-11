use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::LayerStackError;

use super::whiteout::{is_kernel_whiteout_meta, LOGICAL_WHITEOUT_PREFIX, OPAQUE_MARKER};
use crate::fs::remove_path;

pub(super) fn apply_layer(layer_dir: &Path, destination: &Path) -> Result<(), LayerStackError> {
    let mut entries = collect_project_entries(layer_dir)?;
    entries.sort_by(|left, right| left.rel.cmp(&right.rel));
    for entry in entries
        .iter()
        .filter(|entry| matches!(entry.kind, ProjectEntryKind::Opaque))
    {
        let dir = entry
            .rel
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(
                || destination.to_path_buf(),
                |parent| destination.join(parent),
            );
        clear_directory(&dir)?;
    }
    for entry in entries.iter().filter(|entry| {
        matches!(
            entry.kind,
            ProjectEntryKind::LogicalWhiteout | ProjectEntryKind::KernelWhiteout
        )
    }) {
        let target = match entry.kind {
            ProjectEntryKind::LogicalWhiteout => {
                let Some(name) = entry.rel.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let target_name = name.trim_start_matches(LOGICAL_WHITEOUT_PREFIX);
                entry
                    .rel
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map_or_else(
                        || destination.join(target_name),
                        |parent| destination.join(parent).join(target_name),
                    )
            }
            ProjectEntryKind::KernelWhiteout => destination.join(&entry.rel),
            _ => continue,
        };
        remove_path(&target)?;
    }
    for entry in entries.into_iter().filter(|entry| {
        matches!(
            entry.kind,
            ProjectEntryKind::Directory | ProjectEntryKind::File | ProjectEntryKind::Symlink
        )
    }) {
        let target = destination.join(&entry.rel);
        match entry.kind {
            ProjectEntryKind::Directory => ensure_directory(&target)?,
            ProjectEntryKind::File => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                remove_path(&target)?;
                std::fs::copy(entry.path, target)?;
            }
            ProjectEntryKind::Symlink => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                remove_path(&target)?;
                let link_target = std::fs::read_link(entry.path)?;
                std::os::unix::fs::symlink(link_target, target)?;
            }
            ProjectEntryKind::Opaque
            | ProjectEntryKind::LogicalWhiteout
            | ProjectEntryKind::KernelWhiteout => {}
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ProjectEntry {
    path: PathBuf,
    rel: PathBuf,
    kind: ProjectEntryKind,
}

#[derive(Debug)]
enum ProjectEntryKind {
    Opaque,
    LogicalWhiteout,
    KernelWhiteout,
    Directory,
    File,
    Symlink,
}

fn collect_project_entries(layer_dir: &Path) -> Result<Vec<ProjectEntry>, LayerStackError> {
    let mut entries = Vec::new();
    let mut stack = vec![layer_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut children = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            children.push(entry?);
        }
        children.sort_by_key(std::fs::DirEntry::path);
        for entry in children {
            let path = entry.path();
            let rel = path
                .strip_prefix(layer_dir)
                .map_err(|err| LayerStackError::Storage(err.to_string()))?
                .to_path_buf();
            let file_type = entry.file_type()?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let meta = std::fs::symlink_metadata(&path)?;
            let kind = if name == OPAQUE_MARKER {
                ProjectEntryKind::Opaque
            } else if name.starts_with(LOGICAL_WHITEOUT_PREFIX) {
                ProjectEntryKind::LogicalWhiteout
            } else if is_kernel_whiteout_meta(&path, &meta) {
                ProjectEntryKind::KernelWhiteout
            } else if file_type.is_symlink() {
                ProjectEntryKind::Symlink
            } else if file_type.is_dir() {
                stack.push(path.clone());
                ProjectEntryKind::Directory
            } else if file_type.is_file() {
                ProjectEntryKind::File
            } else {
                continue;
            };
            entries.push(ProjectEntry { path, rel, kind });
        }
    }
    Ok(entries)
}

fn clear_directory(path: &Path) -> Result<(), LayerStackError> {
    ensure_directory(path)?;
    for entry in std::fs::read_dir(path)? {
        remove_path(&entry?.path())?;
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<(), LayerStackError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => remove_path(path)?,
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    std::fs::create_dir_all(path)?;
    Ok(())
}
