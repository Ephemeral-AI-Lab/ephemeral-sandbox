//! Plugin service process specifications.
//!
//! The daemon is the impure owner for service process lifecycle. This module
//! keeps the launch contract explicit and keyed by `PluginServiceKey`: every
//! service process gets a stable `/eos/plugin/ppc/*.sock` endpoint plus the
//! environment a small generic harness needs to connect back to the daemon.

use std::collections::BTreeMap;
use std::io::ErrorKind;
#[cfg(all(target_os = "linux", not(test)))]
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use eos_plugin::{PluginError, PluginServiceKey};
#[cfg(all(target_os = "linux", not(test)))]
use eos_protocol::Intent;
#[cfg(all(target_os = "linux", not(test)))]
use eos_runner::{RunMode, RunRequest, ToolCall, WorkspaceRoot};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::ppc_router::PpcClient;
use crate::error::DaemonError;

pub(super) const PLUGIN_PPC_ROOT: &str = "/eos/plugin/ppc";
pub(super) const ENV_PLUGIN_PPC_SOCKET: &str = "EOS_PLUGIN_PPC_SOCKET";
pub(super) const ENV_PLUGIN_LAYER_STACK_ROOT: &str = "EOS_PLUGIN_LAYER_STACK_ROOT";
pub(super) const ENV_PLUGIN_WORKSPACE_ROOT: &str = "EOS_PLUGIN_WORKSPACE_ROOT";
pub(super) const ENV_PLUGIN_ID: &str = "EOS_PLUGIN_ID";
pub(super) const ENV_PLUGIN_DIGEST: &str = "EOS_PLUGIN_DIGEST";
pub(super) const ENV_PLUGIN_SERVICE_ID: &str = "EOS_PLUGIN_SERVICE_ID";
pub(super) const ENV_PLUGIN_SERVICE_PROFILE_DIGEST: &str = "EOS_PLUGIN_SERVICE_PROFILE_DIGEST";
pub(super) const ENV_PLUGIN_PPC_PROTOCOL_VERSION: &str = "EOS_PLUGIN_PPC_PROTOCOL_VERSION";
pub(super) const ENV_PLUGIN_WORKSPACE_MOUNTED: &str = "EOS_PLUGIN_WORKSPACE_MOUNTED";

#[derive(Debug, Clone)]
pub(super) struct PluginServiceOverlay {
    pub(super) run_dir: PathBuf,
    pub(super) layer_paths: Vec<PathBuf>,
    pub(super) upperdir: PathBuf,
    pub(super) workdir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PluginProcessSpec {
    key: PluginServiceKey,
    command: Vec<String>,
    ppc_protocol_version: u32,
    socket_path: PathBuf,
}

impl PluginProcessSpec {
    pub(crate) fn new(
        key: PluginServiceKey,
        command: Vec<String>,
        ppc_protocol_version: u32,
    ) -> Result<Self, PluginError> {
        Self::new_with_socket_root(key, command, ppc_protocol_version, PLUGIN_PPC_ROOT)
    }

    pub(crate) fn new_with_socket_root(
        key: PluginServiceKey,
        command: Vec<String>,
        ppc_protocol_version: u32,
        socket_root: impl AsRef<Path>,
    ) -> Result<Self, PluginError> {
        if command.is_empty() || command[0].trim().is_empty() {
            return Err(PluginError::Manifest(format!(
                "service {} requires a launch command",
                key.service_id
            )));
        }
        if ppc_protocol_version == 0 {
            return Err(PluginError::Manifest(
                "ppc_protocol_version must be positive".to_owned(),
            ));
        }
        let socket_path = socket_path_for_key(&key, socket_root.as_ref());
        Ok(Self {
            key,
            command,
            ppc_protocol_version,
            socket_path,
        })
    }

    pub(crate) fn environment(&self) -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            (
                ENV_PLUGIN_PPC_SOCKET,
                self.socket_path.to_string_lossy().into_owned(),
            ),
            (
                ENV_PLUGIN_LAYER_STACK_ROOT,
                self.key.layer_stack_root.clone(),
            ),
            (ENV_PLUGIN_WORKSPACE_ROOT, self.key.workspace_root.clone()),
            (ENV_PLUGIN_ID, self.key.plugin_id.clone()),
            (ENV_PLUGIN_DIGEST, self.key.plugin_digest.clone()),
            (ENV_PLUGIN_SERVICE_ID, self.key.service_id.clone()),
            (
                ENV_PLUGIN_SERVICE_PROFILE_DIGEST,
                self.key.service_profile_digest.clone(),
            ),
            (
                ENV_PLUGIN_PPC_PROTOCOL_VERSION,
                self.ppc_protocol_version.to_string(),
            ),
            (ENV_PLUGIN_WORKSPACE_MOUNTED, "0".to_owned()),
        ])
    }

    pub(crate) fn service_instance_id(&self) -> String {
        self.key.service_instance_id()
    }

    pub(crate) const fn key(&self) -> &PluginServiceKey {
        &self.key
    }

    pub(crate) fn spawn(&self) -> Result<PluginServiceProcess, DaemonError> {
        let mut env = self.environment();
        env.insert(ENV_PLUGIN_WORKSPACE_MOUNTED, "0".to_owned());
        self.spawn_command(&self.command, env)
    }

    fn spawn_command(
        &self,
        argv: &[String],
        env: BTreeMap<&'static str, String>,
    ) -> Result<PluginServiceProcess, DaemonError> {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn()?;
        let process_group_id = i32::try_from(child.id()).ok();
        Ok(PluginServiceProcess {
            spec: self.clone(),
            child,
            process_group_id,
            torn_down: false,
        })
    }

    pub(crate) fn spawn_connected_with_overlay(
        &self,
        overlay: Option<&PluginServiceOverlay>,
        timeout: Duration,
    ) -> Result<(PluginServiceProcess, PpcClient), DaemonError> {
        let listener = bind_ppc_listener(&self.socket_path)?;
        let mut process = self.spawn_for_overlay(overlay)?;
        match accept_ppc_client(&listener, &mut process, timeout) {
            Ok(client) => Ok((process, client)),
            Err(err) => {
                process.teardown();
                Err(err)
            }
        }
    }

    fn spawn_for_overlay(
        &self,
        overlay: Option<&PluginServiceOverlay>,
    ) -> Result<PluginServiceProcess, DaemonError> {
        if let Some(overlay) = overlay {
            return self.spawn_overlay_runner(overlay);
        }
        self.spawn()
    }

    #[cfg(all(target_os = "linux", not(test)))]
    fn spawn_overlay_runner(
        &self,
        overlay: &PluginServiceOverlay,
    ) -> Result<PluginServiceProcess, DaemonError> {
        let request = self.overlay_run_request(overlay);
        let payload = serde_json::to_vec(&request)
            .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("ns-runner")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| DaemonError::InvalidEnvelope("ns-runner stdin unavailable".to_owned()))?
            .write_all(&payload)?;
        drop(child.stdin.take());
        let process_group_id = i32::try_from(child.id()).ok();
        Ok(PluginServiceProcess {
            spec: self.clone(),
            child,
            process_group_id,
            torn_down: false,
        })
    }

    #[cfg(any(not(target_os = "linux"), test))]
    fn spawn_overlay_runner(
        &self,
        overlay: &PluginServiceOverlay,
    ) -> Result<PluginServiceProcess, DaemonError> {
        let _ = (&overlay.layer_paths, &overlay.upperdir, &overlay.workdir);
        self.spawn()
    }

    #[cfg(all(target_os = "linux", not(test)))]
    fn overlay_run_request(&self, overlay: &PluginServiceOverlay) -> RunRequest {
        let mut env = self.environment();
        env.insert(ENV_PLUGIN_WORKSPACE_MOUNTED, "1".to_owned());
        RunRequest {
            mode: RunMode::FreshNs,
            tool_call: ToolCall {
                invocation_id: format!("plugin-service:{}", self.key.service_instance_id()),
                agent_id: "plugin-service".to_owned(),
                verb: "plugin_service".to_owned(),
                intent: Intent::ReadOnly,
                args: json!({
                    "command": self.command.clone(),
                    "cwd": ".",
                    "env": env,
                }),
                background: false,
            },
            workspace_root: WorkspaceRoot(PathBuf::from(&self.key.workspace_root)),
            layer_paths: overlay.layer_paths.clone(),
            upperdir: Some(overlay.upperdir.clone()),
            workdir: Some(overlay.workdir.clone()),
            ns_fds: None,
            cgroup_path: None,
            timeout_seconds: None,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "service_id": self.key.service_id,
            "service_instance_id": self.key.service_instance_id(),
            "command": self.command,
            "socket_path": self.socket_path,
            "env": self.environment(),
            "ppc_protocol_version": self.ppc_protocol_version,
            "process_started": false,
        })
    }
}

#[derive(Debug)]
pub(super) struct PluginServiceProcess {
    spec: PluginProcessSpec,
    child: Child,
    process_group_id: Option<i32>,
    torn_down: bool,
}

impl PluginServiceProcess {
    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn status_json(&mut self) -> Value {
        let exit_status = self.child.try_wait().ok().flatten();
        let running = exit_status.is_none();
        json!({
            "service_id": self.spec.key.service_id,
            "service_instance_id": self.spec.service_instance_id(),
            "pid": self.child.id(),
            "process_group_id": self.process_group_id,
            "running": running,
            "exit_status": exit_status.and_then(|status| status.code()),
            "socket_path": self.spec.socket_path,
        })
    }

    pub(crate) fn teardown(&mut self) {
        if self.torn_down {
            return;
        }
        self.torn_down = true;
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        terminate_process_group(self.process_group_id);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(target_os = "linux", not(test)))]
pub(super) fn remount_workspace_overlay(
    target_pid: u32,
    workspace_root: &str,
    overlay: &PluginServiceOverlay,
    timeout: Duration,
) -> Result<(), DaemonError> {
    let request = RunRequest {
        mode: RunMode::FreshNs,
        tool_call: ToolCall {
            invocation_id: format!("plugin-service-remount:{target_pid}"),
            agent_id: "plugin-service".to_owned(),
            verb: "remount_overlay".to_owned(),
            intent: Intent::ReadOnly,
            args: json!({}),
            background: false,
        },
        workspace_root: WorkspaceRoot(PathBuf::from(workspace_root)),
        layer_paths: overlay.layer_paths.clone(),
        upperdir: Some(overlay.upperdir.clone()),
        workdir: Some(overlay.workdir.clone()),
        ns_fds: None,
        cgroup_path: None,
        timeout_seconds: None,
    };
    let payload = serde_json::to_vec(&request)
        .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?;
    let mut command = Command::new("nsenter");
    command
        .arg("-t")
        .arg(target_pid.to_string())
        .arg("-U")
        .arg("-m")
        .arg("--preserve-credentials")
        .arg("--")
        .arg(std::env::current_exe()?)
        .arg("ns-runner")
        .arg("--remount-overlay")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|err| {
        DaemonError::OverlayPipeline(format!(
            "failed to spawn nsenter for plugin service remount: {err}"
        ))
    })?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| DaemonError::OverlayPipeline("nsenter stdin unavailable".to_owned()))?
        .write_all(&payload)?;
    drop(child.stdin.take());
    let output = wait_for_helper(child, timeout, "plugin service remount")?;
    if output.status.success() {
        return Ok(());
    }
    Err(DaemonError::OverlayPipeline(format!(
        "plugin service remount failed with status {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(any(not(target_os = "linux"), test))]
// Keep the same fallible signature as the Linux remount path so refresh callers
// stay cfg-free; off-Linux/test builds only validate overlay metadata plumbing.
#[expect(
    clippy::unnecessary_wraps,
    reason = "non-Linux/test parity keeps the Linux fallible helper signature"
)]
pub(super) const fn remount_workspace_overlay(
    _target_pid: u32,
    _workspace_root: &str,
    _overlay: &PluginServiceOverlay,
    _timeout: Duration,
) -> Result<(), DaemonError> {
    Ok(())
}

#[cfg(all(target_os = "linux", not(test)))]
fn wait_for_helper(
    mut child: Child,
    timeout: Duration,
    label: &str,
) -> Result<std::process::Output, DaemonError> {
    let process_group_id = i32::try_from(child.id()).ok();
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output().map_err(DaemonError::from);
        }
        if Instant::now() >= deadline {
            terminate_process_group(process_group_id);
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Err(DaemonError::OverlayPipeline(format!(
                "{label} timed out after {:.3}s: {}",
                timeout.as_secs_f64(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl Drop for PluginServiceProcess {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[cfg(target_os = "linux")]
fn terminate_process_group(process_group_id: Option<i32>) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;

    let Some(process_group_id) = process_group_id else {
        return;
    };
    if killpg(Pid::from_raw(process_group_id), Signal::SIGTERM).is_ok() {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = killpg(Pid::from_raw(process_group_id), Signal::SIGKILL);
    }
}

#[cfg(not(target_os = "linux"))]
const fn terminate_process_group(_process_group_id: Option<i32>) {}

fn socket_path_for_key(key: &PluginServiceKey, socket_root: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(key.service_instance_id().as_bytes());
    hasher.update(b"\0");
    hasher.update(key.plugin_digest.as_bytes());
    let digest = hasher.finalize();
    socket_root.join(format!("{}.sock", lower_hex_16(&digest[..16])))
}

fn lower_hex_16(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn bind_ppc_listener(socket_path: &Path) -> Result<UnixListener, DaemonError> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::remove_file(socket_path) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let listener = UnixListener::bind(socket_path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn accept_ppc_client(
    listener: &UnixListener,
    process: &mut PluginServiceProcess,
    timeout: Duration,
) -> Result<PpcClient, DaemonError> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                stream.set_nonblocking(false)?;
                return PpcClient::new(stream);
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {}
            Err(err) => return Err(err.into()),
        }
        if let Some(status) = process.child.try_wait()? {
            return Err(PluginError::Ensure(format!(
                "plugin service {} exited before PPC connect: {status}",
                process.spec.key.service_id
            ))
            .into());
        }
        if Instant::now() >= deadline {
            return Err(PluginError::Ensure(format!(
                "timed out waiting for plugin service {} to connect PPC socket {}",
                process.spec.key.service_id,
                process.spec.socket_path.display()
            ))
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eos_plugin::{PluginServiceKeyParts, RefreshStrategy, ServiceMode};

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn key(profile: &str) -> std::result::Result<PluginServiceKey, PluginError> {
        PluginServiceKey::new(PluginServiceKeyParts {
            layer_stack_root: "/eos/plugin/layer-stack".to_owned(),
            workspace_root: "/eos/plugin/workspace".to_owned(),
            plugin_id: "demo".to_owned(),
            plugin_digest: "digest-a".to_owned(),
            service_id: "indexer".to_owned(),
            service_profile_digest: profile.to_owned(),
            service_mode: ServiceMode::WorkspaceSnapshotRefresh,
            refresh_strategy: RefreshStrategy::RemountWorkspaceAndNotify,
        })
    }

    #[test]
    fn process_spec_uses_stable_eos_plugin_socket_and_env() -> TestResult {
        let spec = PluginProcessSpec::new(
            key("profile-a")?,
            vec!["demo-indexer".to_owned(), "--stdio".to_owned()],
            1,
        )?;
        let env = spec.environment();

        assert!(env[ENV_PLUGIN_PPC_SOCKET].starts_with("/eos/plugin/ppc/"));
        assert!(std::path::Path::new(&env[ENV_PLUGIN_PPC_SOCKET])
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("sock")));
        assert_eq!(env[ENV_PLUGIN_LAYER_STACK_ROOT], "/eos/plugin/layer-stack");
        assert_eq!(env[ENV_PLUGIN_WORKSPACE_ROOT], "/eos/plugin/workspace");
        assert_eq!(env[ENV_PLUGIN_ID], "demo");
        assert_eq!(env[ENV_PLUGIN_SERVICE_ID], "indexer");
        assert_eq!(env[ENV_PLUGIN_PPC_PROTOCOL_VERSION], "1");
        Ok(())
    }

    #[test]
    fn process_spec_key_changes_socket_path() -> TestResult {
        let first = PluginProcessSpec::new(key("profile-a")?, vec!["svc".to_owned()], 1)?;
        let second = PluginProcessSpec::new(key("profile-b")?, vec!["svc".to_owned()], 1)?;

        assert_ne!(
            first.environment()[ENV_PLUGIN_PPC_SOCKET],
            second.environment()[ENV_PLUGIN_PPC_SOCKET]
        );
        Ok(())
    }

    #[test]
    fn process_spec_rejects_empty_command() -> TestResult {
        let service_key = key("profile-a")?;
        assert!(matches!(
            PluginProcessSpec::new(service_key, Vec::new(), 1),
            Err(PluginError::Manifest(message)) if message.contains("launch command")
        ));
        Ok(())
    }

    #[test]
    fn spawned_process_reports_running_then_tears_down() -> TestResult {
        let spec = PluginProcessSpec::new(
            key("profile-a")?,
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "test \"$EOS_PLUGIN_SERVICE_ID\" = indexer && sleep 30".to_owned(),
            ],
            1,
        )?;
        let mut process = spec.spawn()?;

        let status = process.status_json();
        assert_eq!(status["service_id"], "indexer");
        assert_eq!(status["running"], true);
        let pid = status["pid"]
            .as_u64()
            .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidData, "missing process pid"))?;
        assert!(pid > 0);

        process.teardown();
        let status = process.status_json();
        assert_eq!(status["running"], false);
        Ok(())
    }

    #[test]
    fn spawn_connected_accepts_ppc_socket() -> TestResult {
        let root = test_socket_root("spawn-connected");
        let spec = PluginProcessSpec::new_with_socket_root(
            key("profile-a")?,
            vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 30".to_owned()],
            1,
            &root,
        )?;
        let socket_root = root.clone();
        let connector = std::thread::spawn(move || {
            let socket = wait_for_socket(&socket_root)?;
            std::os::unix::net::UnixStream::connect(socket).map(|_| ())
        });

        let (mut process, _client) =
            spec.spawn_connected_with_overlay(None, Duration::from_secs(1))?;
        match connector.join() {
            Ok(result) => result?,
            Err(_) => {
                return Err(std::io::Error::other("connector thread panicked").into());
            }
        }
        process.teardown();
        let _ = std::fs::remove_dir_all(root);
        Ok(())
    }

    fn test_socket_root(name: &str) -> PathBuf {
        let root = PathBuf::from("target").join(format!("ppc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn wait_for_socket(root: &Path) -> std::io::Result<PathBuf> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) == Some("sock") {
                        return Ok(path);
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    format!("timed out waiting for socket under {}", root.display()),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
