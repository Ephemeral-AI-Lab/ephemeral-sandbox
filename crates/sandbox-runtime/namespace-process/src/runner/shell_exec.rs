//! Namespace shell execution shared by setns runner modes.

#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use super::RunnerError;
#[cfg(target_os = "linux")]
use crate::runner::protocol::{NamespaceRunnerRequest, RunResult};
#[cfg(target_os = "linux")]
use crate::runner::shell_security;
#[cfg(target_os = "linux")]
use sandbox_observability::{record, Observer, ObserverConfig, Sink, TraceContext};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;

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
    execute_shell_inner(request)
}

#[cfg(target_os = "linux")]
fn execute_shell_inner(request: &NamespaceRunnerRequest) -> Result<RunResult, RunnerError> {
    let argv = shell_argv(request)?;
    let cwd = shell_cwd(request)?;
    install_termination_signal_handlers()?;
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
    shell_security::prepare_shell_security_policy()?;
    install_command_process_group(&mut command);

    let mut child = spawn_child(&mut command, request)?;
    let child_pgid = child_process_group(&child)?;
    let (exit_code, timed_out) = match wait_for_command_execution_scope(
        &mut child,
        child_pgid,
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

#[cfg(target_os = "linux")]
fn install_command_process_group(command: &mut Command) {
    // SAFETY: `pre_exec` runs in the forked shell-exec child immediately before
    // `exec`. The closure only calls async-signal-safe syscalls over state
    // prepared before forking and returns the OS error if one fails.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            shell_security::apply_shell_security_policy()
        });
    }
}

#[cfg(target_os = "linux")]
fn child_process_group(child: &std::process::Child) -> Result<i32, RunnerError> {
    i32::try_from(child.id()).map_err(|_| {
        RunnerError::InvalidRequest(format!("child pid does not fit i32: {}", child.id()))
    })
}

#[cfg(target_os = "linux")]
fn spawn_child(
    command: &mut Command,
    request: &NamespaceRunnerRequest,
) -> Result<std::process::Child, RunnerError> {
    let Some(ctx) = trace_context(request) else {
        return command.spawn().map_err(RunnerError::Child);
    };
    let Some(path) = request.observability_log_path.clone() else {
        return command.spawn().map_err(RunnerError::Child);
    };
    let obs = Observer::new(
        ObserverConfig {
            proc: record::proc::NAMESPACE_PROCESS,
            enabled: true,
        },
        Sink::new(path),
    );
    obs.with_context(ctx, || {
        obs.scope(record::names::NAMESPACE_RUNNER_SPAWN_CHILD, |_| {
            command.spawn().map_err(RunnerError::Child)
        })
    })
}

#[cfg(target_os = "linux")]
fn trace_context(request: &NamespaceRunnerRequest) -> Option<TraceContext> {
    Some(TraceContext {
        trace: Arc::<str>::from(request.trace.as_deref()?),
        parent: request.parent.as_deref().map(Arc::<str>::from),
    })
}
