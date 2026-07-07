use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::error::LayerStackError;

pub(crate) const LOGICAL_WHITEOUT_PREFIX: &str = ".wh.";
pub(crate) const OPAQUE_MARKER: &str = ".wh..wh..opq";

pub(crate) const TRUSTED_OVERLAY_WHITEOUT_XATTR: &str = "trusted.overlay.whiteout";
pub(crate) const USER_OVERLAY_WHITEOUT_XATTR: &str = "user.overlay.whiteout";
pub(crate) const TRUSTED_OVERLAY_OPAQUE_XATTR: &str = "trusted.overlay.opaque";
pub(crate) const USER_OVERLAY_OPAQUE_XATTR: &str = "user.overlay.opaque";
#[cfg(target_os = "linux")]
const WHITEOUT_DEVICE_MAJOR: u32 = 0;
#[cfg(target_os = "linux")]
const WHITEOUT_DEVICE_MINOR: u32 = 0;

#[cfg(target_os = "linux")]
pub(crate) fn write_kernel_whiteout(path: &Path) -> Result<(), LayerStackError> {
    let device = rustix::fs::makedev(WHITEOUT_DEVICE_MAJOR, WHITEOUT_DEVICE_MINOR);
    let mknod = rustix::fs::mknodat(
        rustix::fs::CWD,
        path,
        rustix::fs::FileType::CharacterDevice,
        rustix::fs::Mode::from_raw_mode(0o644),
        device,
    );
    if mknod.is_ok() {
        return Ok(());
    }

    std::fs::write(path, b"")?;
    let trusted = rustix::fs::setxattr(
        path,
        TRUSTED_OVERLAY_WHITEOUT_XATTR,
        b"y",
        rustix::fs::XattrFlags::empty(),
    );
    let user = rustix::fs::setxattr(
        path,
        USER_OVERLAY_WHITEOUT_XATTR,
        b"y",
        rustix::fs::XattrFlags::empty(),
    );
    if trusted.is_err() && user.is_err() {
        let _ = std::fs::remove_file(path);
        return Err(LayerStackError::Storage(format!(
            "failed to mark overlay whiteout {}: mknod={:?}, trusted={:?}, user={:?}",
            path.display(),
            mknod.err(),
            trusted.err(),
            user.err()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn write_kernel_whiteout(path: &Path) -> Result<(), LayerStackError> {
    let logical = logical_whiteout_path_for_target(path);
    if let Some(parent) = logical.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(logical, b"")?;
    Ok(())
}

/// Mark `dir` opaque inside a layer directory: the `.wh..wh..opq` marker for
/// the name-based readers plus, on Linux, the kernel overlay opaque xattr so
/// live session mounts mask lower content and hide the marker itself — the
/// same dual encoding squash `flatten` writes.
#[cfg(target_os = "linux")]
pub(crate) fn write_opaque_dir_marker(dir: &Path) -> Result<(), LayerStackError> {
    write_kernel_whiteout(&dir.join(OPAQUE_MARKER))?;
    let trusted = rustix::fs::lsetxattr(
        dir,
        TRUSTED_OVERLAY_OPAQUE_XATTR,
        b"y",
        rustix::fs::XattrFlags::empty(),
    );
    let user = rustix::fs::lsetxattr(
        dir,
        USER_OVERLAY_OPAQUE_XATTR,
        b"y",
        rustix::fs::XattrFlags::empty(),
    );
    if trusted.is_err() && user.is_err() {
        return Err(LayerStackError::Storage(format!(
            "failed to mark opaque dir {}: trusted={:?}, user={:?}",
            dir.display(),
            trusted.err(),
            user.err()
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn write_opaque_dir_marker(dir: &Path) -> Result<(), LayerStackError> {
    std::fs::write(dir.join(OPAQUE_MARKER), b"")?;
    Ok(())
}

pub(crate) fn logical_whiteout_path_for_target(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default();
    let mut whiteout_name = OsString::from(LOGICAL_WHITEOUT_PREFIX);
    whiteout_name.push(name);
    match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => parent.join(whiteout_name),
        None => PathBuf::from(whiteout_name),
    }
}

pub(crate) fn is_kernel_whiteout(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| is_kernel_whiteout_meta(path, &meta))
}

#[cfg(unix)]
pub(crate) fn is_kernel_whiteout_meta(path: &Path, meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_char_device() && meta.rdev() == 0 {
        return true;
    }
    meta.is_file()
        && meta.len() == 0
        && (has_xattr(path, TRUSTED_OVERLAY_WHITEOUT_XATTR)
            || has_xattr(path, USER_OVERLAY_WHITEOUT_XATTR))
}

#[cfg(not(unix))]
pub(crate) fn is_kernel_whiteout_meta(_path: &Path, _meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn has_xattr(path: &Path, name: &str) -> bool {
    let mut value = [0_u8; 1];
    rustix::fs::lgetxattr(path, name, &mut value).is_ok()
}
