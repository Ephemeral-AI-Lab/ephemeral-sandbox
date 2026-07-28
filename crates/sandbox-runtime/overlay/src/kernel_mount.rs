//! Kernel-boundary overlay mount mechanics — the RAW new-mount API.
//!
//! The overlay is built with `fsopen`/`fsconfig`/`fsmount`/`move_mount` (NOT the
//! `mount(8)` binary). `fsmount` applies `MOUNT_ATTR_NODEV` and
//! `MOUNT_ATTR_NOSUID` before the mount can be attached. Ordering invariant: the first
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
use rustix::fs::{statx, AtFlags, Mode, OFlags, StatxFlags};
#[cfg(target_os = "linux")]
use rustix::io::Errno;
#[cfg(target_os = "linux")]
use rustix::mount::{
    fsconfig_create, fsconfig_set_flag, fsconfig_set_string, fsmount, fsopen, move_mount, unmount,
    FsMountFlags, FsOpenFlags, MountAttrFlags, MoveMountFlags, UnmountFlags,
};

use crate::OverlayError;

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

/// Stable kernel identity for an opened path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenedPathIdentity {
    /// Mount containing the opened object.
    pub mount_id: u64,
    /// Filesystem device major number.
    pub device_major: u32,
    /// Filesystem device minor number.
    pub device_minor: u32,
    /// Filesystem inode number.
    pub inode: u64,
}

/// Ordered proof that one pinned lower FD denotes its authorized path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenedLowerBinding {
    /// Newest-first position in the lower stack.
    pub index: usize,
    /// Exact path supplied by the trusted mount plan.
    pub authorized_path: PathBuf,
    /// Identity measured directly through the still-open lower FD.
    pub fd_identity: OpenedPathIdentity,
    /// Identity measured from the authorized path after the FD was opened.
    pub authorized_path_identity: OpenedPathIdentity,
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
        expect(dead_code, reason = "workspace_root is read by linux Drop unmount")
    )]
    workspace_root: Option<PathBuf>,
}

impl OverlayMount {
    /// Explicitly unmount this overlay and report any teardown error.
    ///
    /// `Drop` remains best-effort for callers that only need cleanup, but
    /// audited runners use this consuming method so unmount duration/failure can
    /// be recorded in their result payload.
    ///
    /// # Errors
    ///
    /// Returns [`OverlayError::MountSyscall`] when the mountpoint cannot be
    /// detached, or [`OverlayError::Unsupported`] on non-Linux targets.
    #[cfg(target_os = "linux")]
    pub fn unmount(mut self) -> std::result::Result<(), OverlayError> {
        if let Some(workspace_root) = self.workspace_root.take() {
            peel_unmounts(&workspace_root, true)?;
        }
        Ok(())
    }

    /// Non-Linux unsupported path: overlayfs unmount syscalls do not exist off
    /// Linux.
    ///
    /// # Errors
    ///
    /// Always returns [`OverlayError::Unsupported`].
    #[cfg(not(target_os = "linux"))]
    pub fn unmount(self) -> std::result::Result<(), OverlayError> {
        Err(OverlayError::Unsupported)
    }
}

impl Drop for OverlayMount {
    fn drop(&mut self) {
        // Best-effort cleanup; Drop cannot report cleanup errors.
        #[cfg(target_os = "linux")]
        if let Some(workspace_root) = self.workspace_root.take() {
            let _ = peel_unmounts(&workspace_root, false);
        }
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
pub fn mount_overlay(
    workspace_root: &Path,
    handle: &OverlayHandle,
) -> std::result::Result<OverlayMount, OverlayError> {
    mount_overlay_with_lower_inspection(workspace_root, handle, |_| ()).map(|(mount, ())| mount)
}

/// Mount an overlay and inspect stable lower identities after attach.
///
/// The inspector runs after `move_mount` and before any pinned lower FD is
/// released. Its return value is delivered with the live mount guard.
///
/// # Errors
///
/// Returns [`OverlayError`] when mount inputs are invalid or a kernel mount
/// syscall fails.
#[cfg(target_os = "linux")]
pub fn mount_overlay_with_lower_inspection<T>(
    workspace_root: &Path,
    handle: &OverlayHandle,
    inspect: impl FnOnce(&[OpenedLowerBinding]) -> T,
) -> std::result::Result<(OverlayMount, T), OverlayError> {
    let inputs = ValidatedMountInputs::open(workspace_root, handle)?;
    let fsfd = configured_overlay_fs(&inputs, LowerdirMode::Repeated)?;
    let mount_fd = match fsconfig_create(fsfd.as_fd()) {
        Ok(()) => fsmount(
            fsfd.as_fd(),
            FsMountFlags::FSMOUNT_CLOEXEC,
            overlay_mount_attributes(),
        )
        .map_mount_syscall("fsmount")?,
        Err(Errno::INVAL) => {
            let legacy_fsfd = configured_overlay_fs(&inputs, LowerdirMode::LegacyJoined)?;
            fsconfig_create(legacy_fsfd.as_fd())
                .map_mount_syscall("fsconfig create legacy lowerdir")?;
            fsmount(
                legacy_fsfd.as_fd(),
                FsMountFlags::FSMOUNT_CLOEXEC,
                overlay_mount_attributes(),
            )
            .map_mount_syscall("fsmount legacy lowerdir")?
        }
        Err(err) => {
            return Err(OverlayError::MountSyscall {
                context: "fsconfig create",
                source: std::io::Error::from(err),
            });
        }
    };
    move_mount(
        mount_fd.as_fd(),
        "",
        rustix::fs::CWD,
        &inputs.workspace_root,
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH,
    )
    .map_mount_syscall("move_mount workspace_root")?;
    let inspection = inspect(&inputs.lower_bindings);
    Ok((
        OverlayMount {
            workspace_root: Some(inputs.workspace_root),
        },
        inspection,
    ))
}

#[cfg(target_os = "linux")]
pub(crate) fn overlay_mount_attributes() -> MountAttrFlags {
    MountAttrFlags::MOUNT_ATTR_NODEV | MountAttrFlags::MOUNT_ATTR_NOSUID
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum LowerdirMode {
    Repeated,
    LegacyJoined,
}

#[cfg(target_os = "linux")]
fn configured_overlay_fs(
    inputs: &ValidatedMountInputs,
    lowerdir_mode: LowerdirMode,
) -> std::result::Result<std::os::fd::OwnedFd, OverlayError> {
    let fsfd =
        fsopen("overlay", FsOpenFlags::FSOPEN_CLOEXEC).map_mount_syscall("fsopen overlay")?;
    match lowerdir_mode {
        LowerdirMode::Repeated => {
            for layer in &inputs.layer_paths {
                fsconfig_set_string(fsfd.as_fd(), "lowerdir+", layer)
                    .map_mount_syscall("fsconfig lowerdir+")?;
            }
        }
        LowerdirMode::LegacyJoined => {
            let lowerdir = legacy_lowerdir_value(&inputs.layer_paths);
            fsconfig_set_string(fsfd.as_fd(), "lowerdir", lowerdir.as_str())
                .map_mount_syscall("fsconfig lowerdir")?;
        }
    }
    fsconfig_set_flag(fsfd.as_fd(), "userxattr").map_mount_syscall("fsconfig userxattr")?;
    fsconfig_set_string(fsfd.as_fd(), "upperdir", &inputs.upperdir)
        .map_mount_syscall("fsconfig upperdir")?;
    fsconfig_set_string(fsfd.as_fd(), "workdir", &inputs.workdir)
        .map_mount_syscall("fsconfig workdir")?;
    Ok(fsfd)
}

#[cfg(target_os = "linux")]
pub(crate) fn legacy_lowerdir_value(layer_paths: &[PathBuf]) -> String {
    let mut joined = String::new();
    for (index, path) in layer_paths.iter().enumerate() {
        if index > 0 {
            joined.push(':');
        }
        joined.push_str(&path.to_string_lossy());
    }
    joined
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
) -> std::result::Result<OverlayMount, OverlayError> {
    Err(OverlayError::Unsupported)
}

/// Non-Linux unsupported path for post-attach lower inspection.
///
/// # Errors
///
/// Always returns [`OverlayError::Unsupported`].
#[cfg(not(target_os = "linux"))]
pub fn mount_overlay_with_lower_inspection<T>(
    _workspace_root: &Path,
    _handle: &OverlayHandle,
    _inspect: impl FnOnce(&[OpenedLowerBinding]) -> T,
) -> std::result::Result<(OverlayMount, T), OverlayError> {
    Err(OverlayError::Unsupported)
}

/// Move the mount whose root is `source_mount_root` onto the directory at
/// `target_dir` — both pre-opened `O_PATH` fds, so masked or renamed paths
/// cannot break a staged switch mid-flight.
///
/// # Errors
///
/// Returns [`OverlayError::MountSyscall`] when `move_mount(2)` fails (a
/// failed move leaves both mounts where they were), or
/// [`OverlayError::Unsupported`] off Linux.
#[cfg(target_os = "linux")]
pub fn move_mountpoint(
    source_mount_root: impl AsFd,
    target_dir: impl AsFd,
) -> std::result::Result<(), OverlayError> {
    move_mount(
        source_mount_root.as_fd(),
        "",
        target_dir.as_fd(),
        "",
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH | MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
    )
    .map_mount_syscall("move_mount mountpoint")
}

/// Non-Linux unsupported path: mount-move syscalls do not exist off Linux.
///
/// # Errors
///
/// Always returns [`OverlayError::Unsupported`].
#[cfg(not(target_os = "linux"))]
pub fn move_mountpoint<Source, Target>(
    _source_mount_root: Source,
    _target_dir: Target,
) -> std::result::Result<(), OverlayError> {
    Err(OverlayError::Unsupported)
}

/// Strictly unmount `mountpoint`: one `umount2(path, 0)` with no
/// lazy/`MNT_DETACH` fallback. `EBUSY` surfaces verbatim in the error
/// source so callers can park instead of forcing. A masked mountpoint is
/// reachable through its pre-opened dirfd's `/proc/self/fd/N` magic path,
/// which `umount2`'s mountpoint lookup resolves onto the covering mount.
///
/// # Errors
///
/// Returns [`OverlayError::MountSyscall`] with the raw errno when the
/// kernel refuses the unmount, or [`OverlayError::Unsupported`] off Linux.
#[cfg(target_os = "linux")]
pub fn strict_unmount(mountpoint: &Path) -> std::result::Result<(), OverlayError> {
    unmount(mountpoint, UnmountFlags::empty()).map_mount_syscall("strict umount")
}

/// Non-Linux unsupported path: unmount syscalls do not exist off Linux.
///
/// # Errors
///
/// Always returns [`OverlayError::Unsupported`].
#[cfg(not(target_os = "linux"))]
pub fn strict_unmount(_mountpoint: &Path) -> std::result::Result<(), OverlayError> {
    Err(OverlayError::Unsupported)
}

#[cfg(target_os = "linux")]
pub(crate) struct ValidatedMountInputs {
    workspace_root: PathBuf,
    pub(crate) layer_paths: Vec<PathBuf>,
    pub(crate) upperdir: PathBuf,
    pub(crate) workdir: PathBuf,
    pub(crate) lower_bindings: Vec<OpenedLowerBinding>,
    _fds: Vec<File>,
}

#[cfg(target_os = "linux")]
impl ValidatedMountInputs {
    pub(crate) fn open(
        workspace_root: &Path,
        handle: &OverlayHandle,
    ) -> std::result::Result<Self, OverlayError> {
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
        let mut lower_bindings = Vec::with_capacity(handle.layer_paths.len());
        for (index, layer) in handle.layer_paths.iter().enumerate() {
            require_existing_dir(layer, "leased lowerdir")?;
            let fd = open_dir_no_follow(layer)?;
            let fd_identity = opened_file_identity(&fd)?;
            let authorized_path_identity = path_identity(layer)?;
            if fd_identity != authorized_path_identity {
                return Err(OverlayError::InvalidMountInput(format!(
                    "opened lower identity does not match authorized path at index {index}"
                )));
            }
            lower_bindings.push(OpenedLowerBinding {
                index,
                authorized_path: layer.clone(),
                fd_identity,
                authorized_path_identity,
            });
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
            fs::create_dir_all(path).map_err(|err| OverlayError::capture(path, err))?;
            fds.push(open_dir_no_follow(path)?);
        }

        Ok(Self {
            workspace_root: workspace_root.to_path_buf(),
            layer_paths,
            upperdir: handle.upperdir.clone(),
            workdir: handle.workdir.clone(),
            lower_bindings,
            _fds: fds,
        })
    }
}

#[cfg(target_os = "linux")]
fn opened_file_identity(file: &File) -> std::result::Result<OpenedPathIdentity, OverlayError> {
    let stat = statx(
        file.as_fd(),
        "",
        AtFlags::EMPTY_PATH | AtFlags::NO_AUTOMOUNT,
        StatxFlags::BASIC_STATS | StatxFlags::MNT_ID,
    )
    .map_mount_syscall("statx opened lower")?;
    statx_identity(stat, "opened lower")
}

#[cfg(target_os = "linux")]
fn path_identity(path: &Path) -> std::result::Result<OpenedPathIdentity, OverlayError> {
    let stat = statx(
        rustix::fs::CWD,
        path,
        AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
        StatxFlags::BASIC_STATS | StatxFlags::MNT_ID,
    )
    .map_err(|error| OverlayError::capture(path, error.into()))?;
    statx_identity(stat, "authorized lower path")
}

#[cfg(target_os = "linux")]
fn statx_identity(
    stat: rustix::fs::Statx,
    label: &str,
) -> std::result::Result<OpenedPathIdentity, OverlayError> {
    if stat.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        return Err(OverlayError::InvalidMountInput(format!(
            "{label} does not expose a stable mount id"
        )));
    }
    Ok(OpenedPathIdentity {
        mount_id: stat.stx_mnt_id,
        device_major: stat.stx_dev_major,
        device_minor: stat.stx_dev_minor,
        inode: stat.stx_ino,
    })
}

#[cfg(target_os = "linux")]
fn require_existing_dir(path: &Path, label: &str) -> std::result::Result<(), OverlayError> {
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
fn open_dir_no_follow(path: &Path) -> std::result::Result<File, OverlayError> {
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
fn peel_unmounts(
    workspace_root: &Path,
    allow_lazy_unmount: bool,
) -> std::result::Result<(), OverlayError> {
    for _ in 0..MAX_UNMOUNT_PEELS {
        match unmount(workspace_root, UnmountFlags::empty()) {
            Ok(()) => {}
            // umount(2) reports "nothing mounted here" as EINVAL for a plain
            // directory and ENOENT when the path itself is gone.
            Err(Errno::INVAL | Errno::NOENT) => return Ok(()),
            Err(_) if allow_lazy_unmount => {
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
fn reject_forbidden_chars(path: &Path) -> std::result::Result<(), OverlayError> {
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
    fn map_mount_syscall(self, context: &'static str) -> std::result::Result<T, OverlayError>;
}

#[cfg(target_os = "linux")]
impl<T> MountIo<T> for rustix::io::Result<T> {
    fn map_mount_syscall(self, context: &'static str) -> std::result::Result<T, OverlayError> {
        self.map_err(|err| OverlayError::MountSyscall {
            context,
            source: std::io::Error::from(err),
        })
    }
}
