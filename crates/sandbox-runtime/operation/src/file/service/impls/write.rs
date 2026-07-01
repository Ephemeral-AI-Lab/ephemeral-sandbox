//! `write`: overwrite a single path. Sessionless writes publish one layer
//! atomically through `amend_path` under the layerstack writer lock; session
//! writes land in the session overlay through the namespace runner and are
//! attributed later, on session capture.

use sandbox_runtime_layerstack::ManifestFileRead;

use crate::file::service::support::{amend_error, resolve_layer_path};
use crate::file::{
    FileEntryKind, FileOperationError, FileService, WriteInput, WriteKind, WriteOutput,
};
use crate::layerstack::LayerStackService;
use crate::workspace_session::WorkspaceSessionService;

impl FileService {
    /// Write `input.content` to `input.path`. With `workspace_session_id`, the
    /// write runs inside the live session namespace and does not publish;
    /// without it, the write publishes one layer attributed to
    /// `operation:<request_id>`.
    ///
    /// # Errors
    /// Returns [`FileOperationError`] for invalid/non-regular paths or a backend
    /// failure.
    pub fn write(
        &self,
        layerstack: &LayerStackService,
        workspace_session: &WorkspaceSessionService,
        input: WriteInput,
    ) -> Result<WriteOutput, FileOperationError> {
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
                let owner = format!("operation:{}", input.request_id);
                let content = input.content.into_bytes();
                let outcome = layerstack
                    .amend_path(&rel, &owner, 0, |read| match read {
                        ManifestFileRead::Directory => Err(FileOperationError::NotRegular {
                            path: path.clone(),
                            kind: FileEntryKind::Directory,
                        }),
                        ManifestFileRead::Symlink => Err(FileOperationError::NotRegular {
                            path: path.clone(),
                            kind: FileEntryKind::Symlink,
                        }),
                        ManifestFileRead::Absent
                        | ManifestFileRead::File { .. }
                        | ManifestFileRead::TooLarge { .. } => Ok(content),
                    })
                    .map_err(amend_error)?;
                Ok(WriteOutput {
                    kind: if outcome.existed_before {
                        WriteKind::Update
                    } else {
                        WriteKind::Create
                    },
                    path,
                    bytes_written: outcome.bytes_written,
                })
            }
        }
    }
}
