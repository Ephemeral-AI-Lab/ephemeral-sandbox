//! Plugin service process *lifecycle* — the daemon's impure half over the
//! host-neutral [`PluginProcessSpec`] launch contract (`eos_plugin::host::route`).
//!
//! The daemon owns spawning the live child (overlay/namespace runner included),
//! the PPC accept handshake, and teardown (`Drop` = `killpg`). The spec data,
//! env construction, and socket-path derivation live host-side.

use std::io::ErrorKind;
#[cfg(all(target_os = "linux", not(test)))]
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(all(target_os = "linux", not(test)))]
use eos_cas::Intent;
#[cfg(all(target_os = "linux", not(test)))]
use eos_cas::{RunMode, RunRequest, ToolCall, WorkspaceRoot};
use eos_plugin::host::route::PluginProcessSpec;
#[cfg(all(target_os = "linux", not(test)))]
use eos_plugin::host::route::ENV_PLUGIN_WORKSPACE_MOUNTED;
use eos_plugin::host::PpcClient;
use eos_plugin::PluginError;
use serde_json::{json, Value};

use crate::error::DaemonError;

#[derive(Debug, Clone)]
pub(super) struct PluginServiceOverlay {
    pub(super) run_dir: PathBuf,
    pub(super) layer_paths: Vec<PathBuf>,
    pub(super) upperdir: PathBuf,
    pub(super) workdir: PathBuf,
}

pub(super) fn spawn_connected_with_overlay(
    spec: &PluginProcessSpec,
    overlay: Option<&PluginServiceOverlay>,
    timeout: Duration,
) -> Result<(PluginServiceProcess, PpcClient), DaemonError> {
    let listener = bind_ppc_listener(&spec.socket_path)?;
    let mut process = spawn_for_overlay(spec, overlay)?;
    match accept_ppc_client(&listener, &mut process, timeout) {
        Ok(client) => Ok((process, client)),
        Err(err) => {
            process.teardown();
            Err(err)
        }
    }
}

pub(super) fn spawn(spec: &PluginProcessSpec) -> Result<PluginServiceProcess, DaemonError> {
    let env = spec.environment();
    let mut command = Command::new(&spec.command[0]);
    command.args(&spec.command[1..]);
    if spec.working_dir.is_dir() {
        command.current_dir(&spec.working_dir);
    }
    command
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
        spec: spec.clone(),
        child,
        process_group_id,
        torn_down: false,
    })
}

fn spawn_for_overlay(
    spec: &PluginProcessSpec,
    overlay: Option<&PluginServiceOverlay>,
) -> Result<PluginServiceProcess, DaemonError> {
    if let Some(overlay) = overlay {
        return spawn_overlay_runner(spec, overlay);
    }
    spawn(spec)
}

#[cfg(all(target_os = "linux", not(test)))]
fn spawn_overlay_runner(
    spec: &PluginProcessSpec,
    overlay: &PluginServiceOverlay,
) -> Result<PluginServiceProcess, DaemonError> {
    let request = overlay_run_request(spec, overlay);
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
        spec: spec.clone(),
        child,
        process_group_id,
        torn_down: false,
    })
}

#[cfg(any(not(target_os = "linux"), test))]
fn spawn_overlay_runner(
    spec: &PluginProcessSpec,
    overlay: &PluginServiceOverlay,
) -> Result<PluginServiceProcess, DaemonError> {
    let _ = (&overlay.layer_paths, &overlay.upperdir, &overlay.workdir);
    spawn(spec)
}

#[cfg(all(target_os = "linux", not(test)))]
fn overlay_run_request(spec: &PluginProcessSpec, overlay: &PluginServiceOverlay) -> RunRequest {
    let mut env = spec.environment();
    env.insert(ENV_PLUGIN_WORKSPACE_MOUNTED, "1".to_owned());
    RunRequest {
        mode: RunMode::FreshNs,
        tool_call: ToolCall {
            invocation_id: format!("plugin-service:{}", spec.key.service_instance_id()),
            caller_id: "plugin-service".to_owned(),
            verb: "plugin_service".into(),
            intent: Intent::ReadOnly,
            args: json!({
                "command": spec.command.clone(),
                "cwd": ".",
                "env": env,
            }),
            background: false,
        },
        workspace_root: WorkspaceRoot(PathBuf::from(&spec.key.workspace_root)),
        layer_paths: overlay.layer_paths.clone(),
        upperdir: Some(overlay.upperdir.clone()),
        workdir: Some(overlay.workdir.clone()),
        ns_fds: None,
        cgroup_path: None,
        timeout_seconds: None,
    }
}

pub(super) fn process_spec_to_json(spec: &PluginProcessSpec) -> Value {
    json!({
        "service_id": spec.key.service_id,
        "service_instance_id": spec.key.service_instance_id(),
        "command": spec.command,
        "package_root": spec.package_root,
        "dependency_root": spec.dependency_root,
        "working_dir": spec.working_dir,
        "socket_path": spec.socket_path,
        "env": spec.environment(),
        "ppc_protocol_version": spec.ppc_protocol_version,
        "process_started": false,
    })
}

#[derive(Debug)]
pub(super) struct PluginServiceProcess {
    spec: PluginProcessSpec,
    child: Child,
    process_group_id: Option<i32>,
    torn_down: bool,
}

impl PluginServiceProcess {
    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn status_json(&mut self) -> Value {
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

    pub(super) fn teardown(&mut self) {
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

impl Drop for PluginServiceProcess {
    fn drop(&mut self) {
        self.teardown();
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
            caller_id: "plugin-service".to_owned(),
            verb: "remount_overlay".into(),
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
                return Ok(PpcClient::new(stream)?);
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
fn new_spec_for_test(
    key: eos_plugin::PluginServiceKey,
    command: Vec<String>,
    ppc_protocol_version: u32,
) -> Result<PluginProcessSpec, PluginError> {
    let socket_root = super::plugin_runtime_config().ppc_root;
    new_spec_with_socket_root(key, command, ppc_protocol_version, socket_root)
}

#[cfg(test)]
fn new_spec_with_socket_root(
    key: eos_plugin::PluginServiceKey,
    command: Vec<String>,
    ppc_protocol_version: u32,
    socket_root: impl AsRef<Path>,
) -> Result<PluginProcessSpec, PluginError> {
    let package_root = default_package_root(&key);
    let dependency_root = default_dependency_root(&key);
    PluginProcessSpec::new_with_package_paths(
        key,
        command,
        package_root,
        dependency_root,
        PathBuf::from("."),
        ppc_protocol_version,
        socket_root,
    )
}

#[cfg(test)]
fn default_package_root(key: &eos_plugin::PluginServiceKey) -> PathBuf {
    PathBuf::from("/eos/runtime/plugins/catalog")
        .join(&key.plugin_id)
        .join(&key.plugin_digest)
}

#[cfg(test)]
fn default_dependency_root(key: &eos_plugin::PluginServiceKey) -> PathBuf {
    PathBuf::from("/eos/runtime/packages")
        .join(&key.plugin_id)
        .join(&key.plugin_digest)
}

#[cfg(test)]
#[path = "../../../tests/plugin_process/mod.rs"]
mod tests;
