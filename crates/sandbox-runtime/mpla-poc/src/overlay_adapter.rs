use std::path::{Path, PathBuf};

use sandbox_runtime_overlay::{mount_overlay, strict_unmount, OverlayHandle, OverlayMount};

use crate::{AllocationHandle, PocError, PocResult};

/// A real OverlayFS mount whose writable layer is the permanent allocation.
///
/// The mountpoint is disposable session state. `upper/` and its adjacent
/// `work/` directory remain at their allocation-time paths for the entire
/// allocation lifetime.
#[derive(Debug)]
pub struct PermanentOverlayMount {
    workspace_root: PathBuf,
    allocation_root: PathBuf,
    allocation_upper: PathBuf,
    allocation_work: PathBuf,
    mount: Option<OverlayMount>,
}

impl PermanentOverlayMount {
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    #[must_use]
    pub fn allocation_root(&self) -> &Path {
        &self.allocation_root
    }

    #[must_use]
    pub fn allocation_upper(&self) -> &Path {
        &self.allocation_upper
    }

    #[must_use]
    pub fn allocation_work(&self) -> &Path {
        &self.allocation_work
    }

    #[must_use]
    pub const fn is_mounted(&self) -> bool {
        self.mount.is_some()
    }

    /// Strictly unmount without a lazy-detach fallback.
    ///
    /// The production overlay guard's best-effort `Drop` remains armed until
    /// the strict syscall succeeds. After success, dropping the guard observes
    /// an already-unmounted directory and performs no payload operation.
    pub fn strict_unmount(mut self) -> PocResult<UnmountedOverlay> {
        strict_unmount(&self.workspace_root)?;
        let mount = self
            .mount
            .take()
            .ok_or_else(|| PocError::Integrity("overlay was already unmounted".to_owned()))?;
        drop(mount);
        Ok(UnmountedOverlay {
            workspace_root: self.workspace_root.clone(),
            allocation_root: self.allocation_root.clone(),
            allocation_upper: self.allocation_upper.clone(),
            allocation_work: self.allocation_work.clone(),
        })
    }
}

/// Paths retained after the only live workspace mount has been strictly
/// removed. This receipt contains physical facts for evidence, not identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnmountedOverlay {
    pub workspace_root: PathBuf,
    pub allocation_root: PathBuf,
    pub allocation_upper: PathBuf,
    pub allocation_work: PathBuf,
}

/// Mount a permanent allocation as the writable side of a real OverlayFS
/// workspace. Lower layers are supplied newest-first.
pub fn mount_permanent_overlay(
    allocation: &AllocationHandle,
    lower_dirs_newest_first: Vec<PathBuf>,
    workspace_root: &Path,
) -> PocResult<PermanentOverlayMount> {
    require_stationary_layout(allocation)?;
    std::fs::create_dir_all(workspace_root)
        .map_err(|error| PocError::io("create workspace mountpoint", workspace_root, error))?;

    let handle = OverlayHandle {
        upperdir: allocation.upper_dir.clone(),
        workdir: allocation.work_dir.clone(),
        layer_paths: lower_dirs_newest_first,
    };
    let mount = mount_overlay(workspace_root, &handle)?;
    Ok(PermanentOverlayMount {
        workspace_root: workspace_root.to_path_buf(),
        allocation_root: allocation.allocation_root.clone(),
        allocation_upper: allocation.upper_dir.clone(),
        allocation_work: allocation.work_dir.clone(),
        mount: Some(mount),
    })
}

fn require_stationary_layout(allocation: &AllocationHandle) -> PocResult<()> {
    if allocation.upper_dir.parent() != Some(allocation.allocation_root.as_path())
        || allocation.work_dir.parent() != Some(allocation.allocation_root.as_path())
        || allocation
            .upper_dir
            .file_name()
            .is_none_or(|name| name != "upper")
        || allocation
            .work_dir
            .file_name()
            .is_none_or(|name| name != "work")
    {
        return Err(PocError::Integrity(format!(
            "allocation {} does not have adjacent final-path upper/work directories",
            allocation.descriptor.allocation_id
        )));
    }
    Ok(())
}
