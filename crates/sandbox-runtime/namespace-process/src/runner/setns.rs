//! Setns mode: join holder namespaces, optionally mount overlay/DNS, run a command.

#[cfg(target_os = "linux")]
use std::os::fd::RawFd;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use sandbox_runtime_overlay::OverlayHandle;

use super::RunnerError;
#[cfg(target_os = "linux")]
use crate::runner::protocol::NsFds;
use crate::runner::protocol::{NamespaceRunnerRequest, RunResult};

#[cfg(target_os = "linux")]
pub(crate) fn run_setns(request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    run_setns_inner(request)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn run_setns(_request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    Err(RunnerError::Unsupported)
}

/// Mount the overlay inside an existing workspace mount namespace.
#[cfg(target_os = "linux")]
pub fn setns_overlay_mount(
    request: &NamespaceRunnerRequest,
    hidden_paths: &[PathBuf],
) -> Result<(), RunnerError> {
    setns_overlay_mount_inner(request, hidden_paths)
}

#[cfg(not(target_os = "linux"))]
pub fn setns_overlay_mount(
    _request: &NamespaceRunnerRequest,
    _hidden_paths: &[PathBuf],
) -> Result<(), RunnerError> {
    Err(RunnerError::Unsupported)
}

#[cfg(target_os = "linux")]
fn run_setns_inner(request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    let ns_fds = request
        .ns_fds
        .ok_or_else(|| RunnerError::InvalidRequest("setns mode requires ns_fds".to_owned()))?;
    join_namespaces(&ns_fds)?;
    super::shell_exec::execute_shell(request)
}

#[cfg(target_os = "linux")]
fn setns_overlay_mount_inner(
    request: &NamespaceRunnerRequest,
    hidden_paths: &[PathBuf],
) -> Result<(), RunnerError> {
    setns_user_mnt(request, "setns overlay mount")?;
    let upperdir = request.upperdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires upperdir".to_owned())
    })?;
    let workdir = request.workdir.as_ref().ok_or_else(|| {
        RunnerError::InvalidRequest("setns overlay mount requires workdir".to_owned())
    })?;
    let handle = OverlayHandle {
        layer_paths: if request.layer_paths.is_empty() {
            return Err(RunnerError::InvalidRequest(
                "setns overlay mount requires layer_paths".to_owned(),
            ));
        } else {
            request.layer_paths.clone()
        },
        upperdir: upperdir.clone(),
        workdir: workdir.clone(),
    };
    let guard = sandbox_runtime_overlay::mount_overlay(&request.workspace_root, &handle)?;
    super::mask_model_shell_paths(hidden_paths)?;
    // The setns mount helper is a one-shot process. The mounted overlay must
    // outlive this helper and remain pinned by the target mount namespace until
    // isolated teardown, so the unmount-on-drop guard is deliberately leaked.
    std::mem::forget(guard);
    Ok(())
}

#[cfg(target_os = "linux")]
fn join_namespaces(ns_fds: &NsFds) -> Result<(), RunnerError> {
    for (name, fd, nstype) in namespace_fd_order_with_types(ns_fds) {
        setns_fd(name, fd, nstype)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn setns_fd(name: &str, fd: RawFd, nstype: libc::c_int) -> Result<(), RunnerError> {
    // SAFETY: `fd` is a borrowed namespace file descriptor supplied by the
    // daemon to this dedicated single-threaded runner process. `nstype` is the
    // matching CLONE_NEW* constant for that descriptor, and no Rust references
    // are invalidated by the kernel changing the current task's namespace.
    let rc = unsafe { libc::setns(fd, nstype) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    let kind = err.kind();
    Err(RunnerError::Syscall(std::io::Error::new(
        kind,
        format!("setns({name}, fd={fd}, nstype=0x{nstype:x}) failed: {err}"),
    )))
}
