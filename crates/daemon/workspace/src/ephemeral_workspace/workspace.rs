use std::path::Path;

use super::EphemeralWorkspaceError;
use crate::overlay::dirs::{allocate_overlay_dirs, OverlayDirs};

/// One overlay transaction's scratch dirs.
///
/// Dropping the workspace removes its run directory (best-effort), so the caller
/// can capture the upperdir on success or just drop on cancel/discard.
#[derive(Debug)]
pub struct EphemeralWorkspace {
    dirs: OverlayDirs,
}

impl EphemeralWorkspace {
    /// Allocate fresh overlay dirs under `scratch_root` for one operation.
    ///
    /// `kind` and `token` only shape the scratch directory name (sanitized).
    ///
    /// # Errors
    ///
    /// Returns [`EphemeralWorkspaceError::DirAllocation`] when scratch
    /// directories cannot be created.
    pub fn create(
        scratch_root: &Path,
        kind: &str,
        token: &str,
    ) -> Result<Self, EphemeralWorkspaceError> {
        let dirs = allocate_overlay_dirs(scratch_root, kind, token)?;
        Ok(Self { dirs })
    }

    /// Allocate fresh overlay dirs under the daemon runtime writable root.
    ///
    /// This preserves the legacy host-command scratch placement while allowing
    /// higher-level runtime code to own the workspace lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`EphemeralWorkspaceError::DirAllocation`] when scratch
    /// directories cannot be created.
    pub fn create_runtime_overlay(
        kind: &str,
        token: &str,
    ) -> Result<Self, EphemeralWorkspaceError> {
        let dirs = crate::overlay::dirs::overlay_run_dirs(kind, token)?;
        Ok(Self { dirs })
    }

    #[must_use]
    pub fn dirs(&self) -> &OverlayDirs {
        &self.dirs
    }
}

impl Drop for EphemeralWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dirs.run_dir);
    }
}

#[cfg(test)]
#[path = "../../tests/unit/ephemeral_workspace.rs"]
mod tests;
