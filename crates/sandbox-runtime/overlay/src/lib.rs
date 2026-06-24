//! Overlay kernel-mount path and writable directory helpers.
//!
//! # Invariant
//!
//! The overlay mount itself is built with the RAW new-mount API
//! (`fsopen`/`fsconfig`/`fsmount`/`move_mount`) — NOT the `mount(8)` binary.
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

use thiserror::Error;

pub mod kernel_mount;

pub use kernel_mount::{
    mount_overlay, mount_overlay_with_visible_options, move_mountpoint, unmount_overlay,
    OverlayHandle, OverlayMount,
};

/// Failures raised by the overlay kernel-mount and writable-dir paths.
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
    #[error("upperdir capture failed at {path}: {source}")]
    Capture {
        /// Path whose metadata, directory entries, xattrs, content, or link
        /// target could not be read.
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The current target OS provides no overlayfs mount syscalls.
    #[error("overlay mounts are only supported on linux")]
    Unsupported,
}

impl OverlayError {
    pub fn capture(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Capture {
            path: path.into(),
            source,
        }
    }
}

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
pub fn overlay_writable_root() -> std::result::Result<PathBuf, OverlayError> {
    let root = std::env::var_os("SANDBOX_OVERLAY_WRITABLE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("overlay-writable-root-{}", std::process::id()))
        });
    std::fs::create_dir_all(&root).map_err(|err| OverlayError::capture(&root, err))?;
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
pub fn overlay_writable_root() -> std::result::Result<PathBuf, OverlayError> {
    let root = PathBuf::from("/eos/scratch/overlay");
    if root.parent().is_some_and(Path::is_dir) {
        std::fs::create_dir_all(&root).map_err(|err| OverlayError::capture(&root, err))?;
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
pub fn allocate_overlay_writable_dirs(
    run_dir: &Path,
) -> std::result::Result<OverlayWritableDirs, OverlayError> {
    let upperdir = run_dir.join("upper");
    let workdir = run_dir.join("work");
    std::fs::create_dir_all(&upperdir).map_err(|err| OverlayError::capture(&upperdir, err))?;
    std::fs::create_dir_all(&workdir).map_err(|err| OverlayError::capture(&workdir, err))?;
    Ok(OverlayWritableDirs { upperdir, workdir })
}
