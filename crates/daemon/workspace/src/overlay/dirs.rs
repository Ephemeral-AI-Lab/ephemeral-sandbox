use std::path::{Path, PathBuf};

/// Fresh writable paths allocated for one workspace operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayDirs {
    pub run_dir: PathBuf,
    pub upperdir: PathBuf,
    pub workdir: PathBuf,
}

/// A failed attempt to allocate one overlay scratch path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("dir allocation failed at {}: {reason}", path.display())]
pub struct DirAllocationError {
    pub path: PathBuf,
    pub reason: String,
}

/// Best-effort cleanup guard for an allocated run directory, for callers that
/// hold dirs outside a workspace owner.
#[derive(Debug)]
pub struct OverlayDirsGuard(Option<PathBuf>);

/// Allocate daemon/runtime overlay dirs under the configured writable root.
///
/// # Errors
///
/// Returns [`DirAllocationError`] when the writable root or requested run dirs
/// cannot be created.
pub fn overlay_run_dirs(
    kind: &str,
    invocation_id: &str,
) -> Result<OverlayDirs, DirAllocationError> {
    let writable_root = overlay::overlay_writable_root().map_err(|error| DirAllocationError {
        path: PathBuf::from("overlay_writable_root"),
        reason: error.to_string(),
    })?;
    allocate_overlay_dirs(&writable_root.join("runtime"), kind, invocation_id)
}

/// Allocate `run_dir`, `upperdir`, and `workdir` under `writable_root`.
///
/// # Errors
///
/// Returns [`DirAllocationError`] when any directory cannot be created.
pub fn allocate_overlay_dirs(
    writable_root: &Path,
    kind: &str,
    token: &str,
) -> Result<OverlayDirs, DirAllocationError> {
    let run_dir = writable_root.join(sanitized_segment(kind)).join(format!(
        "{}-{}",
        std::process::id(),
        sanitized_segment(token)
    ));
    create_overlay_dirs(run_dir)
}

/// Create standard overlay scratch children under an already chosen run dir.
///
/// # Errors
///
/// Returns [`DirAllocationError`] when `run_dir`, `upper`, or `work` cannot be
/// created.
pub fn create_overlay_dirs(run_dir: PathBuf) -> Result<OverlayDirs, DirAllocationError> {
    let upperdir = run_dir.join("upper");
    let workdir = run_dir.join("work");

    for path in [&run_dir, &upperdir, &workdir] {
        std::fs::create_dir_all(path).map_err(|error| DirAllocationError {
            path: path.clone(),
            reason: error.to_string(),
        })?;
    }

    Ok(OverlayDirs {
        run_dir,
        upperdir,
        workdir,
    })
}

impl OverlayDirsGuard {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }
}

impl Drop for OverlayDirsGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn sanitized_segment(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "request".to_owned()
    } else {
        cleaned
    }
}
