//! Kernel-boundary overlay mount mechanics — the RAW new-mount API.
//!
//! The overlay is built with `fsopen`/`fsconfig`/`fsmount`/`move_mount` (NOT the
//! `mount(8)` binary). Ordering invariant: the first
//! `fsconfig(SET_STRING, "lowerdir+", path)` call is the highest-priority lower
//! layer, so [`OverlayHandle::layer_paths`] is iterated in its given
//! newest-first order.
//!
//! Linux-only: every syscall body is gated behind `#[cfg(target_os = "linux")]`
//! with a `#[cfg(not(target_os = "linux"))]` arm returning
//! [`OverlayError::Unsupported`] so non-Linux `cargo check` stays green.

#[cfg(target_os = "linux")]
use std::fs::{self, File};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use rustix::fd::AsFd;
#[cfg(target_os = "linux")]
use rustix::fs::{Mode, OFlags};
#[cfg(target_os = "linux")]
use rustix::io::Errno;
#[cfg(target_os = "linux")]
use rustix::mount::{
    fsconfig_create, fsconfig_set_string, fsmount, fsopen, move_mount, unmount, FsMountFlags,
    FsOpenFlags, MountAttrFlags, MoveMountFlags, UnmountFlags,
};

use crate::{OverlayError, Result};

#[cfg(target_os = "linux")]
const MAX_UNMOUNT_PEELS: usize = 64;

/// The inputs for one overlay mount.
///
/// `layer_paths` is the leased lower stack in NEWEST-FIRST order (element 0 =
/// highest-priority lower); `upperdir`/`workdir` are the writable side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayHandle {
    /// Writable upper directory.
    pub upperdir: PathBuf,
    /// Overlayfs work directory (sibling of `upperdir`).
    pub workdir: PathBuf,
    /// Leased lower-layer paths, NEWEST-FIRST (mount priority order).
    pub layer_paths: Vec<PathBuf>,
}

/// A live overlay mount at a workspace root. RAII: [`Drop`] unmounts.
///
/// The raw mount fd is closed after `move_mount`; this guard owns teardown by
/// unmounting the workspace root when it is dropped.
#[derive(Debug)]
pub struct OverlayMount {
    /// The mountpoint this overlay was moved onto (`move_mount` destination).
    #[cfg_attr(
        not(target_os = "linux"),
        allow(dead_code, reason = "workspace_root is read by linux Drop unmount")
    )]
    workspace_root: PathBuf,
}

impl Drop for OverlayMount {
    fn drop(&mut self) {
        // Best-effort cleanup; Drop cannot report cleanup errors.
        #[cfg(target_os = "linux")]
        let _ = peel_unmounts(&self.workspace_root, false);
    }
}

/// Mount an overlay filesystem at `workspace_root` from `handle`.
///
/// Builds the mount via the raw API in this exact order (per the ordering
/// invariant): `fsopen("overlay")`, one
/// `fsconfig_string("lowerdir+", layer)` per layer in `handle.layer_paths`
/// (newest-first), then real-path `"upperdir"` / `"workdir"`,
/// `fsconfig_create`, `fsmount`, and finally `move_mount` onto the real
/// `workspace_root` (NOT a `/proc/self/fd` symlink — `move_mount(2)` rejects
/// that as a destination, and overlayfs rejects fd-backed upper/work paths on
/// common kernels).
///
/// # Errors
///
/// Returns [`OverlayError`] when mount inputs are invalid or a kernel mount
/// syscall fails.
#[cfg(target_os = "linux")]
pub fn mount_overlay(workspace_root: &Path, handle: &OverlayHandle) -> Result<OverlayMount> {
    let inputs = ValidatedMountInputs::open(workspace_root, handle)?;
    let fsfd =
        fsopen("overlay", FsOpenFlags::FSOPEN_CLOEXEC).map_mount_syscall("fsopen overlay")?;
    for layer in &inputs.layer_paths {
        fsconfig_set_string(fsfd.as_fd(), "lowerdir+", layer)
            .map_mount_syscall("fsconfig lowerdir+")?;
    }
    fsconfig_set_string(fsfd.as_fd(), "upperdir", &inputs.upperdir)
        .map_mount_syscall("fsconfig upperdir")?;
    fsconfig_set_string(fsfd.as_fd(), "workdir", &inputs.workdir)
        .map_mount_syscall("fsconfig workdir")?;
    fsconfig_create(fsfd.as_fd()).map_mount_syscall("fsconfig create")?;
    let mount_fd = fsmount(
        fsfd.as_fd(),
        FsMountFlags::FSMOUNT_CLOEXEC,
        MountAttrFlags::empty(),
    )
    .map_mount_syscall("fsmount")?;
    move_mount(
        mount_fd.as_fd(),
        "",
        rustix::fs::CWD,
        &inputs.workspace_root,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )
    .map_mount_syscall("move_mount workspace_root")?;
    Ok(OverlayMount {
        workspace_root: inputs.workspace_root,
    })
}

/// Unmount every overlay stacked at `workspace_root`.
///
/// A single `umount` peels only the top mount. This helper loops until
/// `workspace_root` is no longer a mountpoint. If a normal unmount fails, it
/// falls back to `MNT_DETACH` so long-lived services with open descriptors can
/// be refreshed safely.
///
/// # Errors
///
/// Returns [`OverlayError::MountSyscall`] when the mountpoint cannot be
/// detached.
#[cfg(target_os = "linux")]
pub fn unmount_overlay(workspace_root: &Path) -> Result<()> {
    peel_unmounts(workspace_root, true)
}

/// Non-Linux unsupported path: overlayfs unmount syscalls do not exist off Linux.
///
/// # Errors
///
/// Always returns [`OverlayError::Unsupported`].
#[cfg(not(target_os = "linux"))]
pub const fn unmount_overlay(_workspace_root: &Path) -> Result<()> {
    Err(OverlayError::Unsupported)
}

/// Non-Linux unsupported path: overlayfs mount syscalls do not exist off Linux.
///
/// # Errors
///
/// Always returns [`OverlayError::Unsupported`].
#[cfg(not(target_os = "linux"))]
pub const fn mount_overlay(
    _workspace_root: &Path,
    _handle: &OverlayHandle,
) -> Result<OverlayMount> {
    Err(OverlayError::Unsupported)
}

#[cfg(target_os = "linux")]
struct ValidatedMountInputs {
    workspace_root: PathBuf,
    layer_paths: Vec<PathBuf>,
    upperdir: PathBuf,
    workdir: PathBuf,
    _fds: Vec<File>,
}

#[cfg(target_os = "linux")]
impl ValidatedMountInputs {
    fn open(workspace_root: &Path, handle: &OverlayHandle) -> Result<Self> {
        if handle.layer_paths.is_empty() {
            return Err(OverlayError::InvalidMountInput(
                "layer_paths must not be empty".to_owned(),
            ));
        }

        reject_forbidden_chars(workspace_root)?;
        for path in &handle.layer_paths {
            reject_forbidden_chars(path)?;
        }
        reject_forbidden_chars(&handle.upperdir)?;
        reject_forbidden_chars(&handle.workdir)?;

        require_existing_dir(workspace_root, "workspace root")?;
        let mut fds = Vec::with_capacity(handle.layer_paths.len() + 3);
        fds.push(open_dir_no_follow(workspace_root)?);

        let mut layer_paths = Vec::with_capacity(handle.layer_paths.len());
        for layer in &handle.layer_paths {
            require_existing_dir(layer, "leased lowerdir")?;
            let fd = open_dir_no_follow(layer)?;
            layer_paths.push(fd_path(&fd));
            fds.push(fd);
        }

        for path in [&handle.upperdir, &handle.workdir] {
            match path.symlink_metadata() {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(OverlayError::InvalidMountInput(format!(
                        "overlay upper/work dir must not be a symlink: {}",
                        path.display()
                    )));
                }
                Ok(meta) if !meta.is_dir() => {
                    return Err(OverlayError::InvalidMountInput(format!(
                        "overlay upper/work path is not a directory: {}",
                        path.display()
                    )));
                }
                _ => {}
            }
            fs::create_dir_all(path).map_err(OverlayError::Capture)?;
            fds.push(open_dir_no_follow(path)?);
        }

        Ok(Self {
            workspace_root: workspace_root.to_path_buf(),
            layer_paths,
            upperdir: handle.upperdir.clone(),
            workdir: handle.workdir.clone(),
            _fds: fds,
        })
    }
}

#[cfg(target_os = "linux")]
fn require_existing_dir(path: &Path, label: &str) -> Result<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(OverlayError::InvalidMountInput(format!(
            "{label} must not be a symlink: {}",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(OverlayError::InvalidMountInput(format!(
            "{label} is missing: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_dir_no_follow(path: &Path) -> Result<File> {
    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_mount_syscall("open directory")
}

#[cfg(target_os = "linux")]
fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_os = "linux")]
fn peel_unmounts(workspace_root: &Path, allow_lazy_fallback: bool) -> Result<()> {
    for _ in 0..MAX_UNMOUNT_PEELS {
        match unmount(workspace_root, UnmountFlags::empty()) {
            Ok(()) => {}
            // umount(2) reports "nothing mounted here" as EINVAL for a plain
            // directory and ENOENT when the path itself is gone.
            Err(Errno::INVAL | Errno::NOENT) => return Ok(()),
            Err(_) if allow_lazy_fallback => {
                unmount(workspace_root, UnmountFlags::DETACH)
                    .map_mount_syscall("lazy umount workspace_root")?;
            }
            Err(err) => {
                return Err(OverlayError::MountSyscall {
                    context: "umount workspace_root",
                    source: std::io::Error::from(err),
                });
            }
        }
    }
    Err(OverlayError::MountSyscall {
        context: "umount workspace_root",
        source: std::io::Error::other(format!(
            "workspace root is still mounted after {MAX_UNMOUNT_PEELS} unmount attempts: {}",
            workspace_root.display()
        )),
    })
}

#[cfg(target_os = "linux")]
fn reject_forbidden_chars(path: &Path) -> Result<()> {
    let text = path.as_os_str().to_string_lossy();
    for bad in [",", ":", "\\", "\n", "\r", "\t", "\0"] {
        if text.contains(bad) {
            return Err(OverlayError::InvalidMountInput(format!(
                "overlay mount path cannot contain {bad:?}: {text:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
trait MountIo<T> {
    fn map_mount_syscall(self, context: &'static str) -> Result<T>;
}

#[cfg(target_os = "linux")]
impl<T> MountIo<T> for rustix::io::Result<T> {
    fn map_mount_syscall(self, context: &'static str) -> Result<T> {
        self.map_err(|err| OverlayError::MountSyscall {
            context,
            source: std::io::Error::from(err),
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "../tests/unit/kernel_mount.rs"]
mod tests;
