//! Setns mode: join holder namespaces, optionally mount overlay/DNS, run a command.

use std::path::PathBuf;

use super::RunnerError;
use crate::runner::file_op::{FileRunnerError, FileRunnerResult};
use crate::runner::protocol::{NamespaceRunnerRequest, RunResult};

#[cfg(target_os = "linux")]
mod file_op;
#[cfg(target_os = "linux")]
mod mount_overlay;
#[cfg(target_os = "linux")]
pub(crate) mod namespaces;
#[cfg(target_os = "linux")]
pub(crate) mod remount_overlay;
#[cfg(target_os = "linux")]
mod shell;

#[cfg(target_os = "linux")]
pub(crate) fn run_setns(request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    shell::run_setns(request)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn run_setns(_request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    Err(RunnerError::Unsupported)
}

/// Run a file operation inside the session's user+mount namespaces. File-op
/// outcomes (including not-found and not-regular) are returned as values; only a
/// `setns` or transport failure becomes [`FileRunnerError::Io`].
#[cfg(target_os = "linux")]
pub(crate) fn run_file_op_setns(
    request: &NamespaceRunnerRequest,
) -> Result<FileRunnerResult, FileRunnerError> {
    file_op::run_file_op_setns(request)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn run_file_op_setns(
    _request: &NamespaceRunnerRequest,
) -> Result<FileRunnerResult, FileRunnerError> {
    Err(FileRunnerError::Io {
        path: String::new(),
        message: "namespace file runner is only supported on linux".to_owned(),
    })
}

/// Run the staged-switch remount inside the holder's user+mount namespaces.
/// Every post-`setns` outcome is a report of two booleans plus free-form
/// detail; only request decoding and the `setns` itself can error.
#[cfg(target_os = "linux")]
pub fn setns_remount_overlay(
    request: &NamespaceRunnerRequest,
    hidden_paths: &[PathBuf],
) -> Result<RunResult, RunnerError> {
    remount_overlay::run_remount_overlay(request, hidden_paths)
}

#[cfg(not(target_os = "linux"))]
pub fn setns_remount_overlay(
    _request: &NamespaceRunnerRequest,
    _hidden_paths: &[PathBuf],
) -> Result<RunResult, RunnerError> {
    Err(RunnerError::Unsupported)
}

/// Mount the overlay inside an existing workspace mount namespace.
#[cfg(target_os = "linux")]
pub fn setns_overlay_mount(
    request: &NamespaceRunnerRequest,
    hidden_paths: &[PathBuf],
) -> Result<(), RunnerError> {
    mount_overlay::setns_overlay_mount(request, hidden_paths)
}

#[cfg(not(target_os = "linux"))]
pub fn setns_overlay_mount(
    _request: &NamespaceRunnerRequest,
    _hidden_paths: &[PathBuf],
) -> Result<(), RunnerError> {
    Err(RunnerError::Unsupported)
}
