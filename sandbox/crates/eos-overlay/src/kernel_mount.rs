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
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::path::Component;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use rustix::fd::AsFd;
#[cfg(target_os = "linux")]
use rustix::mount::{
    fsconfig_create, fsconfig_set_flag, fsconfig_set_string, fsmount, fsopen, move_mount, unmount,
    FsMountFlags, FsOpenFlags, MountAttrFlags, MoveMountFlags, UnmountFlags,
};

use crate::error::{OverlayError, Result};

/// The inputs for one overlay mount.
///
/// `layer_paths` is the leased lower stack in NEWEST-FIRST order (element 0 =
/// highest-priority lower); `upperdir`/`workdir` are the writable side from
/// [`crate::writable_dirs`].
/// `// PORT backend/src/sandbox/overlay/kernel_mount.py:31-42 — MountInputs`
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
/// Wraps the `fsmount` file descriptor returned by the new-mount API; the fd is
/// `#[repr(transparent)]` so it crosses the syscall FFI as a bare `RawFd`, and
/// owns its teardown — dropping the handle unmounts the workspace root.
/// `// PORT backend/src/sandbox/overlay/kernel_mount.py:49-75 — mount_overlay (+ umount on teardown)`
#[derive(Debug)]
pub struct OverlayMount {
    /// The mountpoint this overlay was moved onto (`move_mount` destination).
    workspace_root: PathBuf,
}

impl OverlayMount {
    /// The workspace root this overlay is mounted at.
    pub fn workspace_root(&self) -> &std::path::Path {
        &self.workspace_root
    }
}

impl Drop for OverlayMount {
    fn drop(&mut self) {
        // Best-effort: peel every stacked mount at `workspace_root`. A Drop impl
        // cannot return an error, so failures are swallowed (matching the Python
        // non-raising default).
        // PORT backend/src/sandbox/overlay/kernel_mount.py:78-121 — umount loop on teardown
        #[cfg(target_os = "linux")]
        {
            for _ in 0..64 {
                if !is_mountpoint(&self.workspace_root) {
                    return;
                }
                if unmount(&self.workspace_root, UnmountFlags::empty()).is_err() {
                    return;
                }
            }
        }
    }
}

/// Mount an overlay filesystem at `workspace_root` from `handle`.
///
/// Builds the mount via the raw API in this exact order (per the ordering
/// invariant): `fsopen("overlay")`, `userxattr`, one
/// `fsconfig_string("lowerdir+", layer)` per layer in `handle.layer_paths`
/// (newest-first), then `"upperdir"`, `"workdir"`, `fsconfig_create`,
/// `fsmount`, and finally `move_mount` onto the real `workspace_root` (NOT a
/// `/proc/self/fd` symlink — `move_mount(2)` rejects that as a destination).
/// `// PORT backend/src/sandbox/overlay/kernel_mount.py:49-75 — mount_overlay`
#[cfg(target_os = "linux")]
pub fn mount_overlay(workspace_root: &Path, handle: &OverlayHandle) -> Result<OverlayMount> {
    // PORT backend/src/sandbox/overlay/kernel_mount.py:62-70 — fsopen/fsconfig/fsmount/move_mount
    let inputs = ValidatedMountInputs::open(workspace_root, handle)?;
    let fsfd = fsopen("overlay", FsOpenFlags::FSOPEN_CLOEXEC).map_io()?;
    fsconfig_set_flag(fsfd.as_fd(), "userxattr").map_io()?;
    for layer in &inputs.layer_paths {
        fsconfig_set_string(fsfd.as_fd(), "lowerdir+", layer).map_io()?;
    }
    fsconfig_set_string(fsfd.as_fd(), "upperdir", &inputs.upperdir).map_io()?;
    fsconfig_set_string(fsfd.as_fd(), "workdir", &inputs.workdir).map_io()?;
    fsconfig_create(fsfd.as_fd()).map_io()?;
    let mount_fd = fsmount(
        fsfd.as_fd(),
        FsMountFlags::FSMOUNT_CLOEXEC,
        MountAttrFlags::empty(),
    )
    .map_io()?;
    move_mount(
        mount_fd.as_fd(),
        "",
        rustix::fs::CWD,
        &inputs.workspace_root,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )
    .map_io()?;
    Ok(OverlayMount {
        workspace_root: inputs.workspace_root,
    })
}

/// Non-Linux stub: overlayfs mount syscalls do not exist off Linux.
#[cfg(not(target_os = "linux"))]
pub fn mount_overlay(_workspace_root: &Path, _handle: &OverlayHandle) -> Result<OverlayMount> {
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

        for layer in &handle.layer_paths {
            require_existing_dir(layer, "leased lowerdir")?;
            fds.push(open_dir_no_follow(layer)?);
        }

        for path in [&handle.upperdir, &handle.workdir] {
            if path
                .symlink_metadata()
                .is_ok_and(|meta| meta.file_type().is_symlink())
            {
                return Err(OverlayError::InvalidMountInput(format!(
                    "overlay upper/work dir must not be a symlink: {}",
                    path.display()
                )));
            }
            if path.exists() && !path.is_dir() {
                return Err(OverlayError::InvalidMountInput(format!(
                    "overlay upper/work path is not a directory: {}",
                    path.display()
                )));
            }
            fs::create_dir_all(path).map_err(OverlayError::Capture)?;
            fds.push(open_dir_no_follow(path)?);
        }

        let layer_paths = (0..handle.layer_paths.len())
            .map(|idx| fd_path(&fds[idx + 1]))
            .collect();
        let upperdir = fd_path(&fds[handle.layer_paths.len() + 1]);
        let workdir = fd_path(&fds[handle.layer_paths.len() + 2]);

        Ok(Self {
            workspace_root: workspace_root.to_path_buf(),
            layer_paths,
            upperdir,
            workdir,
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
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(OverlayError::MountSyscall)
}

#[cfg(target_os = "linux")]
fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
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
fn is_mountpoint(path: &Path) -> bool {
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return true;
    };
    let target = normalize_mount_path(path);
    mountinfo.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let mountpoint = fields.nth(4);
        mountpoint
            .map(decode_mountinfo_path)
            .is_some_and(|candidate| normalize_mount_path(&candidate) == target)
    })
}

#[cfg(target_os = "linux")]
fn normalize_mount_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(target_os = "linux")]
fn decode_mountinfo_path(raw: &str) -> PathBuf {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3].is_ascii_digit()
        {
            if let Ok(value) = u8::from_str_radix(&raw[i + 1..i + 4], 8) {
                out.push(value);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(target_os = "linux")]
trait MountIo<T> {
    fn map_io(self) -> Result<T>;
}

#[cfg(target_os = "linux")]
impl<T> MountIo<T> for rustix::io::Result<T> {
    fn map_io(self) -> Result<T> {
        self.map_err(|err| OverlayError::MountSyscall(std::io::Error::from(err)))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{decode_mountinfo_path, normalize_mount_path};
    use std::path::Path;

    #[test]
    fn decodes_mountinfo_octal_escapes() {
        assert_eq!(
            decode_mountinfo_path("/tmp/has\\040space"),
            Path::new("/tmp/has space")
        );
    }

    #[test]
    fn normalizes_paths_lexically() {
        assert_eq!(
            normalize_mount_path(Path::new("/tmp/./a/../b")),
            Path::new("/tmp/b")
        );
    }
}
