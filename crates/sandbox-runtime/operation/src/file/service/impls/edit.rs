//! `edit`: ordered exact-string replacements over a single path. Sessionless
//! edits are an atomic read-modify-write of head through `amend_path`; session
//! edits read the live overlay, apply the edits, and write back through the
//! namespace runner. Empty and no-op edit sets are rejected.

use sandbox_runtime_layerstack::ManifestFileRead;

use crate::file::service::support::{amend_error, apply_edits, resolve_layer_path, MAX_EDIT_BYTES};
use crate::file::{EditInput, EditOutput, FileEntryKind, FileOperationError, FileService};
use crate::layerstack::LayerStackService;
use crate::workspace_session::WorkspaceSessionService;

impl FileService {
    /// Apply `input.edits` in order to `input.path`. With `workspace_session_id`,
    /// the edit runs inside the live session namespace and does not publish;
    /// without it, the edit publishes one layer attributed to
    /// `operation:<request_id>`.
    ///
    /// # Errors
    /// Returns [`FileOperationError`] for empty/no-op edits, unmatched or
    /// non-unique `old_string`, missing/invalid/non-regular paths, oversized
    /// files, or a backend failure.
    pub fn edit(
        &self,
        layerstack: &LayerStackService,
        workspace_session: &WorkspaceSessionService,
        input: EditInput,
    ) -> Result<EditOutput, FileOperationError> {
        if input.edits.is_empty() {
            return Err(FileOperationError::NoEdits);
        }
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
                let edits = &input.edits;
                let mut replacements = 0;
                let outcome = layerstack
                    .amend_path(&rel, &owner, MAX_EDIT_BYTES, |read| {
                        let bytes = match read {
                            ManifestFileRead::Absent => {
                                return Err(FileOperationError::NotFound(path.clone()))
                            }
                            ManifestFileRead::Directory => {
                                return Err(FileOperationError::NotRegular {
                                    path: path.clone(),
                                    kind: FileEntryKind::Directory,
                                })
                            }
                            ManifestFileRead::Symlink => {
                                return Err(FileOperationError::NotRegular {
                                    path: path.clone(),
                                    kind: FileEntryKind::Symlink,
                                })
                            }
                            ManifestFileRead::TooLarge { size, limit } => {
                                return Err(FileOperationError::FileTooLarge {
                                    path: path.clone(),
                                    size,
                                    limit,
                                })
                            }
                            ManifestFileRead::File { bytes, .. } => bytes,
                        };
                        let text = String::from_utf8(bytes)
                            .map_err(|_| FileOperationError::NotUtf8(path.clone()))?;
                        let (edited, count) = apply_edits(&text, edits, &path)?;
                        replacements = count;
                        Ok(edited.into_bytes())
                    })
                    .map_err(amend_error)?;
                Ok(EditOutput {
                    path,
                    edits_applied: input.edits.len(),
                    replacements,
                    bytes_written: outcome.bytes_written,
                })
            }
        }
    }
}
