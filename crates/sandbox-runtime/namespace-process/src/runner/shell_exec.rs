//! Namespace shell execution shared by setns runner modes.

#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

#[cfg(target_os = "linux")]
use super::RunnerError;
#[cfg(target_os = "linux")]
use crate::runner::protocol::{NamespaceRunnerRequest, RunResult};

#[cfg(target_os = "linux")]
pub(crate) mod request;
#[cfg(target_os = "linux")]
mod wait;

#[cfg(target_os = "linux")]
use request::*;
#[cfg(target_os = "linux")]
use wait::*;

#[cfg(target_os = "linux")]
pub(crate) fn execute_shell(request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    let argv = shell_argv(request)?;
    let cwd = shell_cwd(request)?;
    // Open a handle to /proc before applying the mount mask, so scope-wait can
    // still enumerate same-pgid descendant processes if a custom config hides it.
    let proc_dir = rustix::fs::open(
        "/proc",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok();
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(command_environment(&request.args))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = command.spawn().map_err(RunnerError::Child)?;
    let (exit_code, timed_out) = match wait_for_command_execution_scope(
        &mut child,
        request.timeout_seconds,
        proc_dir.as_ref().map(std::os::fd::AsFd::as_fd),
    ) {
        Ok(exit_code) => (exit_code, false),
        Err(RunnerError::TimedOut) => (124, true),
        Err(err) => return Err(err),
    };
    Ok(RunResult {
        exit_code,
        payload: serde_json::json!({
            "success": exit_code == 0,
            "status": result_status(exit_code, timed_out),
        }),
    })
}

#[cfg(target_os = "linux")]
const fn result_status(exit_code: i32, timed_out: bool) -> &'static str {
    if timed_out {
        "timed_out"
    } else if exit_code == 0 {
        "ok"
    } else {
        "error"
    }
}
