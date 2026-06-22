use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{ManagerError, SandboxDaemonEndpoint, SandboxRecord};

const DAEMON_AUTH_TOKEN_ENV: &str = "SANDBOX_DAEMON_AUTH_TOKEN";
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(2);
const DAEMON_READY_POLL: Duration = Duration::from_millis(20);

pub trait SandboxDaemonInstaller: Send + Sync {
    fn install_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn start_daemon(&self, record: &SandboxRecord) -> Result<SandboxDaemonEndpoint, ManagerError>;

    fn stop_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError>;

    fn check_daemon(&self, _endpoint: &SandboxDaemonEndpoint) -> Result<(), ManagerError> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct LocalSandboxDaemonInstaller {
    executable: PathBuf,
    config_yaml_path: PathBuf,
    runtime_root: PathBuf,
    auth_token: Option<String>,
    ready_timeout: Duration,
}

impl LocalSandboxDaemonInstaller {
    #[must_use]
    pub fn new(
        executable: impl Into<PathBuf>,
        config_yaml_path: impl Into<PathBuf>,
        runtime_root: impl Into<PathBuf>,
        auth_token: Option<String>,
    ) -> Self {
        Self {
            executable: executable.into(),
            config_yaml_path: config_yaml_path.into(),
            runtime_root: runtime_root.into(),
            auth_token,
            ready_timeout: DAEMON_READY_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_ready_timeout(mut self, ready_timeout: Duration) -> Self {
        self.ready_timeout = ready_timeout;
        self
    }

    pub fn launch_spec(
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
            auth_token: self.auth_token.clone(),
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
        let mut command = Command::new(&spec.executable);
        command.args(&spec.args);
        if let Some(auth_token) = spec.auth_token.as_deref() {
            command.env(DAEMON_AUTH_TOKEN_ENV, auth_token);
        }
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
            spec.socket_path,
            spec.auth_token,
        ))
    }

    fn stop_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        let spec = self.launch_spec(record)?;
        let _ = std::fs::remove_file(spec.pid_path);
        let _ = std::fs::remove_file(spec.socket_path);
        Ok(())
    }

    fn check_daemon(&self, endpoint: &SandboxDaemonEndpoint) -> Result<(), ManagerError> {
        let deadline = Instant::now() + self.ready_timeout;
        while Instant::now() < deadline {
            if endpoint.socket_path.exists() {
                return Ok(());
            }
            std::thread::sleep(DAEMON_READY_POLL);
        }
        Err(daemon_install_error(format!(
            "sandbox daemon socket did not appear: {}",
            endpoint.socket_path.display()
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxDaemonLaunchSpec {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    pub auth_token: Option<String>,
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

fn daemon_install_error(message: String) -> ManagerError {
    ManagerError::DaemonInstallFailed { message }
}
