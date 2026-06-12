//! Overlay kernel-mount path and upper-dir capture.
//!
//! # Invariant
//!
//! **Capture + publish is ONE atomic unit per op.** The write set for an
//! operation is captured by walking ONLY the overlay `upperdir` (never the
//! lower layers); other agents never observe a partial write set. The overlay
//! mount itself is built with the RAW new-mount API
//! (`fsopen`/`fsconfig`/`fsmount`/`move_mount`) — NOT the `mount(8)` binary.
//!
//! Overlay produces the layer stack's change vocabulary one-way; the only
//! `eos-layerstack` edge is those model types. Workspace crates consume the
//! re-exported vocabulary from here without linking the storage engine's write
//! surface directly.
//!
//! # Build-time guarantee / platform
//!
//! Syscall crate built entirely on safe `rustix` wrappers — `unsafe_code` is
//! forbidden. The syscall surface is Linux-only: every mount/unmount body is
//! gated behind `#[cfg(target_os = "linux")]`, with a
//! `#[cfg(not(target_os = "linux"))]` arm returning
//! [`OverlayError::Unsupported`] so `cargo check` is green on the macOS dev
//! host.
//!
#![forbid(unsafe_code)]

use std::io;
use std::path::{Path, PathBuf};

use eos_layerstack::CasError;
use thiserror::Error;

pub mod kernel_mount;
pub mod path_change;

// The capture vocabulary, re-exported so overlay consumers (the workspace
// crates) never import the storage engine directly.
pub use eos_layerstack::{LayerChange, LayerPath};

pub use kernel_mount::{mount_overlay, unmount_overlay, OverlayHandle, OverlayMount};
pub use path_change::capture_upperdir;

/// Failures raised by the overlay kernel-mount and upper-dir capture paths.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OverlayError {
    /// The canonical writable root (`/eos/scratch/overlay`) is unavailable.
    #[error("overlay writable root is missing: {0}")]
    WritableRootUnavailable(String),

    /// A mount input failed validation before being handed to the mount syscalls.
    #[error("invalid mount input: {0}")]
    InvalidMountInput(String),

    /// A raw mount syscall (`fsopen`/`fsconfig`/`fsmount`/`move_mount`) or `umount` failed.
    #[error("overlay mount syscall failed at {context}: {source}")]
    MountSyscall {
        context: &'static str,
        #[source]
        source: io::Error,
    },

    /// An upper-dir walk / capture I/O error.
    #[error("upperdir capture failed: {0}")]
    Capture(#[source] io::Error),

    /// A captured overlay path did not normalize to a valid relative layer path.
    #[error(transparent)]
    Path(#[from] CasError),

    /// A captured overlay path could not be expressed as a layer path.
    #[error("invalid overlay path change: {0}")]
    InvalidPathChange(String),

    /// The current target OS provides no overlayfs mount syscalls.
    #[error("overlay mounts are only supported on linux")]
    Unsupported,
}

/// Crate result alias.
pub type Result<T> = std::result::Result<T, OverlayError>;

/// Per-overlay writable directories created beside each other under one run dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayWritableDirs {
    /// The overlay `upperdir` (`run_dir/upper`).
    pub upperdir: PathBuf,
    /// The overlay `workdir` (`run_dir/work`).
    pub workdir: PathBuf,
}

/// Return the test writable root, creating it if needed.
///
/// # Errors
///
/// Returns [`OverlayError::Capture`] when directory creation fails.
#[cfg(feature = "test-root-override")]
pub fn overlay_writable_root() -> Result<PathBuf> {
    let root = std::env::var_os("EOS_OVERLAY_WRITABLE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("eos-overlay-writable-root-{}", std::process::id()))
        });
    std::fs::create_dir_all(&root).map_err(OverlayError::Capture)?;
    Ok(root)
}

/// Return the canonical writable root (`/eos/scratch/overlay`), creating it
/// if its parent exists.
///
/// # Errors
///
/// Returns [`OverlayError::Capture`] when directory creation fails, or
/// [`OverlayError::WritableRootUnavailable`] when the canonical root is not a
/// directory.
#[cfg(not(feature = "test-root-override"))]
pub fn overlay_writable_root() -> Result<PathBuf> {
    let root = PathBuf::from("/eos/scratch/overlay");
    if root.parent().is_some_and(Path::is_dir) {
        std::fs::create_dir_all(&root).map_err(OverlayError::Capture)?;
    }
    if root.is_dir() {
        Ok(root)
    } else {
        Err(OverlayError::WritableRootUnavailable(
            root.display().to_string(),
        ))
    }
}

/// Create and return the `upper`/`work` dirs for one overlay instance.
///
/// # Errors
///
/// Returns [`OverlayError::Capture`] when either writable directory cannot be
/// created.
pub fn allocate_overlay_writable_dirs(run_dir: &Path) -> Result<OverlayWritableDirs> {
    let upperdir = run_dir.join("upper");
    let workdir = run_dir.join("work");
    std::fs::create_dir_all(&upperdir).map_err(OverlayError::Capture)?;
    std::fs::create_dir_all(&workdir).map_err(OverlayError::Capture)?;
    Ok(OverlayWritableDirs { upperdir, workdir })
}

#[cfg(test)]
#[path = "../tests/unit/writable_dirs.rs"]
mod writable_dirs_tests;
