//! Writable overlay directory allocation.
//!
//! Overlayfs needs a writable `upperdir` plus a sibling `workdir` for every
//! mounted overlay. Lower layers are leased from the layer stack; this module
//! owns only the upper/work side of the mount. There is intentionally NO
//! fallback root — Docker-backed sandboxes provide the writable filesystem
//! under the unified `/eos` tmpfs.

use std::path::{Path, PathBuf};

use crate::error::{OverlayError, Result};

/// Canonical filesystem for overlay `upperdir`/`workdir`.
pub const OVERLAY_WRITABLE_ROOT: &str = "/eos/scratch/overlay";

/// Per-overlay writable directories created beside each other under one run dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayWritableDirs {
    /// The per-overlay run directory the upper/work dirs live under.
    pub run_dir: PathBuf,
    /// The overlay `upperdir` (`run_dir/upper`).
    pub upperdir: PathBuf,
    /// The overlay `workdir` (`run_dir/work`).
    pub workdir: PathBuf,
}

/// Return the canonical writable root, creating it if its parent exists.
///
/// Creates `OVERLAY_WRITABLE_ROOT` only when its parent is already a directory,
/// then requires the result to be a directory or raises
/// [`OverlayError::WritableRootUnavailable`]. No fallback.
///
/// # Errors
///
/// Returns [`OverlayError::Capture`] when directory creation fails, or
/// [`OverlayError::WritableRootUnavailable`] when the canonical root is not a
/// directory.
pub fn overlay_writable_root() -> Result<PathBuf> {
    let root = PathBuf::from(OVERLAY_WRITABLE_ROOT);
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
    Ok(OverlayWritableDirs {
        run_dir: run_dir.to_path_buf(),
        upperdir,
        workdir,
    })
}

#[cfg(test)]
mod tests {
    use super::allocate_overlay_writable_dirs;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn allocates_upper_and_work_dirs() -> TestResult {
        let run_dir = std::env::temp_dir().join(format!("eos-overlay-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&run_dir);

        let dirs = allocate_overlay_writable_dirs(&run_dir)?;
        assert!(dirs.upperdir.is_dir());
        assert!(dirs.workdir.is_dir());

        let _ = std::fs::remove_dir_all(&run_dir);
        Ok(())
    }
}
