use std::path::{Path, PathBuf};

use crate::capture::{capture_upperdir, CapturedChanges};
use crate::dirs::{DirAllocator, OverlayDirs};
use crate::EphemeralWorkspaceError;

/// One overlay transaction: scratch dirs bound to a frozen layer-path set.
///
/// Dropping the workspace removes its run directory (best-effort), so the
/// settle paths are simply: `capture()` then drop on success, plain drop on
/// cancel/discard. The lease that froze `layer_paths` stays with whoever
/// acquired it.
#[derive(Debug)]
pub struct EphemeralWorkspace {
    workspace_root: PathBuf,
    layer_paths: Vec<PathBuf>,
    dirs: OverlayDirs,
}

/// Everything a runner child needs to mount the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountPlan<'a> {
    /// Mount target and exec cwd inside the namespace.
    pub workspace_root: &'a Path,
    /// Frozen lower layers, newest-first.
    pub layer_paths: &'a [PathBuf],
    pub upperdir: &'a Path,
    pub workdir: &'a Path,
}

impl EphemeralWorkspace {
    /// Allocate fresh overlay dirs under `scratch_root` for one operation.
    ///
    /// `kind` and `token` only shape the scratch directory name (sanitized);
    /// `layer_paths` is the snapshot's frozen lower-layer list, newest-first.
    ///
    /// # Errors
    ///
    /// Returns [`EphemeralWorkspaceError::DirAllocation`] when scratch
    /// directories cannot be created.
    pub fn create(
        scratch_root: &Path,
        kind: &str,
        token: &str,
        workspace_root: PathBuf,
        layer_paths: Vec<PathBuf>,
    ) -> Result<Self, EphemeralWorkspaceError> {
        let dirs = DirAllocator::new(scratch_root.to_path_buf()).allocate(kind, token)?;
        Ok(Self {
            workspace_root,
            layer_paths,
            dirs,
        })
    }

    #[must_use]
    pub fn mount_plan(&self) -> MountPlan<'_> {
        MountPlan {
            workspace_root: &self.workspace_root,
            layer_paths: &self.layer_paths,
            upperdir: &self.dirs.upperdir,
            workdir: &self.dirs.workdir,
        }
    }

    #[must_use]
    pub fn dirs(&self) -> &OverlayDirs {
        &self.dirs
    }

    /// Capture the upperdir delta for publishing.
    ///
    /// Non-consuming: the caller publishes the returned changes and then drops
    /// the workspace, so a failed publish can still inspect the dirs.
    ///
    /// # Errors
    ///
    /// Returns [`EphemeralWorkspaceError::CaptureFailed`] when the overlay
    /// capture walk fails.
    pub fn capture(&self) -> Result<CapturedChanges, EphemeralWorkspaceError> {
        capture_upperdir(&self.dirs.upperdir)
    }
}

impl Drop for EphemeralWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dirs.run_dir);
    }
}

#[cfg(test)]
#[path = "../tests/unit/workspace.rs"]
mod tests;
