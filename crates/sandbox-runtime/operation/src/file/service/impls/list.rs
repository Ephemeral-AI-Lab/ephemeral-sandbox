//! `list`: a pure one-level directory listing. Sessionless listings project
//! the active layerstack snapshot's merged view; session listings run a
//! namespace `ListDir` against the session's mounted workspace. Never mounts,
//! publishes, or mutates.

use std::path::Path;

use sandbox_runtime_layerstack::{LayerPath, ManifestDirEntryKind, ManifestDirList};

use crate::file::service::namespace;
use crate::file::service::support::resolve_layer_path;
use crate::file::{
    FileListEntry, FileListEntryKind, FileOperationError, FileService, ListInput, ListOutput,
};
use crate::layerstack::LayerStackService;
use crate::workspace_crate::{FileRunnerDirEntryKind, FileRunnerOp, FileRunnerResult};
use crate::workspace_session::WorkspaceSessionService;

impl FileService {
    /// List one directory level at `input.path` (the workspace root when
    /// omitted). With `workspace_session_id`, the listing runs inside the
    /// live session namespace; without it, it projects the latest published
    /// snapshot's merged view.
    ///
    /// # Errors
    /// Returns [`FileOperationError`] for missing paths, non-directory
    /// paths, invalid paths, or a backend failure.
    pub fn list(
        &self,
        layerstack: &LayerStackService,
        workspace_session: &WorkspaceSessionService,
        input: ListInput,
    ) -> Result<ListOutput, FileOperationError> {
        if input.limit == Some(0) {
            return Err(FileOperationError::InvalidListLimit(0));
        }
        let limit = input
            .limit
            .unwrap_or(self.caps().max_list_entries)
            .min(self.caps().max_list_entries);
        match &input.workspace_session_id {
            Some(workspace_session_id) => {
                let handler = workspace_session
                    .resolve_session(workspace_session_id.clone())
                    .map_err(|_| {
                        FileOperationError::WorkspaceSessionNotFound(workspace_session_id.0.clone())
                    })?;
                let rel = resolve_list_rel(&handler.handle.workspace_root, input.path.as_deref())?;
                let rel_str = rel.as_ref().map_or("", |rel| rel.as_str()).to_owned();
                let listed = namespace::run_file_op(
                    workspace_session,
                    &handler,
                    &rel_str,
                    FileRunnerOp::ListDir {
                        rel: rel_str.clone(),
                        limit,
                    },
                )
                .map_err(|error| match error {
                    FileOperationError::NotRegular { path, .. } => {
                        FileOperationError::NotDirectory(path)
                    }
                    other => other,
                })?;
                match listed {
                    FileRunnerResult::ListDir { existed: false, .. } => {
                        Err(FileOperationError::NotFound(rel_str))
                    }
                    FileRunnerResult::ListDir {
                        entries, truncated, ..
                    } => Ok(ListOutput {
                        path: rel_str,
                        entries: entries.into_iter().map(runner_entry).collect(),
                        truncated,
                    }),
                    _ => Err(FileOperationError::WorkspaceSession(
                        "namespace list returned an unexpected result".to_owned(),
                    )),
                }
            }
            None => {
                let workspace_root = layerstack.workspace_root()?;
                let rel = resolve_list_rel(&workspace_root, input.path.as_deref())?;
                let rel_str = rel.as_ref().map_or("", |rel| rel.as_str()).to_owned();
                match layerstack.list_current_dir(rel.as_ref(), limit)? {
                    ManifestDirList::Absent => Err(FileOperationError::NotFound(rel_str)),
                    ManifestDirList::NotDirectory => Err(FileOperationError::NotDirectory(rel_str)),
                    ManifestDirList::Entries { entries, truncated } => Ok(ListOutput {
                        path: rel_str,
                        entries: entries.into_iter().map(manifest_entry).collect(),
                        truncated,
                    }),
                }
            }
        }
    }
}

/// Map an accepted list `path` (absolute under `workspace_root`,
/// repo-relative, or omitted/root) to an optional [`LayerPath`]; `None`
/// means the workspace root itself.
fn resolve_list_rel(
    workspace_root: &Path,
    path: Option<&str>,
) -> Result<Option<LayerPath>, FileOperationError> {
    let Some(raw) = path else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "/" || trimmed == "." {
        return Ok(None);
    }
    if Path::new(trimmed) == workspace_root {
        return Ok(None);
    }
    resolve_layer_path(workspace_root, trimmed).map(Some)
}

fn runner_entry(entry: crate::workspace_crate::FileRunnerDirEntry) -> FileListEntry {
    FileListEntry {
        name: entry.name,
        kind: match entry.kind {
            FileRunnerDirEntryKind::File => FileListEntryKind::File,
            FileRunnerDirEntryKind::Directory => FileListEntryKind::Directory,
            FileRunnerDirEntryKind::Symlink => FileListEntryKind::Symlink,
            FileRunnerDirEntryKind::Other => FileListEntryKind::Other,
        },
        size: entry.size,
    }
}

fn manifest_entry(entry: sandbox_runtime_layerstack::ManifestDirEntry) -> FileListEntry {
    FileListEntry {
        name: entry.name,
        kind: match entry.kind {
            ManifestDirEntryKind::File => FileListEntryKind::File,
            ManifestDirEntryKind::Directory => FileListEntryKind::Directory,
            ManifestDirEntryKind::Symlink => FileListEntryKind::Symlink,
            ManifestDirEntryKind::Other => FileListEntryKind::Other,
        },
        size: entry.size,
    }
}
