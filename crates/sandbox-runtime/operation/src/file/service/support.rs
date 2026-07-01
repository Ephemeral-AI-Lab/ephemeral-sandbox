//! Backend-agnostic file-operation helpers: path mapping, read-window
//! normalization, and the ordered edit application shared by the sessionless and
//! session edit backends.

use std::path::Path;

use sandbox_runtime_layerstack::LayerPath;

use crate::file::{EditOp, FileOperationError};
use crate::layerstack::AmendError;

pub(crate) const DEFAULT_READ_LIMIT: usize = 2000;
pub(crate) const MAX_OUTPUT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_EDIT_BYTES: usize = 4 * 1024 * 1024;

const SNIPPET_MAX_CHARS: usize = 60;

/// Map an accepted `path` (absolute under `workspace_root`, or repo-relative) to
/// a normalized [`LayerPath`]. Absolute paths outside the root, `..`, empty, and
/// NUL paths are rejected by `LayerPath`.
pub(crate) fn resolve_layer_path(
    workspace_root: &Path,
    path: &str,
) -> Result<LayerPath, FileOperationError> {
    let candidate = match Path::new(path).strip_prefix(workspace_root) {
        Ok(rel) => rel
            .to_str()
            .ok_or_else(|| FileOperationError::InvalidPath(path.to_owned()))?
            .to_owned(),
        Err(_) => path.to_owned(),
    };
    LayerPath::parse(&candidate).map_err(|_| FileOperationError::InvalidPath(path.to_owned()))
}

/// Normalize the requested read window: `offset <= 1` starts at line 1, and an
/// omitted `limit` defaults to 2000. The `limit` range is validated at dispatch.
pub(crate) fn effective_read_window(offset: Option<u64>, limit: Option<usize>) -> (u64, usize) {
    let offset = match offset {
        Some(value) if value > 1 => value,
        _ => 1,
    };
    (offset, limit.unwrap_or(DEFAULT_READ_LIMIT))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

/// Apply ordered exact-string edits to `text`, matching the local-os `edit`
/// tool: match against line-ending-normalized content, then restore the file's
/// original dominant line ending. Returns the edited text and total replacement
/// count.
pub(crate) fn apply_edits(
    text: &str,
    edits: &[EditOp],
    path: &str,
) -> Result<(String, usize), FileOperationError> {
    let line_ending = detect_line_ending(text);
    let original = normalize_line_endings(text);
    let mut current = original.clone();
    let mut replacements = 0;
    for edit in edits {
        let old = normalize_line_endings(&edit.old_string);
        let new = normalize_line_endings(&edit.new_string);
        if old.is_empty() {
            return Err(FileOperationError::EditNotFound {
                path: path.to_owned(),
                snippet: snippet(&old),
            });
        }
        if old == new {
            return Err(FileOperationError::NoChanges(path.to_owned()));
        }
        let count = current.matches(&old).count();
        if count == 0 {
            return Err(FileOperationError::EditNotFound {
                path: path.to_owned(),
                snippet: snippet(&old),
            });
        }
        if count > 1 && !edit.replace_all {
            return Err(FileOperationError::EditNotUnique {
                path: path.to_owned(),
                count,
                snippet: snippet(&old),
            });
        }
        if edit.replace_all {
            current = current.replace(&old, &new);
            replacements += count;
        } else {
            current = current.replacen(&old, &new, 1);
            replacements += 1;
        }
    }
    if current == original {
        return Err(FileOperationError::NoChanges(path.to_owned()));
    }
    Ok((restore_line_endings(&current, line_ending), replacements))
}

/// Flatten a sessionless-backend `amend_path` failure into a
/// [`FileOperationError`]: the transform error passes through unchanged, and a
/// layerstack failure becomes the `LayerStack` variant.
pub(crate) fn amend_error(error: AmendError<FileOperationError>) -> FileOperationError {
    match error {
        AmendError::Transform(inner) => inner,
        AmendError::LayerStack(inner) => FileOperationError::LayerStack(inner),
    }
}

fn snippet(text: &str) -> String {
    let mut snippet: String = text.chars().take(SNIPPET_MAX_CHARS).collect();
    if text.chars().count() > SNIPPET_MAX_CHARS {
        snippet.push('…');
    }
    snippet
}

fn detect_line_ending(text: &str) -> LineEnding {
    let first_crlf = text.find("\r\n");
    let first_lf = text.find('\n');
    let first_cr = text.find('\r');
    if let (Some(crlf), Some(cr)) = (first_crlf, first_cr) {
        if crlf == cr {
            return LineEnding::CrLf;
        }
    }
    if let Some(cr) = first_cr {
        if first_lf.is_none_or(|lf| cr < lf) {
            return LineEnding::Cr;
        }
    }
    LineEnding::Lf
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(text: &str, line_ending: LineEnding) -> String {
    match line_ending {
        LineEnding::Lf => text.to_owned(),
        LineEnding::CrLf => text.replace('\n', "\r\n"),
        LineEnding::Cr => text.replace('\n', "\r"),
    }
}
