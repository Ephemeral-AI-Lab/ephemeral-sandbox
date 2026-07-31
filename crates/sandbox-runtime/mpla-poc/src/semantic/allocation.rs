use std::fs::File;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::PocResult;

#[cfg(target_os = "linux")]
const FIEMAP_IOCTL: libc::Ioctl = 0xC020_660Bu32 as libc::Ioctl;
#[cfg(target_os = "linux")]
const FIEMAP_EXTENT_LAST: u32 = 0x0000_0001;
#[cfg(target_os = "linux")]
const FIEMAP_BATCH_EXTENTS: usize = 128;

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FiemapHeader {
    fm_start: u64,
    fm_length: u64,
    fm_flags: u32,
    fm_mapped_extents: u32,
    fm_extent_count: u32,
    fm_reserved: u32,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FiemapExtent {
    fe_logical: u64,
    fe_physical: u64,
    fe_length: u64,
    fe_reserved64: [u64; 2],
    fe_flags: u32,
    fe_reserved: [u32; 3],
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct FiemapRequest {
    header: FiemapHeader,
    extents: [FiemapExtent; FIEMAP_BATCH_EXTENTS],
}

#[cfg(target_os = "linux")]
impl Default for FiemapRequest {
    fn default() -> Self {
        Self {
            header: FiemapHeader::default(),
            extents: [FiemapExtent::default(); FIEMAP_BATCH_EXTENTS],
        }
    }
}

/// Returns whether a file has physical extents covering its whole logical
/// range.  This deliberately precedes `SEEK_DATA`: OverlayFS may expose a
/// fully allocated lower file as an all-hole `SEEK_DATA` view even though
/// FIEMAP still reports its physical extents.
pub fn is_fully_allocated(file: &File, path: &Path, logical_size: u64) -> PocResult<bool> {
    #[cfg(target_os = "linux")]
    {
        match fiemap_covers_logical_file(file, logical_size) {
            Ok(full_coverage) => return Ok(full_coverage),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EINVAL | libc::ENOTTY | libc::EOPNOTSUPP)
                ) => {}
            Err(error) => {
                return Err(crate::PocError::io(
                    "inspect semantic file allocation",
                    path,
                    error,
                ))
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (file, path, logical_size);
    Ok(false)
}

#[cfg(target_os = "linux")]
fn fiemap_covers_logical_file(file: &File, logical_size: u64) -> std::io::Result<bool> {
    if logical_size == 0 {
        return Ok(true);
    }
    let mut cursor = 0_u64;
    while cursor < logical_size {
        let mut request = FiemapRequest::default();
        request.header.fm_start = cursor;
        request.header.fm_length = logical_size - cursor;
        request.header.fm_extent_count = u32::try_from(FIEMAP_BATCH_EXTENTS)
            .expect("FIEMAP batch extent count is representable");
        // SAFETY: `request` is a C-compatible fiemap buffer with space for the
        // advertised extents, and `file` stays open for the duration of ioctl.
        if unsafe { libc::ioctl(file.as_raw_fd(), FIEMAP_IOCTL, &mut request) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mapped = usize::try_from(request.header.fm_mapped_extents)
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EOVERFLOW))?;
        if mapped > FIEMAP_BATCH_EXTENTS {
            return Err(std::io::Error::from_raw_os_error(libc::EOVERFLOW));
        }
        if mapped == 0 {
            return Ok(false);
        }
        let mut saw_last = false;
        for extent in request.extents.iter().take(mapped) {
            if extent.fe_logical != cursor || extent.fe_length == 0 {
                return Ok(false);
            }
            cursor = match cursor.checked_add(extent.fe_length) {
                Some(end) if end <= logical_size => end,
                _ => return Ok(false),
            };
            saw_last |= extent.fe_flags & FIEMAP_EXTENT_LAST != 0;
        }
        if cursor == logical_size {
            return Ok(true);
        }
        if saw_last {
            return Ok(false);
        }
    }
    Ok(true)
}
