//! Sessionless snapshot reads: `read_current_window` projects one path from the
//! active head, shaping regular UTF-8 files into a line window, and
//! `workspace_root` exposes the layerstack workspace binding for path mapping.

use std::path::PathBuf;

use sandbox_runtime_layerstack::{LayerPath, LayerStack, ManifestFileRead};

use crate::layerstack::service::model::ManifestReadWindow;
use crate::layerstack::{LayerStackService, LayerStackServiceError};

impl LayerStackService {
    /// Read a classified line window of `rel` from the active snapshot. Loads the
    /// whole regular file (the snapshot is local) but only caps the selected
    /// output, so a large file is never rejected for its total size.
    ///
    /// # Errors
    /// Returns [`LayerStackServiceError`] when the stack cannot be opened or read.
    pub fn read_current_window(
        &self,
        rel: &LayerPath,
        offset: u64,
        limit: usize,
        output_cap: usize,
    ) -> Result<ManifestReadWindow, LayerStackServiceError> {
        let stack = LayerStack::open(self.layer_stack_root.clone()).map_err(|error| {
            LayerStackServiceError::LayerStack {
                operation: "open",
                error,
            }
        })?;
        let read = stack.read_classified(rel, usize::MAX).map_err(|error| {
            LayerStackServiceError::LayerStack {
                operation: "read",
                error,
            }
        })?;
        Ok(match read {
            ManifestFileRead::Absent => ManifestReadWindow::Absent,
            ManifestFileRead::Directory => ManifestReadWindow::Directory,
            ManifestFileRead::Symlink => ManifestReadWindow::Symlink,
            ManifestFileRead::TooLarge { .. } => {
                ManifestReadWindow::OutputTooLarge { limit: output_cap }
            }
            ManifestFileRead::File { bytes, total_bytes } => match std::str::from_utf8(&bytes) {
                Err(_) => ManifestReadWindow::NotUtf8,
                Ok(text) => match window_text(text, offset, limit, output_cap) {
                    None => ManifestReadWindow::OutputTooLarge { limit: output_cap },
                    Some(window) => ManifestReadWindow::Text {
                        content: window.content,
                        start_line: window.start_line,
                        num_lines: window.num_lines,
                        total_lines: window.total_lines,
                        bytes_read: window.bytes_read,
                        total_bytes,
                        next_offset: window.next_offset,
                        truncated: window.truncated,
                    },
                },
            },
        })
    }

    /// The workspace root the active snapshot is bound to, for absolute-path
    /// mapping. Reads the existing layerstack binding; adds no state.
    ///
    /// # Errors
    /// Returns [`LayerStackServiceError`] when the binding cannot be read.
    pub fn workspace_root(&self) -> Result<PathBuf, LayerStackServiceError> {
        let binding = sandbox_runtime_layerstack::require_workspace_binding(&self.layer_stack_root)
            .map_err(|error| LayerStackServiceError::LayerStack {
                operation: "workspace_root",
                error,
            })?;
        Ok(PathBuf::from(binding.workspace_root))
    }
}

struct TextWindow {
    content: String,
    start_line: u64,
    num_lines: usize,
    total_lines: u64,
    bytes_read: usize,
    next_offset: Option<u64>,
    truncated: bool,
}

/// Shape a UTF-8 file into a line window, matching the local-os `read` tool:
/// drop a leading BOM, normalize `\r\n`/`\r` to `\n`, select `[offset, offset +
/// limit)` 1-indexed lines, and cap the selected output at `output_cap`
/// (`None`). `offset` is the 1-indexed start (already normalized so `<= 1`
/// means line 1).
fn window_text(raw: &str, offset: u64, limit: usize, output_cap: usize) -> Option<TextWindow> {
    let normalized = normalize_text(raw);
    let lines = split_lines(&normalized);
    let total_lines = lines.len() as u64;
    let start_index = offset.saturating_sub(1) as usize;
    let selected: Vec<&str> = lines
        .iter()
        .skip(start_index)
        .take(limit)
        .copied()
        .collect();
    let content = selected.join("\n");
    let bytes_read = content.len();
    if bytes_read > output_cap {
        return None;
    }
    let next_index = start_index.saturating_add(selected.len());
    let next_offset = (next_index < lines.len()).then(|| next_index as u64 + 1);
    Some(TextWindow {
        content,
        start_line: offset.max(1),
        num_lines: selected.len(),
        total_lines,
        bytes_read,
        next_offset,
        truncated: next_offset.is_some(),
    })
}

fn normalize_text(raw: &str) -> String {
    let without_bom = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    without_bom.replace("\r\n", "\n").replace('\r', "\n")
}

fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let without_trailing = text.strip_suffix('\n').unwrap_or(text);
    if without_trailing.is_empty() {
        return vec![""];
    }
    without_trailing.split('\n').collect()
}
