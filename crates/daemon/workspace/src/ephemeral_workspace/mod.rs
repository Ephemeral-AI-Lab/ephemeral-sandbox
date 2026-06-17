//! A per-operation overlay transaction.
//!
//! An ephemeral workspace is an overlay that one command runs **on** - never a
//! thing that runs commands. It allocates fresh overlay dirs for a runner
//! request, captures the upperdir delta when the operation settles, and removes
//! scratch directories on drop.
//!
//! What this module deliberately does NOT know: leases (the orchestrator that
//! acquired the snapshot keeps custody), publishing (the captured changes are
//! returned to the caller, which decides what reaches the layer stack), and
//! command execution (PTY and runner-protocol concerns live in their own
//! crates).

mod workspace;

pub use crate::overlay::capture::{capture_upperdir, CapturedChanges};
pub use crate::overlay::dirs::{OverlayDirs, OverlayDirsGuard};
pub use crate::overlay::tree::TreeResourceStats;
pub use workspace::EphemeralWorkspace;

use std::path::PathBuf;

use crate::overlay::dirs;
use thiserror::Error;

/// Errors raised by the overlay-transaction lifecycle.
#[derive(Debug, Error)]
pub enum EphemeralWorkspaceError {
    /// Fresh writable directory allocation failed.
    #[error("dir allocation failed at {}: {reason}", path.display())]
    DirAllocation { path: PathBuf, reason: String },
}

impl From<dirs::DirAllocationError> for EphemeralWorkspaceError {
    fn from(error: dirs::DirAllocationError) -> Self {
        Self::DirAllocation {
            path: error.path,
            reason: error.reason,
        }
    }
}

/// Allocate daemon/runtime overlay dirs under the configured writable root.
///
/// # Errors
///
/// Returns [`EphemeralWorkspaceError::DirAllocation`] when the writable root or
/// requested run dirs cannot be created.
pub fn overlay_run_dirs(
    kind: &str,
    invocation_id: &str,
) -> Result<OverlayDirs, EphemeralWorkspaceError> {
    dirs::overlay_run_dirs(kind, invocation_id).map_err(Into::into)
}
