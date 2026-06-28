use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use nix::errno::Errno;
#[cfg(unix)]
use nix::sys::signal::{kill, Signal};
#[cfg(unix)]
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
#[cfg(unix)]
use nix::unistd::Pid;

use crate::{ManagerError, ProgressSink, SandboxDaemonEndpoint, SandboxRecord};

const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(2);
const DAEMON_READY_POLL: Duration = Duration::from_millis(20);
const DAEMON_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const DAEMON_STOP_POLL: Duration = Duration::from_millis(20);
const DAEMON_TCP_HOST: &str = "127.0.0.1";

pub trait SandboxDaemonInstaller: Send + Sync {
    fn install_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError>;

    fn start_daemon(&self, record: &SandboxRecord) -> Result<SandboxDaemonEndpoint, ManagerError>;

    fn stop_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError>;

    fn check_daemon(
        &self,
        record: &SandboxRecord,
        endpoint: &SandboxDaemonEndpoint,
    ) -> Result<(), ManagerError>;

    fn check_daemon_with_progress(
        &self,
        record: &SandboxRecord,
        endpoint: &SandboxDaemonEndpoint,
        _progress: &ProgressSink,
    ) -> Result<(), ManagerError> {
        self.check_daemon(record, endpoint)
    }
}

#[derive(Debug, Clone)]
pub struct LocalSandboxDaemonInstaller {
    executable: PathBuf,
    config_yaml_path: PathBuf,
    runtime_root: PathBuf,
}

impl LocalSandboxDaemonInstaller {
    #[must_use]
    pub fn new(
        executable: impl Into<PathBuf>,
        config_yaml_path: impl Into<PathBuf>,
        runtime_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            executable: executable.into(),
            config_yaml_path: config_yaml_path.into(),
            runtime_root: runtime_root.into(),
        }
    }

    pub(crate) fn launch_spec(
        &self,
        record: &SandboxRecord,
    ) -> Result<SandboxDaemonLaunchSpec, ManagerError> {
        validate_absolute(&record.workspace_root, "workspace_root")?;
        validate_absolute(&self.runtime_root, "daemon runtime root")?;
        let sandbox_runtime_dir = self.runtime_root.join(record.id.as_str());
        let socket_path = sandbox_runtime_dir.join("runtime.sock");
        let pid_path = sandbox_runtime_dir.join("runtime.pid");
        let args = vec![
            "serve".to_owned(),
            "--spawn".to_owned(),
            "--config-yaml".to_owned(),
            self.config_yaml_path.to_string_lossy().into_owned(),
            "--workspace-root".to_owned(),
            record.workspace_root.to_string_lossy().into_owned(),
            "--socket".to_owned(),
            socket_path.to_string_lossy().into_owned(),
            "--pid-file".to_owned(),
            pid_path.to_string_lossy().into_owned(),
            "--sandbox-id".to_owned(),
            record.id.as_str().to_owned(),
        ];
        Ok(SandboxDaemonLaunchSpec {
            executable: self.executable.clone(),
            args,
            socket_path,
            pid_path,
        })
    }
}

impl SandboxDaemonInstaller for LocalSandboxDaemonInstaller {
    fn install_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        let spec = self.launch_spec(record)?;
        create_parent(&spec.socket_path)?;
        create_parent(&spec.pid_path)?;
        Ok(())
    }

    fn start_daemon(&self, record: &SandboxRecord) -> Result<SandboxDaemonEndpoint, ManagerError> {
        let spec = self.launch_spec(record)?;
        let port = reserve_local_port()?;
        let auth_token = uuid::Uuid::new_v4().to_string();
        let port_arg = port.to_string();
        let mut command = Command::new(&spec.executable);
        command.args(&spec.args);
        command.args([
            "--tcp-host",
            DAEMON_TCP_HOST,
            "--tcp-port",
            port_arg.as_str(),
            "--auth-token",
            auth_token.as_str(),
        ]);
        command.stdin(Stdio::null());
        let status = command.status().map_err(|error| {
            daemon_install_error(format!(
                "failed to start sandbox daemon for {}: {error}",
                record.id
            ))
        })?;
        if !status.success() {
            return Err(daemon_install_error(format!(
                "sandbox daemon start for {} exited with {status}",
                record.id
            )));
        }
        Ok(SandboxDaemonEndpoint::new(
            DAEMON_TCP_HOST,
            port,
            auth_token,
        ))
    }

    fn stop_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        let spec = self.launch_spec(record)?;
        if !spec.pid_path.exists() {
            if spec.socket_path.exists() {
                return Err(daemon_install_error(format!(
                    "sandbox daemon socket exists without pid file: socket={} pid_file={}",
                    spec.socket_path.display(),
                    spec.pid_path.display()
                )));
            }
            return Ok(());
        }
        let pid = read_daemon_pid(&spec.pid_path)?;
        terminate_daemon_process(pid)?;
        remove_daemon_file(&spec.pid_path)?;
        remove_daemon_file(&spec.socket_path)?;
        Ok(())
    }

    fn check_daemon(
        &self,
        _record: &SandboxRecord,
        endpoint: &SandboxDaemonEndpoint,
    ) -> Result<(), ManagerError> {
        let deadline = Instant::now() + DAEMON_READY_TIMEOUT;
        loop {
            if TcpStream::connect((endpoint.host.as_str(), endpoint.port)).is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(daemon_install_error(format!(
                    "sandbox daemon did not accept connections at {}:{}",
                    endpoint.host, endpoint.port
                )));
            }
            std::thread::sleep(DAEMON_READY_POLL);
        }
    }
}

fn reserve_local_port() -> Result<u16, ManagerError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| {
        daemon_install_error(format!("failed to reserve local daemon port: {error}"))
    })?;
    let port = listener
        .local_addr()
        .map_err(|error| {
            daemon_install_error(format!("failed to read reserved daemon port: {error}"))
        })?
        .port();
    Ok(port)
}

#[derive(Debug)]
pub(crate) struct SandboxDaemonLaunchSpec {
    pub(crate) executable: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) socket_path: PathBuf,
    pub(crate) pid_path: PathBuf,
}

fn validate_absolute(path: &Path, label: &'static str) -> Result<(), ManagerError> {
    if path.is_absolute() {
        return Ok(());
    }
    Err(daemon_install_error(format!(
        "{label} must be absolute: {}",
        path.display()
    )))
}

fn create_parent(path: &Path) -> Result<(), ManagerError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| {
        daemon_install_error(format!(
            "failed to create daemon directory {}: {error}",
            parent.display()
        ))
    })
}

#[cfg(unix)]
fn read_daemon_pid(pid_path: &Path) -> Result<Pid, ManagerError> {
    let contents = std::fs::read_to_string(pid_path).map_err(|error| {
        daemon_install_error(format!(
            "failed to read daemon pid file {}: {error}",
            pid_path.display()
        ))
    })?;
    let parsed = contents.trim().parse::<i32>().map_err(|error| {
        daemon_install_error(format!(
            "daemon pid file {} is invalid: {error}",
            pid_path.display()
        ))
    })?;
    if parsed <= 0 {
        return Err(daemon_install_error(format!(
            "daemon pid file {} must contain a positive pid",
            pid_path.display()
        )));
    }
    Ok(Pid::from_raw(parsed))
}

#[cfg(unix)]
fn terminate_daemon_process(pid: Pid) -> Result<(), ManagerError> {
    if !daemon_process_exists(pid)? {
        return Ok(());
    }
    send_signal(pid, Signal::SIGTERM, "terminate")?;
    if wait_for_daemon_exit(pid)? {
        return Ok(());
    }
    send_signal(pid, Signal::SIGKILL, "kill")?;
    if wait_for_daemon_exit(pid)? {
        return Ok(());
    }
    Err(daemon_install_error(format!(
        "sandbox daemon pid {pid} did not exit after SIGKILL"
    )))
}

#[cfg(unix)]
fn send_signal(pid: Pid, signal: Signal, action: &str) -> Result<(), ManagerError> {
    match kill(pid, Some(signal)) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(daemon_install_error(format!(
            "failed to {action} sandbox daemon pid {pid}: {error}"
        ))),
    }
}

#[cfg(unix)]
fn wait_for_daemon_exit(pid: Pid) -> Result<bool, ManagerError> {
    let deadline = Instant::now() + DAEMON_STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !daemon_process_exists(pid)? {
            return Ok(true);
        }
        std::thread::sleep(DAEMON_STOP_POLL);
    }
    Ok(!daemon_process_exists(pid)?)
}

#[cfg(unix)]
fn daemon_process_exists(pid: Pid) -> Result<bool, ManagerError> {
    if reap_exited_child(pid)? {
        return Ok(false);
    }
    match kill(pid, None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(error) => Err(daemon_install_error(format!(
            "failed to inspect sandbox daemon pid {pid}: {error}"
        ))),
    }
}

#[cfg(unix)]
fn reap_exited_child(pid: Pid) -> Result<bool, ManagerError> {
    match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(..) | WaitStatus::Signaled(..)) => Ok(true),
        Ok(WaitStatus::StillAlive) | Err(Errno::ECHILD) => Ok(false),
        Ok(_) => Ok(false),
        Err(error) => Err(daemon_install_error(format!(
            "failed to wait for sandbox daemon pid {pid}: {error}"
        ))),
    }
}

#[cfg(not(unix))]
fn read_daemon_pid(_pid_path: &Path) -> Result<u32, ManagerError> {
    Err(daemon_install_error(
        "local daemon stop is unsupported on this platform".to_owned(),
    ))
}

#[cfg(not(unix))]
fn terminate_daemon_process(_pid: u32) -> Result<(), ManagerError> {
    Err(daemon_install_error(
        "local daemon stop is unsupported on this platform".to_owned(),
    ))
}

fn remove_daemon_file(path: &Path) -> Result<(), ManagerError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(daemon_install_error(format!(
            "failed to remove daemon file {}: {error}",
            path.display()
        ))),
    }
}

fn daemon_install_error(message: String) -> ManagerError {
    ManagerError::DaemonInstallFailed { message }
}
