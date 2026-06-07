//! Workspace binding persisted beside the active manifest.
//!
//! Maps public absolute or repo-relative tool paths onto layer-relative paths.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::LayerStackError;

/// Binding filename under a layer-stack storage root.
pub const WORKSPACE_BINDING_FILE: &str = "workspace.json";

/// Durable binding from a real workspace root to the layer-stack storage root.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct WorkspaceBinding {
    pub workspace_root: String,
    pub layer_stack_root: String,
    pub active_manifest_version: i64,
    pub active_root_hash: String,
    pub base_manifest_version: i64,
    pub base_root_hash: String,
}

impl WorkspaceBinding {
    /// Translate a repo-relative path into the normalized layer path.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the path is empty, absolute, or fails
    /// layer-path normalization.
    pub fn layer_path_from_relative(&self, path: &str) -> Result<String, LayerStackError> {
        let raw = path.trim();
        if raw.is_empty() {
            return Err(LayerStackError::WorkspaceBinding(
                "path is required".to_owned(),
            ));
        }
        if raw.starts_with('/') {
            return Err(LayerStackError::WorkspaceBinding(format!(
                "path must be relative: {raw}"
            )));
        }
        normalize_layer_path(raw)
    }

    /// Translate a workspace-absolute path into the normalized layer path.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the path is empty, relative, outside the
    /// bound workspace root, or fails layer-path normalization.
    pub fn layer_path_from_absolute(&self, path: &str) -> Result<String, LayerStackError> {
        let raw = path.trim();
        if raw.is_empty() {
            return Err(LayerStackError::WorkspaceBinding(
                "path is required".to_owned(),
            ));
        }
        if !raw.starts_with('/') {
            return Err(LayerStackError::WorkspaceBinding(format!(
                "path must be absolute: {raw}"
            )));
        }
        let workspace = PathBuf::from(&self.workspace_root);
        let candidate = PathBuf::from(raw);
        let relative = candidate.strip_prefix(&workspace).map_err(|_| {
            LayerStackError::WorkspaceBinding(format!(
                "path is outside bound workspace {}: {raw}",
                self.workspace_root
            ))
        })?;
        normalize_layer_path(&relative.to_string_lossy())
    }
}

/// Read the optional workspace binding.
///
/// # Errors
///
/// Returns [`LayerStackError`] when the binding file cannot be read or decoded.
pub fn read_workspace_binding(
    layer_stack_root: impl AsRef<Path>,
) -> Result<Option<WorkspaceBinding>, LayerStackError> {
    let path = layer_stack_root.as_ref().join(WORKSPACE_BINDING_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let payload = std::fs::read_to_string(&path)?;
    let binding = serde_json::from_str::<WorkspaceBinding>(&payload)
        .map_err(|err| LayerStackError::WorkspaceBinding(err.to_string()))?;
    Ok(Some(binding))
}

/// Read the required workspace binding.
///
/// # Errors
///
/// Returns [`LayerStackError`] when the binding is missing, unreadable, or
/// invalid.
pub fn require_workspace_binding(
    layer_stack_root: impl AsRef<Path>,
) -> Result<WorkspaceBinding, LayerStackError> {
    read_workspace_binding(layer_stack_root.as_ref())?.ok_or_else(|| {
        LayerStackError::WorkspaceBinding(format!(
            "workspace binding is missing: {}",
            layer_stack_root
                .as_ref()
                .join(WORKSPACE_BINDING_FILE)
                .display()
        ))
    })
}

fn normalize_layer_path(path: &str) -> Result<String, LayerStackError> {
    eos_protocol::LayerPath::parse(path)
        .map(|path| path.as_str().to_owned())
        .map_err(LayerStackError::from)
}
