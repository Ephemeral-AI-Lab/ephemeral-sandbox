use std::env;
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use rustix::io::{fcntl_setfd, FdFlags};
use rustix::pipe::pipe;
use sandbox_runtime_namespace_process::runner::protocol::{NamespaceRunnerRequest, RunResult};

use crate::error::NamespaceExecutionError;
use crate::pty::{open_pty_pair, terminate_pgid, PtyMaster};

pub(crate) const MOUNT_OVERLAY_MODE_FLAG: &str = "--mount-overlay";
const SETUP_WAIT_POLL: Duration = Duration::from_millis(1);

pub trait NsRunnerLauncher: Send + Sync {
    fn spawn_pty(
        &self,
        request: NamespaceRunnerRequest,
        transcript_path: Option<PathBuf>,
        cancelled: Arc<AtomicBool>,
        cgroup_procs_path: Option<PathBuf>,
    ) -> Result<(Box<dyn RunnerChild>, PtyMaster), NamespaceExecutionError>;

    fn spawn_overlay_mount(
        &self,
        request: NamespaceRunnerRequest,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError>;
}

pub trait RunnerChild: Send {
    fn wait_completion(&mut self) -> Result<RunResult, NamespaceExecutionError>;
}

pub(crate) struct ForkRunnerLauncher;

struct ForkRunnerChild {
    child: Child,
    result_read: OwnedFd,
    mode_flag: Option<&'static str>,
    setup_timeout_s: f64,
}

struct SpawnedRunner {
    child: Child,
    result_read: OwnedFd,
    request_write: OwnedFd,
    pgid: i32,
}

static SPAWN_CRITICAL_SECTION: Mutex<()> = Mutex::new(());

impl NsRunnerLauncher for ForkRunnerLauncher {
    fn spawn_pty(
        &self,
        request: NamespaceRunnerRequest,
        transcript_path: Option<PathBuf>,
        cancelled: Arc<AtomicBool>,
        cgroup_procs_path: Option<PathBuf>,
    ) -> Result<(Box<dyn RunnerChild>, PtyMaster), NamespaceExecutionError> {
        let request_bytes = encode_request(&request)?;
        let (mut spawned, master) = spawn_locked(None, |command| {
            let (master, slave) = open_pty_pair().map_err(spawn_error)?;
            command
                .stdin(Stdio::from(slave.try_clone().map_err(spawn_error)?))
                .stdout(Stdio::from(slave.try_clone().map_err(spawn_error)?))
                .stderr(Stdio::from(slave));
            install_pgid_leader_hook(command);
            Ok(master)
        })?;
        place_child_in_cgroup(spawned.child.id(), cgroup_procs_path.as_deref());
        let pgid = spawned.pgid;
        let cancel: Box<dyn Fn() + Send + Sync> = Box::new(move || {
            cancelled.store(true, Ordering::Release);
            terminate_pgid(pgid);
        });
        let pty =
            PtyMaster::spawn(master, Some(pgid), transcript_path, cancel).map_err(spawn_error);
        let pty = match pty {
            Ok(pty) => pty,
            Err(error) => {
                terminate_spawned_child(&mut spawned.child, Some(pgid));
                return Err(error);
            }
        };
        let child = spawned.into_child(&request_bytes, None, 0.0)?;
        Ok((Box::new(child), pty))
    }

    fn spawn_overlay_mount(
        &self,
        request: NamespaceRunnerRequest,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError> {
        let request_bytes = encode_request(&request)?;
        let (spawned, ()) = spawn_locked(Some(MOUNT_OVERLAY_MODE_FLAG), |command| {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            install_pgid_leader_hook(command);
            Ok(())
        })?;
        Ok(Box::new(spawned.into_child(
            &request_bytes,
            Some(MOUNT_OVERLAY_MODE_FLAG),
            setup_timeout_s,
        )?))
    }
}

impl SpawnedRunner {
    fn into_child(
        self,
        request_bytes: &[u8],
        mode_flag: Option<&'static str>,
        setup_timeout_s: f64,
    ) -> Result<ForkRunnerChild, NamespaceExecutionError> {
        let SpawnedRunner {
            mut child,
            result_read,
            request_write,
            pgid,
        } = self;
        if let Err(error) = write_request(request_write, request_bytes) {
            terminate_spawned_child(&mut child, Some(pgid));
            return Err(error);
        }
        Ok(ForkRunnerChild {
            child,
            result_read,
            mode_flag,
            setup_timeout_s,
        })
    }
}

fn spawn_locked<R>(
    mode_flag: Option<&'static str>,
    configure: impl FnOnce(&mut Command) -> Result<R, NamespaceExecutionError>,
) -> Result<(SpawnedRunner, R), NamespaceExecutionError> {
    let _spawn_guard = spawn_lock();
    let (request_read, request_write) = request_pipe()?;
    let (result_read, result_write) = result_pipe()?;
    let mut command = ns_runner_command(
        mode_flag,
        request_read.as_raw_fd(),
        result_write.as_raw_fd(),
    )?;
    let resource = configure(&mut command)?;
    let mut child = command.spawn().map_err(spawn_error)?;
    drop(request_read);
    drop(result_write);
    let pgid = match child_pgid(&child) {
        Ok(pgid) => pgid,
        Err(error) => {
            terminate_spawned_child(&mut child, None);
            return Err(error);
        }
    };
    Ok((
        SpawnedRunner {
            child,
            result_read,
            request_write,
            pgid,
        },
        resource,
    ))
}

impl RunnerChild for ForkRunnerChild {
    fn wait_completion(&mut self) -> Result<RunResult, NamespaceExecutionError> {
        let status = match self.mode_flag {
            Some(mode_flag) => {
                wait_for_child_with_timeout(&mut self.child, mode_flag, self.setup_timeout_s)?
            }
            None => self.child.wait().map_err(spawn_error)?,
        };
        let bytes = read_result_fd(&self.result_read).unwrap_or_default();
        if let Ok(result) = serde_json::from_slice::<RunResult>(&bytes) {
            return Ok(result);
        }
        synthesize_result(status)
    }
}

fn ns_runner_command(
    mode_flag: Option<&str>,
    request_fd: RawFd,
    result_fd: RawFd,
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
        .arg(result_fd.to_string());
    Ok(command)
}

fn write_request(request: OwnedFd, request_bytes: &[u8]) -> Result<(), NamespaceExecutionError> {
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

/// Best-effort placement of the freshly spawned `ns-runner` into the workspace
/// cgroup by writing its pid to the workspace `cgroup.procs`. Membership inherits
/// across the runner re-exec, fork/exec, and setns; a write failure never blocks
/// execution (cgroup accounting degrades to unavailable instead).
fn place_child_in_cgroup(pid: u32, cgroup_procs_path: Option<&Path>) {
    if let Some(path) = cgroup_procs_path {
        let _ = std::fs::write(path, pid.to_string());
    }
}

fn install_pgid_leader_hook(command: &mut Command) {
    // SAFETY: `pre_exec` runs in the forked child immediately before `exec`.
    // The closure only calls async-signal-safe `setpgid(2)` and returns the
    // OS error if it fails; it does not touch shared Rust state.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }
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

fn synthesize_result(status: ExitStatus) -> Result<RunResult, NamespaceExecutionError> {
    let exit_code = status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .unwrap_or(1);
    if exit_code == 0 {
        return Err(NamespaceExecutionError::Completion(
            "runner exited successfully without a valid result envelope".to_owned(),
        ));
    }
    Ok(RunResult {
        exit_code,
        payload: serde_json::json!({ "status": "error" }),
    })
}

fn wait_for_child_with_timeout(
    child: &mut Child,
    mode_flag: &str,
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
        thread::sleep(SETUP_WAIT_POLL);
    }
}

fn setup_timeout_duration(setup_timeout_s: f64) -> Duration {
    let seconds = if setup_timeout_s.is_finite() {
        setup_timeout_s.max(0.0)
    } else {
        0.0
    };
    Duration::from_secs_f64(seconds)
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
        terminate_pgid(pgid);
    } else {
        terminate_child(child, Signal::SIGKILL);
    }
    let _ = child.wait();
}

fn timeout_error(mode_flag: &str) -> NamespaceExecutionError {
    NamespaceExecutionError::Spawn(format!("ns-runner {mode_flag} timed out"))
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

fn spawn_error(error: impl std::fmt::Display) -> NamespaceExecutionError {
    NamespaceExecutionError::Spawn(error.to_string())
}
