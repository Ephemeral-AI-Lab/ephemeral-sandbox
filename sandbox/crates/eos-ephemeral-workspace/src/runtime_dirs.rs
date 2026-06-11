use std::path::PathBuf;

use crate::{DirAllocator, EphemeralWorkspaceError, OverlayDirs};

/// Allocate daemon/runtime overlay dirs under the configured writable root.
///
/// This keeps the reusable directory shape with the ephemeral workspace crate;
/// callers that need to launch a namespace runner still own that process
/// behavior themselves.
pub fn overlay_run_dirs(
    kind: &str,
    invocation_id: &str,
) -> Result<OverlayDirs, EphemeralWorkspaceError> {
    let writable_root = eos_overlay::overlay_writable_root().map_err(|error| {
        EphemeralWorkspaceError::DirAllocation {
            path: PathBuf::from("overlay_writable_root"),
            reason: error.to_string(),
        }
    })?;
    DirAllocator::new(writable_root.join("runtime")).allocate(kind, invocation_id)
}
