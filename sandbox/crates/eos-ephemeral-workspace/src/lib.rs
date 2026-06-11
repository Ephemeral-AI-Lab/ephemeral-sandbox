//! A per-operation overlay transaction.
//!
//! An ephemeral workspace is an overlay that one command runs **on** — never a
//! thing that runs commands. It is created against a frozen set of layer
//! paths, exposes the mount plan a runner child needs, captures its upperdir
//! delta when the operation settles, and removes its scratch directories on
//! drop.
//!
//! What this crate deliberately does NOT know: leases (the orchestrator that
//! acquired the snapshot keeps custody), publishing (the captured changes are
//! returned to the caller, which decides what reaches the layer stack), and
//! command execution (PTY and runner-protocol concerns live in their own
//! crates). The only storage vocabulary used is the `LayerChange` re-export
//! from `eos-overlay`.
#![forbid(unsafe_code)]

mod capture;
mod dirs;
mod runtime_dirs;
mod stats;
mod workspace;

pub use capture::{
    capture_upperdir, path_changes_to_wire, CapturedChanges, PathChange, PathChangeKind,
};
pub use dirs::{DirAllocator, OverlayDirs, OverlayDirsGuard};
pub use runtime_dirs::overlay_run_dirs;
pub use stats::TreeResourceStats;
pub use workspace::{EphemeralWorkspace, MountPlan};

use std::path::PathBuf;

use thiserror::Error;

/// Errors raised by the overlay-transaction lifecycle.
#[derive(Debug, Error)]
pub enum EphemeralWorkspaceError {
    /// Fresh writable directory allocation failed.
    #[error("dir allocation failed at {}: {reason}", path.display())]
    DirAllocation { path: PathBuf, reason: String },
    /// Upperdir capture failed.
    #[error("capture failed: {reason}")]
    CaptureFailed { reason: String },
}
