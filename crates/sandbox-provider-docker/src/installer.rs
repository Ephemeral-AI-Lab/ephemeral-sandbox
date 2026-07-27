//! `SandboxDaemonInstaller` over bollard: upload daemon assets into the stopped
//! container, start it, gate readiness with an authenticated daemon request, and
//! best-effort stop it. Removal stays with the runtime's `destroy_sandbox`.

use std::collections::HashSet;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::archive::build_install_archive;
use crate::engine::{DockerEngine, DockerError};
use crate::readiness::{readiness_request_line, validate_readiness_response};
use sandbox_config::configs::manager::DockerRuntimeConfig;
use sandbox_manager::{
    ManagerError, ProgressSink, SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxHttpEndpoint,
    SandboxRecord, StartedDaemon,
};

const ARCHIVE_ROOT: &str = "/";
const READINESS_IO_TIMEOUT: Duration = Duration::from_millis(250);
const DAEMON_ARTIFACT_PREFIX: &str = "sandbox-daemon";
const STORAGE_ADMIN_ARTIFACT_PREFIX: &str = "mpla-storage-admin-v1";
const STORAGE_ADMIN_CONTAINER_PATH: &str =
    "/usr/local/libexec/ephemeral-sandbox/mpla-storage-admin-v1";

/// Docker-backed daemon installer.
pub struct DockerSandboxDaemonInstaller {
    engine: DockerEngine,
}

impl DockerSandboxDaemonInstaller {
    /// Build an installer from the resolved Docker config.
    #[must_use]
    pub fn new(config: DockerRuntimeConfig) -> Self {
        Self {
            engine: DockerEngine::new(config),
        }
    }
}

impl SandboxDaemonInstaller for DockerSandboxDaemonInstaller {
    fn install_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        let config = self.engine.config();
        let daemon_binary = std::fs::read(&config.daemon_binary_path).map_err(|error| {
            daemon_install_failed(format!(
                "read daemon binary {}: {error}",
                config.daemon_binary_path.display()
            ))
        })?;
        let storage_admin_binary_path =
            storage_admin_binary_path(&config.daemon_binary_path).map_err(daemon_install_failed)?;
        let storage_admin_binary = std::fs::read(&storage_admin_binary_path).map_err(|error| {
            daemon_install_failed(format!(
                "read storage administrator binary {}: {error}",
                storage_admin_binary_path.display()
            ))
        })?;
        let config_yaml = std::fs::read(&config.daemon_config_yaml_path).map_err(|error| {
            daemon_install_failed(format!(
                "read daemon config {}: {error}",
                config.daemon_config_yaml_path.display()
            ))
        })?;
        let archive = build_install_archive(
            &config.container_daemon_binary_path,
            &daemon_binary,
            Path::new(STORAGE_ADMIN_CONTAINER_PATH),
            &storage_admin_binary,
            &config.container_daemon_config_yaml_path,
            &config_yaml,
        )
        .map_err(|error| daemon_install_failed(format!("build install archive: {error}")))?;
        self.engine
            .upload_archive(
                record.id.as_str().to_owned(),
                ARCHIVE_ROOT.to_owned(),
                archive,
            )
            .map_err(install_error)
    }

    fn start_daemon(&self, record: &SandboxRecord) -> Result<StartedDaemon, ManagerError> {
        let config = self.engine.config();
        let daemon_port = config.daemon_port;
        let daemon_http_port = config.daemon_http_port;
        let started = self
            .engine
            .start_and_resolve(record.id.as_str().to_owned(), daemon_port, daemon_http_port)
            .map_err(|error| {
                let context = self
                    .engine
                    .capture_failure_context(record.id.as_str().to_owned());
                daemon_install_failed(format!(
                    "start daemon for {}: {error}; container {context}",
                    record.id
                ))
            })?;
        Ok(StartedDaemon {
            daemon: SandboxDaemonEndpoint::new("127.0.0.1", started.port, started.auth_token),
            daemon_http: Some(SandboxHttpEndpoint::new("127.0.0.1", started.http_port)),
        })
    }

    fn stop_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        let stop_timeout_s = i64::try_from(self.engine.config().stop_timeout_s).unwrap_or(i64::MAX);
        self.engine
            .stop_container(record.id.as_str().to_owned(), stop_timeout_s)
            .map_err(install_error)
    }

    fn check_daemon(
        &self,
        record: &SandboxRecord,
        endpoint: &SandboxDaemonEndpoint,
    ) -> Result<(), ManagerError> {
        let timeout = Duration::from_millis(self.engine.config().readiness_timeout_ms);
        let poll_interval = Duration::from_millis(self.engine.config().readiness_poll_ms);
        let sandbox_id = record.id.as_str();
        let mut seen_logs = HashSet::new();
        poll_until_ready_with_progress(endpoint, sandbox_id, timeout, poll_interval, || {
            fail_on_daemon_logs(
                &self.engine.capture_logs(sandbox_id.to_owned()),
                &mut seen_logs,
                None,
            )?;
            fail_if_container_exited(&self.engine, sandbox_id)
        })
        .map_err(|error| {
            readiness_failed(
                &error,
                readiness_failure_message(endpoint, sandbox_id, &error, &self.engine),
            )
        })
    }

    fn check_daemon_with_progress(
        &self,
        record: &SandboxRecord,
        endpoint: &SandboxDaemonEndpoint,
        progress: &ProgressSink,
    ) -> Result<(), ManagerError> {
        let timeout = Duration::from_millis(self.engine.config().readiness_timeout_ms);
        let poll_interval = Duration::from_millis(self.engine.config().readiness_poll_ms);
        let sandbox_id = record.id.as_str();
        let mut seen_logs = HashSet::new();
        poll_until_ready_with_progress(endpoint, sandbox_id, timeout, poll_interval, || {
            fail_on_daemon_logs(
                &self.engine.capture_logs(sandbox_id.to_owned()),
                &mut seen_logs,
                Some(progress),
            )?;
            fail_if_container_exited(&self.engine, sandbox_id)
        })
        .map_err(|error| {
            readiness_failed(
                &error,
                readiness_failure_message(endpoint, sandbox_id, &error, &self.engine),
            )
        })
    }
}

fn storage_admin_binary_path(daemon_binary_path: &Path) -> Result<PathBuf, String> {
    let file_name = daemon_binary_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "daemon binary path has no UTF-8 filename: {}",
                daemon_binary_path.display()
            )
        })?;
    let suffix = file_name
        .strip_prefix(DAEMON_ARTIFACT_PREFIX)
        .ok_or_else(|| {
            format!(
                "daemon binary filename must start with {DAEMON_ARTIFACT_PREFIX:?}: {}",
                daemon_binary_path.display()
            )
        })?;
    Ok(daemon_binary_path.with_file_name(format!("{STORAGE_ADMIN_ARTIFACT_PREFIX}{suffix}")))
}

/// Poll the published port with an authenticated, sandbox-scoped readiness
/// request until the daemon confirms it is ready for this sandbox (a bare TCP
/// connect through Docker's proxy is not a reliable readiness signal), or the
/// deadline elapses.
fn poll_until_ready_with_progress<F>(
    endpoint: &SandboxDaemonEndpoint,
    sandbox_id: &str,
    timeout: Duration,
    poll_interval: Duration,
    mut on_poll: F,
) -> Result<(), String>
where
    F: FnMut() -> Result<(), String>,
{
    let request_line = readiness_request_line(sandbox_id, &endpoint.auth_token)?;
    let deadline = Instant::now() + timeout;
    loop {
        on_poll()?;
        let error = match authenticated_exchange(
            &endpoint.host,
            endpoint.port,
            &request_line,
            sandbox_id,
        ) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        on_poll()?;
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out after {} ms: {error}",
                timeout.as_millis()
            ));
        }
        std::thread::sleep(poll_interval);
    }
}

fn fail_if_container_exited(engine: &DockerEngine, sandbox_id: &str) -> Result<(), String> {
    if let Some(reason) = engine
        .container_exit_reason(sandbox_id.to_owned())
        .map_err(|error| error.to_string())?
    {
        return Err(reason);
    }
    Ok(())
}

fn readiness_failure_message(
    endpoint: &SandboxDaemonEndpoint,
    sandbox_id: &str,
    error: &str,
    engine: &DockerEngine,
) -> String {
    if is_concise_daemon_failure(error) {
        return format!(
            "{error} (sandbox {sandbox_id}, daemon {}:{})",
            endpoint.host, endpoint.port
        );
    }
    let context = engine.capture_failure_context(sandbox_id.to_owned());
    format!(
        "daemon at {}:{} for {sandbox_id} is not ready: {error}; container {context}",
        endpoint.host, endpoint.port
    )
}

fn readiness_failed(error: &str, message: String) -> ManagerError {
    if is_workspace_setup_failure(error) {
        ManagerError::WorkspaceSetupFailed { message }
    } else {
        daemon_install_failed(message)
    }
}

fn is_workspace_setup_failure(message: &str) -> bool {
    is_fatal_daemon_log(message)
}

fn fail_on_daemon_logs(
    logs: &str,
    seen_logs: &mut HashSet<String>,
    progress: Option<&ProgressSink>,
) -> Result<(), String> {
    for line in logs.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !seen_logs.insert(line.to_owned()) {
            continue;
        }
        if let Some(message) = parse_cli_log(line) {
            if let Some(progress) = progress {
                progress.emit(&message);
            }
            if is_fatal_daemon_log(&message) {
                return Err(message);
            }
        } else if line.contains("panicked at ") {
            return Err(line.to_owned());
        }
    }
    Ok(())
}

fn parse_cli_log(line: &str) -> Option<String> {
    let encoded = line.strip_prefix("cli_log(")?.strip_suffix(')')?;
    serde_json::from_str(encoded).ok()
}

fn is_fatal_daemon_log(message: &str) -> bool {
    message.starts_with("layer-stack ")
        || message.starts_with("manifest error:")
        || message.starts_with("file too large:")
        || message.starts_with("could not allocate ")
        || message.starts_with("active manifest changed:")
        || message.starts_with("invalid lease owner:")
}

fn is_concise_daemon_failure(message: &str) -> bool {
    is_fatal_daemon_log(message) || message.contains("panicked at ")
}

fn authenticated_exchange(
    host: &str,
    port: u16,
    request_line: &[u8],
    expected_sandbox_id: &str,
) -> Result<(), String> {
    let mut stream =
        TcpStream::connect((host, port)).map_err(|error| format!("connect: {error}"))?;
    stream.set_read_timeout(Some(READINESS_IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(READINESS_IO_TIMEOUT)).ok();
    stream
        .write_all(request_line)
        .map_err(|error| format!("write: {error}"))?;
    stream.shutdown(Shutdown::Write).ok();
    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    reader
        .read_until(b'\n', &mut response)
        .map_err(|error| format!("read: {error}"))?;
    validate_readiness_response(&response, expected_sandbox_id)
}

fn install_error(error: DockerError) -> ManagerError {
    daemon_install_failed(error.to_string())
}

fn daemon_install_failed(message: String) -> ManagerError {
    ManagerError::DaemonInstallFailed { message }
}
