//! Safe terminal-pair allocation for command sessions.

#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(target_os = "linux")]
use std::ffi::CStr;
use std::fs::File;
#[cfg(target_os = "linux")]
use std::fs::OpenOptions;
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
const NO_CONTROL: i32 = 0o400;

#[derive(Debug)]
pub struct TerminalPair {
    pub controller: File,
    pub attached: File,
}

#[cfg(target_os = "linux")]
pub fn open_terminal_pair() -> io::Result<TerminalPair> {
    let controller = open_controller()?;
    prepare_attached(&controller)?;
    let attached_path = attached_path(&controller)?;
    let attached = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(NO_CONTROL)
        .open(attached_path)?;
    Ok(TerminalPair {
        controller,
        attached,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn open_terminal_pair() -> io::Result<TerminalPair> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "terminal pairs are only supported on linux",
    ))
}

#[cfg(target_os = "linux")]
fn open_controller() -> io::Result<File> {
    // SAFETY: `posix_openpt` returns either a negative error sentinel or a new
    // owned file descriptor. No Rust references are passed across the FFI call.
    let fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_CLOEXEC | NO_CONTROL) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is freshly returned by `posix_openpt` and is not owned by any
    // other Rust value, so transferring ownership into `File` is sound.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "linux")]
fn prepare_attached(controller: &File) -> io::Result<()> {
    // SAFETY: `controller.as_raw_fd()` is valid for the lifetime of this call;
    // libc does not retain the descriptor after returning.
    if unsafe { libc::grantpt(controller.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `controller.as_raw_fd()` is valid for the lifetime of this call;
    // libc does not retain the descriptor after returning.
    if unsafe { libc::unlockpt(controller.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn attached_path(controller: &File) -> io::Result<PathBuf> {
    // `c_char` is `i8` on x86_64 but `u8` on aarch64; `ptsname_r`/`CStr::from_ptr`
    // take `*mut/*const c_char`, so the buffer element type must follow the target.
    let mut buf = vec![0 as libc::c_char; 128];
    loop {
        // SAFETY: `buf` is writable for `buf.len()` bytes and the descriptor is
        // valid for the lifetime of the call. libc writes a NUL-terminated path
        // or returns an error code.
        let rc = unsafe { libc::ptsname_r(controller.as_raw_fd(), buf.as_mut_ptr(), buf.len()) };
        if rc == 0 {
            // SAFETY: successful `ptsname_r` guarantees a NUL-terminated string
            // in `buf`.
            let path = unsafe { CStr::from_ptr(buf.as_ptr()) }
                .to_str()
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            return Ok(PathBuf::from(path));
        }
        if rc == libc::ERANGE && buf.len() < 4096 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        return Err(io::Error::from_raw_os_error(rc));
    }
}
