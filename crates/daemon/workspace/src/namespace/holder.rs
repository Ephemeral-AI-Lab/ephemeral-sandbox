#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, IntoRawFd};
#[cfg(all(target_os = "linux", unix))]
use std::os::unix::process::ExitStatusExt;
#[cfg(target_os = "linux")]
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
#[cfg(target_os = "linux")]
use std::sync::{Mutex, MutexGuard, OnceLock};
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use nix::fcntl::OFlag;
#[cfg(target_os = "linux")]
use nix::sys::signal::{kill, Signal};
#[cfg(target_os = "linux")]
use nix::unistd::{close, pipe2, Pid};

use crate::isolated_workspace::error::IsolatedError;
use crate::isolated_workspace::manager::WorkspaceHandle;

#[cfg(target_os = "linux")]
use super::fds::{clear_cloexec, expect_line, set_nonblocking};
#[cfg(any(target_os = "linux", test))]
use super::setup_error;
use super::{HolderKillReport, NamespaceRuntime};

impl NamespaceRuntime {
    pub(crate) fn spawn_ns_holder(
        &self,
        handle: &mut WorkspaceHandle,
        setup_timeout_s: f64,
    ) -> Result<i32, IsolatedError> {
        if self.stub {
            #[cfg(test)]
            {
                let _ = (handle, setup_timeout_s);
                return Ok(self.stub_holder_pid);
            }
            #[cfg(not(test))]
            return Ok(0);
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (handle, setup_timeout_s);
            Ok(0)
        }
        #[cfg(target_os = "linux")]
        {
            let (readiness_read, readiness_write) = pipe2(OFlag::O_CLOEXEC).map_err(setup_error)?;
            let (control_read, control_write) = pipe2(OFlag::O_CLOEXEC).map_err(setup_error)?;
            let readiness_child_fd = readiness_write.as_raw_fd();
            let control_child_fd = control_read.as_raw_fd();
            clear_cloexec(readiness_child_fd)?;
            clear_cloexec(control_child_fd)?;
            let mut child = Command::new(std::env::current_exe().map_err(setup_error)?)
                .arg("ns-holder")
                .arg(readiness_child_fd.to_string())
                .arg(control_child_fd.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(setup_error)?;
            drop(readiness_write);
            drop(control_read);
            let readiness_fd = readiness_read.into_raw_fd();
            let control_fd = control_write.into_raw_fd();
            handle.readiness_fd = readiness_fd;
            handle.control_fd = control_fd;
            if let Err(error) = set_nonblocking(readiness_fd)
                .and_then(|()| expect_line(readiness_fd, b"ns-up", setup_timeout_s))
            {
                let stderr = child.stderr.take();
                let error = ns_holder_startup_error(error, &mut child, stderr);
                let _ = close(readiness_fd);
                let _ = close(control_fd);
                return Err(error);
            }
            let Ok(holder_pid) = i32::try_from(child.id()) else {
                let stderr = child.stderr.take();
                let error = ns_holder_startup_error(
                    setup_error(format!("ns-holder pid does not fit i32: {}", child.id())),
                    &mut child,
                    stderr,
                );
                let _ = close(readiness_fd);
                let _ = close(control_fd);
                return Err(error);
            };
            lock_holder_children()?.insert(holder_pid, child);
            Ok(holder_pid)
        }
    }

    pub(crate) fn kill_holder(
        &self,
        holder_pid: i32,
        grace_s: f64,
    ) -> Result<HolderKillReport, IsolatedError> {
        if self.stub || holder_pid <= 0 {
            #[cfg(test)]
            if self.stub && holder_pid > 0 {
                if let Some(killed_holders) = self.killed_holders.as_ref() {
                    killed_holders
                        .lock()
                        .map_err(|_| setup_error("stub holder kill log lock poisoned"))?
                        .push(holder_pid);
                }
            }
            return Ok(HolderKillReport::default());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = grace_s;
            Ok(HolderKillReport::default())
        }
        #[cfg(target_os = "linux")]
        {
            let child = lock_holder_children()?.remove(&holder_pid);
            if let Some(mut child) = child {
                if let Some(status) = child.try_wait().map_err(setup_error)? {
                    return Ok(holder_kill_report(false, status));
                }
                let _ = kill(Pid::from_raw(holder_pid), Signal::SIGTERM);
                let deadline = Instant::now() + Duration::from_secs_f64(grace_s.max(0.0));
                while Instant::now() < deadline {
                    if let Some(status) = child.try_wait().map_err(setup_error)? {
                        return Ok(holder_kill_report(true, status));
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                let _ = kill(Pid::from_raw(holder_pid), Signal::SIGKILL);
                let status = child.wait().map_err(setup_error)?;
                return Ok(holder_kill_report(true, status));
            } else {
                let holder_was_alive = kill(Pid::from_raw(holder_pid), Signal::SIGTERM).is_ok();
                if holder_was_alive {
                    thread::sleep(Duration::from_secs_f64(grace_s.max(0.0)));
                    let _ = kill(Pid::from_raw(holder_pid), Signal::SIGKILL);
                }
                return Ok(HolderKillReport {
                    holder_was_alive,
                    ..HolderKillReport::default()
                });
            }
        }
    }
}

#[cfg(all(target_os = "linux", unix))]
fn holder_kill_report(
    holder_was_alive: bool,
    status: std::process::ExitStatus,
) -> HolderKillReport {
    HolderKillReport {
        holder_was_alive,
        exit_status: status.code(),
        signal: status.signal(),
        status_raw: Some(status.into_raw()),
    }
}

#[cfg(target_os = "linux")]
fn holder_children() -> &'static Mutex<HashMap<i32, Child>> {
    static CHILDREN: OnceLock<Mutex<HashMap<i32, Child>>> = OnceLock::new();
    CHILDREN.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(target_os = "linux")]
fn lock_holder_children() -> Result<MutexGuard<'static, HashMap<i32, Child>>, IsolatedError> {
    holder_children()
        .lock()
        .map_err(|_| setup_error("ns-holder child registry lock poisoned"))
}

#[cfg(target_os = "linux")]
fn ns_holder_startup_error(
    error: IsolatedError,
    child: &mut Child,
    stderr: Option<ChildStderr>,
) -> IsolatedError {
    let original_step = match error {
        IsolatedError::SetupFailed { step } => step,
        other => other.to_string(),
    };
    let _ = child.kill();
    let status = child.wait().ok();
    let stderr = read_child_stderr(stderr);
    IsolatedError::SetupFailed {
        step: format!(
            "{original_step}; ns-holder {}; stderr: {}",
            format_exit_status(status.as_ref()),
            stderr_summary(&stderr)
        ),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn ns_holder_runtime_error(
    error: IsolatedError,
    holder_pid: i32,
) -> Result<IsolatedError, IsolatedError> {
    let original_step = match error {
        IsolatedError::SetupFailed { step } => step,
        other => other.to_string(),
    };
    let Some(mut child) = lock_holder_children()?.remove(&holder_pid) else {
        return Ok(IsolatedError::SetupFailed {
            step: format!("{original_step}; ns-holder child {holder_pid} was not tracked"),
        });
    };
    let stderr = child.stderr.take();
    Ok(ns_holder_startup_error(
        IsolatedError::SetupFailed {
            step: original_step,
        },
        &mut child,
        stderr,
    ))
}

#[cfg(target_os = "linux")]
fn read_child_stderr(stderr: Option<ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut output = String::new();
    let _ = stderr.read_to_string(&mut output);
    output
}

#[cfg(target_os = "linux")]
fn stderr_summary(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        "<empty>".to_owned()
    } else {
        trimmed.replace('\n', " | ")
    }
}

#[cfg(target_os = "linux")]
fn format_exit_status(status: Option<&ExitStatus>) -> String {
    let Some(status) = status else {
        return "exit status unavailable".to_owned();
    };
    if let Some(code) = status.code() {
        return format!("exited with status {code}");
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return format!("terminated by signal {signal}");
    }
    status.to_string()
}
