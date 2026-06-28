//! `SandboxDaemonInstaller` over bollard: upload daemon assets into the stopped
//! container, start it, gate readiness with an authenticated daemon request, and
//! best-effort stop it. Removal stays with the runtime's `destroy_sandbox`.

use std::collections::HashSet;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use sandbox_config::configs::manager::DockerRuntimeConfig;
use sandbox_manager::{
    ManagerError, ProgressSink, SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxRecord,
};
use serde_json::Value;

use crate::archive::build_install_archive;
use crate::engine::{DockerEngine, DockerError};
use crate::readiness::{readiness_request_line, validate_readiness_response};

const ARCHIVE_ROOT: &str = "/";
const STOP_TIMEOUT_SECS: i64 = 5;
const READINESS_POLL: Duration = Duration::from_millis(250);
const READINESS_IO_TIMEOUT: Duration = Duration::from_secs(5);

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
        let config_yaml = std::fs::read(&config.daemon_config_yaml_path).map_err(|error| {
            daemon_install_failed(format!(
                "read daemon config {}: {error}",
                config.daemon_config_yaml_path.display()
            ))
        })?;
        let archive = build_install_archive(
            &config.container_daemon_binary_path,
            &daemon_binary,
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

    fn start_daemon(&self, record: &SandboxRecord) -> Result<SandboxDaemonEndpoint, ManagerError> {
        let daemon_port = self.engine.config().daemon_port;
        let started = self
            .engine
            .start_and_resolve(record.id.as_str().to_owned(), daemon_port)
            .map_err(|error| {
                let context = self
                    .engine
                    .capture_failure_context(record.id.as_str().to_owned());
                daemon_install_failed(format!(
                    "start daemon for {}: {error}; container {context}",
                    record.id
                ))
            })?;
        Ok(SandboxDaemonEndpoint::new(
            "127.0.0.1",
            started.port,
            started.auth_token,
        ))
    }

    fn stop_daemon(&self, record: &SandboxRecord) -> Result<(), ManagerError> {
        self.engine
            .stop_container(record.id.as_str().to_owned(), STOP_TIMEOUT_SECS)
            .map_err(install_error)
    }

    fn check_daemon(
        &self,
        record: &SandboxRecord,
        endpoint: &SandboxDaemonEndpoint,
    ) -> Result<(), ManagerError> {
        let timeout = Duration::from_millis(self.engine.config().readiness_timeout_ms);
        let sandbox_id = record.id.as_str();
        poll_until_ready(endpoint, sandbox_id, timeout).map_err(|error| {
            let context = self.engine.capture_failure_context(sandbox_id.to_owned());
            daemon_install_failed(format!(
                "daemon at {}:{} for {sandbox_id} did not become ready within {} ms: {error}; container {context}",
                endpoint.host,
                endpoint.port,
                timeout.as_millis()
            ))
        })
    }

    fn check_daemon_with_progress(
        &self,
        record: &SandboxRecord,
        endpoint: &SandboxDaemonEndpoint,
        progress: &ProgressSink,
    ) -> Result<(), ManagerError> {
        let timeout = Duration::from_millis(self.engine.config().readiness_timeout_ms);
        let sandbox_id = record.id.as_str();
        let mut seen_logs = HashSet::new();
        poll_until_ready_with_progress(
            endpoint,
            sandbox_id,
            timeout,
            || {
                emit_container_progress(
                    &self.engine.capture_logs(sandbox_id.to_owned()),
                    &mut seen_logs,
                    progress,
                    sandbox_id,
                );
            },
        )
        .map_err(|error| {
            let context = self.engine.capture_failure_context(sandbox_id.to_owned());
            daemon_install_failed(format!(
                "daemon at {}:{} for {sandbox_id} did not become ready within {} ms: {error}; container {context}",
                endpoint.host,
                endpoint.port,
                timeout.as_millis()
            ))
        })
    }
}

/// Poll the published port with an authenticated, sandbox-scoped readiness
/// request until the daemon confirms it is ready for this sandbox (a bare TCP
/// connect through Docker's proxy is not a reliable readiness signal), or the
/// deadline elapses.
fn poll_until_ready(
    endpoint: &SandboxDaemonEndpoint,
    sandbox_id: &str,
    timeout: Duration,
) -> Result<(), String> {
    poll_until_ready_with_progress(endpoint, sandbox_id, timeout, || {})
}

fn poll_until_ready_with_progress<F>(
    endpoint: &SandboxDaemonEndpoint,
    sandbox_id: &str,
    timeout: Duration,
    mut on_poll: F,
) -> Result<(), String>
where
    F: FnMut(),
{
    let request_line = readiness_request_line(sandbox_id, &endpoint.auth_token);
    let deadline = Instant::now() + timeout;
    loop {
        on_poll();
        let error = match authenticated_exchange(
            &endpoint.host,
            endpoint.port,
            &request_line,
            sandbox_id,
        ) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if Instant::now() >= deadline {
            on_poll();
            return Err(error);
        }
        std::thread::sleep(READINESS_POLL);
    }
}

fn emit_container_progress(
    logs: &str,
    seen_logs: &mut HashSet<String>,
    progress: &ProgressSink,
    default_sandbox_id: &str,
) {
    for line in logs.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if !seen_logs.insert(line.to_owned()) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let progress_value = value.get("progress").unwrap_or(&value);
        if progress_value.get("event").and_then(Value::as_str) != Some("progress")
            && value.get("event").and_then(Value::as_str) != Some("progress")
        {
            continue;
        }
        let op = progress_value
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("daemon");
        let phase = progress_value
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("log");
        let state = progress_value
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("info");
        let message = progress_value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("");
        let sandbox_id = progress_value
            .get("sandbox_id")
            .and_then(Value::as_str)
            .unwrap_or(default_sandbox_id);
        progress.emit(op, phase, state, message, Some(sandbox_id));
    }
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn emit_container_progress_relays_json_progress_lines_once() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let progress = ProgressSink::new({
            let events = Arc::clone(&events);
            move |event| events.lock().expect("events lock").push(event)
        });
        let mut seen_logs = HashSet::new();
        let logs = r#"
not json
{"event":"progress","progress":{"op":"daemon.startup","phase":"layerstack.ensure_workspace_base","state":"started","message":"ensuring base","sandbox_id":"sbox-1"}}
{"event":"progress","op":"layerstack.setup","phase":"workspace_base.copy","state":"running","message":"copied files"}
"#;

        emit_container_progress(logs, &mut seen_logs, &progress, "fallback-sbox");
        emit_container_progress(logs, &mut seen_logs, &progress, "fallback-sbox");

        let events = events.lock().expect("events lock");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].op, "daemon.startup");
        assert_eq!(events[0].phase, "layerstack.ensure_workspace_base");
        assert_eq!(events[0].sandbox_id.as_deref(), Some("sbox-1"));
        assert_eq!(events[1].op, "layerstack.setup");
        assert_eq!(events[1].phase, "workspace_base.copy");
        assert_eq!(events[1].sandbox_id.as_deref(), Some("fallback-sbox"));
    }
}
