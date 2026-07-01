//! `read`: a pure read of a single path. Sessionless reads project the active
//! layerstack snapshot; session reads run a namespace read-window against the
//! session's mounted workspace. Never mounts, publishes, or mutates.

use crate::file::service::support::{effective_read_window, resolve_layer_path, MAX_OUTPUT_BYTES};
use crate::file::{FileEntryKind, FileOperationError, FileService, ReadInput, ReadOutput};
use crate::layerstack::{LayerStackService, ManifestReadWindow};
use crate::workspace_session::WorkspaceSessionService;

impl FileService {
    /// Read a text window from `input.path`. With `workspace_session_id`, the
    /// read runs inside the live session namespace; without it, the read
    /// projects the latest published snapshot.
    ///
    /// # Errors
    /// Returns [`FileOperationError`] for missing/invalid paths, non-UTF-8 or
    /// non-regular files, oversized selected output, or a backend failure.
    pub fn read(
        &self,
        layerstack: &LayerStackService,
        workspace_session: &WorkspaceSessionService,
        input: ReadInput,
    ) -> Result<ReadOutput, FileOperationError> {
        match &input.workspace_session_id {
            Some(_workspace_session_id) => {
                let _ = workspace_session;
                Err(FileOperationError::WorkspaceSession(
                    "session file operations require the namespace runner (M4)".to_owned(),
                ))
            }
            None => {
                let workspace_root = layerstack.workspace_root()?;
                let rel = resolve_layer_path(&workspace_root, &input.path)?;
                let path = rel.as_str().to_owned();
                let (offset, limit) = effective_read_window(input.offset, input.limit);
                match layerstack.read_current_window(&rel, offset, limit, MAX_OUTPUT_BYTES)? {
                    ManifestReadWindow::Absent => Err(FileOperationError::NotFound(path)),
                    ManifestReadWindow::Directory => Err(FileOperationError::NotRegular {
                        path,
                        kind: FileEntryKind::Directory,
                    }),
                    ManifestReadWindow::Symlink => Err(FileOperationError::NotRegular {
                        path,
                        kind: FileEntryKind::Symlink,
                    }),
                    ManifestReadWindow::NotUtf8 => Err(FileOperationError::NotUtf8(path)),
                    ManifestReadWindow::OutputTooLarge { limit } => {
                        Err(FileOperationError::OutputTooLarge { path, limit })
                    }
                    ManifestReadWindow::Text {
                        content,
                        start_line,
                        num_lines,
                        total_lines,
                        bytes_read,
                        total_bytes,
                        next_offset,
                        truncated,
                    } => Ok(ReadOutput {
                        path,
                        content,
                        start_line,
                        num_lines,
                        total_lines,
                        bytes_read,
                        total_bytes,
                        next_offset,
                        truncated,
                    }),
                }
            }
        }
    }
}
