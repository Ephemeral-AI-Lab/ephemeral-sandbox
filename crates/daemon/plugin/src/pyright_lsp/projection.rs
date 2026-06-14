use std::path::Path;

use layerstack::{LayerStack, MergedView};

use crate::PluginRuntimeError;

pub(super) fn manifest_key(version: i64, root_hash: &str) -> String {
    format!("version:{version}:{root_hash}")
}

pub(super) fn project_snapshot(
    stack_root: &Path,
    projection_root: &Path,
    manifest: &layerstack::Manifest,
) -> Result<(), PluginRuntimeError> {
    validate_projection_root(projection_root)?;
    if projection_root.exists() {
        std::fs::remove_dir_all(projection_root)?;
    }
    if let Some(parent) = projection_root.parent() {
        std::fs::create_dir_all(parent)?;
    }
    MergedView::new(stack_root.to_path_buf()).project(projection_root, manifest)?;
    Ok(())
}

fn validate_projection_root(path: &Path) -> Result<(), PluginRuntimeError> {
    if !path.is_absolute() || path == Path::new("/") {
        return Err(PluginRuntimeError::InvalidRequest(format!(
            "pyright_lsp workspace_root must be an absolute non-root path: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn release_snapshot(stack_root: &Path, lease_id: &str) {
    if let Ok(mut stack) = LayerStack::open(stack_root.to_path_buf()) {
        let _ = stack.release_lease(lease_id);
    }
}
