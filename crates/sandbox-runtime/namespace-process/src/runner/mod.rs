//! Namespace command runner.

use protocol::{NamespaceRunnerRequest, RunResult};
#[cfg(target_os = "linux")]
use rustix::io::Errno;
#[cfg(target_os = "linux")]
use rustix::mount::{unmount, UnmountFlags};
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod protocol;
pub mod setns;
pub(crate) mod shell_exec;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RunnerError {
    #[error("namespace syscall failed: {0}")]
    Syscall(#[source] std::io::Error),
    #[error("invalid namespace runner request: {0}")]
    InvalidRequest(String),
    #[error("overlay mount failed")]
    Overlay(#[source] sandbox_runtime_overlay::OverlayError),
    #[error("child process failed")]
    Child(#[source] std::io::Error),
    #[error("runner command timed out")]
    TimedOut,
    #[error("namespace runner is only supported on linux")]
    Unsupported,
}

impl From<std::io::Error> for RunnerError {
    fn from(err: std::io::Error) -> Self {
        Self::Syscall(err)
    }
}

impl From<sandbox_runtime_overlay::OverlayError> for RunnerError {
    fn from(err: sandbox_runtime_overlay::OverlayError) -> Self {
        Self::Overlay(err)
    }
}

pub fn run(request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    setns::run_setns(request)
}

#[cfg(target_os = "linux")]
pub(crate) fn mask_model_shell_paths(hidden_paths: &[PathBuf]) -> Result<(), RunnerError> {
    for path in hidden_paths {
        if path.exists() {
            mask_with_empty_tmpfs(path)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn unmask_model_shell_paths(hidden_paths: &[PathBuf]) -> Result<(), RunnerError> {
    for path in hidden_paths.iter().rev() {
        unmount_mask(path)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn mask_with_empty_tmpfs(path: &Path) -> Result<(), RunnerError> {
    if !path.is_dir() {
        return Err(RunnerError::InvalidRequest(format!(
            "masked path is not a directory: {}",
            path.display()
        )));
    }
    let target = CString::new(path.as_os_str().as_bytes()).map_err(|err| {
        RunnerError::InvalidRequest(format!("masked path contains an interior nul byte: {err}"))
    })?;
    let tmpfs = CString::new("tmpfs").expect("static string has no nul");
    let data = CString::new("size=4k,mode=000").expect("static string has no nul");
    let flags = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC | libc::MS_RDONLY;

    // SAFETY: strings are NUL-terminated for the syscall, and the runner is in
    // its dedicated mount namespace with CAP_SYS_ADMIN.
    let rc = unsafe {
        libc::mount(
            tmpfs.as_ptr(),
            target.as_ptr(),
            tmpfs.as_ptr(),
            flags,
            data.as_ptr().cast(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(RunnerError::Syscall(std::io::Error::last_os_error()))
    }
}

#[cfg(target_os = "linux")]
fn unmount_mask(path: &Path) -> Result<(), RunnerError> {
    match unmount(path, UnmountFlags::empty()) {
        Ok(()) | Err(Errno::INVAL | Errno::NOENT) => Ok(()),
        Err(_) => unmount(path, UnmountFlags::DETACH)
            .map_err(|err| RunnerError::Syscall(std::io::Error::from(err))),
    }
}
