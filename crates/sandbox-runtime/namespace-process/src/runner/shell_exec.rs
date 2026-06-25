//! Namespace shell execution shared by setns runner modes.

#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

#[cfg(target_os = "linux")]
use super::RunnerError;
#[cfg(target_os = "linux")]
use crate::runner::protocol::{NamespaceRunnerRequest, RunResult};
#[cfg(target_os = "linux")]
use crate::timing;
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
    let total_started = std::time::Instant::now();
    let prepare_started = std::time::Instant::now();
    let argv = shell_argv(request)?;
    let cwd = shell_cwd(request)?;
    timing::duration("ns_runner.shell.prepare_argv_cwd", prepare_started);
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
    install_command_process_group(&mut command);

    let spawn_started = std::time::Instant::now();
    let mut child = command.spawn().map_err(RunnerError::Child)?;
    timing::duration("ns_runner.shell.spawn_child", spawn_started);
    let child_pgid = child_process_group(&child)?;
    let wait_started = std::time::Instant::now();
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
    timing::duration("ns_runner.shell.wait_scope", wait_started);
    timing::duration("ns_runner.shell.total", total_started);
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
    // SAFETY: `pre_exec` runs in the forked command child immediately before
    // `exec`. The closure only calls async-signal-safe `setpgid(2)` and returns
    // the OS error if it fails.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

#[cfg(target_os = "linux")]
fn child_process_group(child: &std::process::Child) -> Result<i32, RunnerError> {
    i32::try_from(child.id()).map_err(|_| {
        RunnerError::InvalidRequest(format!("child pid does not fit i32: {}", child.id()))
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;
    use crate::runner::protocol::NamespaceRunnerRequest;

    #[test]
    fn command_timeout_survives_as_timed_out_result() {
        let workspace_root = unique_workspace_root();
        std::fs::create_dir_all(&workspace_root).expect("create workspace root");
        let request = NamespaceRunnerRequest {
            request_id: "timeout-regression".to_owned(),
            args: json!({ "command": "sleep 5", "cwd": "." }),
            workspace_root: workspace_root.clone(),
            layer_paths: vec![],
            upperdir: None,
            workdir: None,
            ns_fds: None,
            timeout_seconds: Some(0.05),
        };

        let result = execute_shell(&request).expect("timeout should produce a result envelope");

        assert_eq!(result.exit_code, 124);
        assert_eq!(result.payload["status"], "timed_out");
        let _ = std::fs::remove_dir_all(workspace_root);
    }

    fn unique_workspace_root() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "eos-namespace-process-shell-timeout-{}-{nanos}",
            std::process::id()
        ))
    }
}
