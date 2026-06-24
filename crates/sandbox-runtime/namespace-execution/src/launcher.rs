use std::env;
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use rustix::io::{fcntl_setfd, FdFlags};
use rustix::pipe::pipe;
use sandbox_runtime_namespace_process::runner::protocol::{NamespaceRunnerRequest, RunResult};

use crate::error::NamespaceExecutionError;
use crate::pty::{open_pty_pair, terminate_process_group, PtyMaster};

/// The launcher Bridge seam: fork the in-namespace runner and yield a completion
/// event (plus a `PtyMaster` for the interactive path). The fork backing is the
/// only impl in Phase 2; a persistent-server backend would be another `impl`.
pub trait NsRunnerLauncher: Send + Sync {
    fn spawn_pty(
        &self,
        request: NamespaceRunnerRequest,
    ) -> Result<(Box<dyn RunnerChild>, PtyMaster), NamespaceExecutionError>;

    fn spawn_piped(
        &self,
        mode_flag: &'static str,
        request: NamespaceRunnerRequest,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError>;
}

/// The Bridge's completion event: one blocking wait — no poll, no result-fd
/// reader thread.
pub trait RunnerChild: Send {
    fn wait_completion(&mut self) -> Result<RunResult, NamespaceExecutionError>;
}

/// Real fork backing — `current_exe ns-runner …`. Compile-coverage on darwin;
/// exercised at runtime once a real caller wires it (Phase 3/4).
pub(crate) struct ForkRunnerLauncher;

struct ForkRunnerChild {
    child: Child,
    result_read: OwnedFd,
    timeout: Option<PipedCompletionTimeout>,
}

#[derive(Clone, Copy)]
struct PipedCompletionTimeout {
    mode_flag: &'static str,
    setup_timeout_s: f64,
}

static SPAWN_CRITICAL_SECTION: Mutex<()> = Mutex::new(());

impl NsRunnerLauncher for ForkRunnerLauncher {
    fn spawn_pty(
        &self,
        request: NamespaceRunnerRequest,
    ) -> Result<(Box<dyn RunnerChild>, PtyMaster), NamespaceExecutionError> {
        let request_bytes = encode_request(&request)?;
        let (mut child, result_read, start_ack_write, request_write, master, pgid) = {
            let _spawn_guard = spawn_lock();
            let (request_read, request_write) = request_pipe()?;
            let (result_read, result_write) = result_pipe()?;
            let (start_ack_read, start_ack_write) = start_ack_pipe()?;
            let (master, slave) = open_pty_pair().map_err(spawn_error)?;
            let mut command = ns_runner_command(
                None,
                request_read.as_raw_fd(),
                result_write.as_raw_fd(),
                start_ack_read.as_raw_fd(),
            )?;
            command
                .stdin(Stdio::from(slave.try_clone().map_err(spawn_error)?))
                .stdout(Stdio::from(slave.try_clone().map_err(spawn_error)?))
                .stderr(Stdio::from(slave))
                .process_group(0);
            let mut child = command.spawn().map_err(spawn_error)?;
            drop(request_read);
            drop(result_write);
            drop(start_ack_read);
            let pgid = match child_pgid(&child) {
                Ok(pgid) => pgid,
                Err(error) => {
                    terminate_spawned_child(&mut child, None);
                    return Err(error);
                }
            };
            (
                child,
                result_read,
                start_ack_write,
                request_write,
                master,
                pgid,
            )
        };
        let pty = PtyMaster::spawn(
            master,
            Some(pgid),
            Box::new(move || terminate_process_group(pgid)),
        )
        .map_err(spawn_error);
        let pty = match pty {
            Ok(pty) => pty,
            Err(error) => {
                terminate_spawned_child(&mut child, Some(pgid));
                return Err(error);
            }
        };
        if let Err(error) = release_start_ack(start_ack_write, request_write, &request_bytes) {
            terminate_spawned_child(&mut child, Some(pgid));
            return Err(error);
        }
        Ok((
            Box::new(ForkRunnerChild {
                child,
                result_read,
                timeout: None,
            }),
            pty,
        ))
    }

    fn spawn_piped(
        &self,
        mode_flag: &'static str,
        request: NamespaceRunnerRequest,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError> {
        let request_bytes = encode_request(&request)?;
        let (mut child, result_read, start_ack_write, request_write, pgid) = {
            let _spawn_guard = spawn_lock();
            let (request_read, request_write) = request_pipe()?;
            let (result_read, result_write) = result_pipe()?;
            let (start_ack_read, start_ack_write) = start_ack_pipe()?;
            let mut command = ns_runner_command(
                Some(mode_flag),
                request_read.as_raw_fd(),
                result_write.as_raw_fd(),
                start_ack_read.as_raw_fd(),
            )?;
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            let mut child = command.spawn().map_err(spawn_error)?;
            drop(request_read);
            drop(result_write);
            drop(start_ack_read);
            let pgid = match child_pgid(&child) {
                Ok(pgid) => pgid,
                Err(error) => {
                    terminate_spawned_child(&mut child, None);
                    return Err(error);
                }
            };
            (child, result_read, start_ack_write, request_write, pgid)
        };
        if let Err(error) = release_start_ack(start_ack_write, request_write, &request_bytes) {
            terminate_spawned_child(&mut child, Some(pgid));
            return Err(error);
        }
        Ok(Box::new(ForkRunnerChild {
            child,
            result_read,
            timeout: Some(PipedCompletionTimeout {
                mode_flag,
                setup_timeout_s,
            }),
        }))
    }
}

impl RunnerChild for ForkRunnerChild {
    fn wait_completion(&mut self) -> Result<RunResult, NamespaceExecutionError> {
        let status = match self.timeout {
            Some(timeout) => wait_for_child_with_timeout(
                &mut self.child,
                timeout.mode_flag,
                timeout.setup_timeout_s,
            )?,
            None => self.child.wait().map_err(spawn_error)?,
        };
        run_result_from_child_status(status, &self.result_read)
    }
}

fn ns_runner_command(
    mode_flag: Option<&str>,
    request_fd: RawFd,
    result_fd: RawFd,
    start_ack_fd: RawFd,
) -> Result<Command, NamespaceExecutionError> {
    let mut command = Command::new(env::current_exe().map_err(spawn_error)?);
    command.arg("ns-runner");
    if let Some(mode_flag) = mode_flag {
        command.arg(mode_flag);
    }
    command
        .arg("--request-fd")
        .arg(request_fd.to_string())
        .arg("--result-fd")
        .arg(result_fd.to_string())
        .arg("--start-ack-fd")
        .arg(start_ack_fd.to_string());
    Ok(command)
}

/// KEEP start-ack (Phase 6 removes it): release the in-namespace child by writing
/// the ack byte, then write the request. The child `read_exact`s the ack first.
fn release_start_ack(
    start_ack: OwnedFd,
    request: OwnedFd,
    request_bytes: &[u8],
) -> Result<(), NamespaceExecutionError> {
    File::from(start_ack).write_all(b"1").map_err(spawn_error)?;
    File::from(request)
        .write_all(request_bytes)
        .map_err(spawn_error)?;
    Ok(())
}

fn child_pgid(child: &Child) -> Result<i32, NamespaceExecutionError> {
    i32::try_from(child.id()).map_err(|_| {
        NamespaceExecutionError::Spawn(format!("child pid does not fit i32: {}", child.id()))
    })
}

fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
    SPAWN_CRITICAL_SECTION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_result_fd(result_read: &OwnedFd) -> io::Result<Vec<u8>> {
    let mut file = File::from(result_read.try_clone()?);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn run_result_from_child_status(
    status: ExitStatus,
    result_read: &OwnedFd,
) -> Result<RunResult, NamespaceExecutionError> {
    match read_result_fd(result_read) {
        Ok(bytes) => match serde_json::from_slice::<RunResult>(&bytes) {
            Ok(result) => Ok(result),
            Err(error) if status.success() => Err(NamespaceExecutionError::Completion(format!(
                "decode runner result JSON: {error}"
            ))),
            Err(_) => Ok(synthesize_result(status)),
        },
        Err(error) if status.success() => Err(NamespaceExecutionError::Completion(format!(
            "read runner result fd: {error}"
        ))),
        Err(_) => Ok(synthesize_result(status)),
    }
}

fn synthesize_result(status: ExitStatus) -> RunResult {
    let exit_code = status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .unwrap_or(1);
    RunResult {
        exit_code,
        payload: serde_json::json!({ "status": "error" }),
    }
}

fn wait_for_child_with_timeout(
    child: &mut Child,
    mode_flag: &'static str,
    setup_timeout_s: f64,
) -> Result<ExitStatus, NamespaceExecutionError> {
    let deadline = Instant::now() + setup_timeout_duration(setup_timeout_s);
    loop {
        if let Some(status) = child.try_wait().map_err(spawn_error)? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            terminate_child(child, Signal::SIGTERM);
            let grace_deadline = Instant::now() + Duration::from_millis(100);
            while Instant::now() < grace_deadline {
                if child.try_wait().map_err(spawn_error)?.is_some() {
                    return Err(timeout_error(mode_flag));
                }
                thread::sleep(Duration::from_millis(10));
            }
            terminate_child(child, Signal::SIGKILL);
            let _ = child.wait();
            return Err(timeout_error(mode_flag));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn setup_timeout_duration(setup_timeout_s: f64) -> Duration {
    let finite_seconds = if setup_timeout_s.is_finite() {
        setup_timeout_s.max(0.0)
    } else {
        0.0
    };
    Duration::from_secs_f64(finite_seconds)
}

fn terminate_child(child: &mut Child, signal: Signal) {
    let Ok(pid) = i32::try_from(child.id()) else {
        if signal == Signal::SIGKILL {
            let _ = child.kill();
        }
        return;
    };
    let _ = kill(Pid::from_raw(-pid), signal);
    let _ = kill(Pid::from_raw(pid), signal);
}

fn terminate_spawned_child(child: &mut Child, pgid: Option<i32>) {
    if let Some(pgid) = pgid {
        terminate_process_group(pgid);
    } else {
        terminate_child(child, Signal::SIGKILL);
    }
    let _ = child.wait();
}

fn timeout_error(mode_flag: &'static str) -> NamespaceExecutionError {
    NamespaceExecutionError::Timeout { mode_flag }
}

fn encode_request(request: &NamespaceRunnerRequest) -> Result<Vec<u8>, NamespaceExecutionError> {
    serde_json::to_vec(request).map_err(|error| {
        NamespaceExecutionError::Spawn(format!("serialize runner request: {error}"))
    })
}

fn request_pipe() -> Result<(OwnedFd, OwnedFd), NamespaceExecutionError> {
    let (read, write) = pipe().map_err(spawn_error)?;
    fcntl_setfd(&read, FdFlags::empty()).map_err(spawn_error)?;
    fcntl_setfd(&write, FdFlags::CLOEXEC).map_err(spawn_error)?;
    Ok((read, write))
}

fn result_pipe() -> Result<(OwnedFd, OwnedFd), NamespaceExecutionError> {
    let (read, write) = pipe().map_err(spawn_error)?;
    fcntl_setfd(&read, FdFlags::CLOEXEC).map_err(spawn_error)?;
    fcntl_setfd(&write, FdFlags::empty()).map_err(spawn_error)?;
    Ok((read, write))
}

fn start_ack_pipe() -> Result<(OwnedFd, OwnedFd), NamespaceExecutionError> {
    let (read, write) = pipe().map_err(spawn_error)?;
    fcntl_setfd(&read, FdFlags::empty()).map_err(spawn_error)?;
    fcntl_setfd(&write, FdFlags::CLOEXEC).map_err(spawn_error)?;
    Ok((read, write))
}

fn spawn_error(error: impl std::fmt::Display) -> NamespaceExecutionError {
    NamespaceExecutionError::Spawn(error.to_string())
}
