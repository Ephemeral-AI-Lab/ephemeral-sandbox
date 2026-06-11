//! Plugin service process lifecycle over the host-neutral
//! [`PluginProcessSpec`] launch contract (`crate::route`).
//!
//! This module owns the PPC accept handshake, the run-request shapes for
//! overlay-backed services, and teardown (`Drop` = `killpg`). The ns-runner
//! binary identity lives behind the injected
//! [`NsRunnerLauncher`]; the spec data, env
//! construction, and socket-path derivation live host-side.

use std::io::ErrorKind;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::route::{PluginProcessSpec, ENV_PLUGIN_WORKSPACE_MOUNTED};
use eos_isolated_workspace::NsRunnerLauncher;
use eos_namespace::protocol::{Intent, RunMode, RunRequest, ToolCall, WorkspaceRoot};
use eos_plugin::PluginError;
use serde::Serialize;
use serde_json::json;

use crate::PluginRuntimeError;

use super::transport::PpcClient;

#[derive(Debug, Clone)]
pub(super) struct PluginServiceOverlay {
    pub(super) run_dir: PathBuf,
    pub(super) layer_paths: Vec<PathBuf>,
    pub(super) upperdir: PathBuf,
    pub(super) workdir: PathBuf,
}

pub(super) fn spawn_connected_with_overlay(
    launcher: &dyn NsRunnerLauncher,
    spec: &PluginProcessSpec,
    overlay: Option<&PluginServiceOverlay>,
    timeout: Duration,
) -> Result<(PluginServiceProcess, PpcClient), PluginRuntimeError> {
    let listener = bind_ppc_listener(&spec.socket_path)?;
    let mut process = match overlay {
        Some(overlay) => spawn_overlay_runner(launcher, spec, overlay)?,
        None => spawn(spec)?,
    };
    match accept_ppc_client(&listener, &mut process, timeout) {
        Ok(client) => Ok((process, client)),
        Err(err) => {
            process.teardown();
            Err(err)
        }
    }
}

pub(super) fn spawn(spec: &PluginProcessSpec) -> Result<PluginServiceProcess, PluginRuntimeError> {
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
    Ok(PluginServiceProcess::from_child(spec.clone(), child))
}

fn spawn_overlay_runner(
    launcher: &dyn NsRunnerLauncher,
    spec: &PluginProcessSpec,
    overlay: &PluginServiceOverlay,
) -> Result<PluginServiceProcess, PluginRuntimeError> {
    let request = overlay_run_request(spec, overlay);
    let child = launcher.spawn_detached(&request)?;
    Ok(PluginServiceProcess::from_child(spec.clone(), child))
}

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

/// One tracked service process's status snapshot. Field order is the wire
/// order; the adapter serializes this directly into responses.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceProcessStatus {
    pub service_id: String,
    pub service_instance_id: String,
    pub pid: u32,
    pub process_group_id: Option<i32>,
    pub running: bool,
    pub exit_status: Option<i32>,
    pub socket_path: PathBuf,
}

#[derive(Debug)]
pub(super) struct PluginServiceProcess {
    spec: PluginProcessSpec,
    child: Child,
    process_group_id: Option<i32>,
    torn_down: bool,
}

impl PluginServiceProcess {
    fn from_child(spec: PluginProcessSpec, child: Child) -> Self {
        let process_group_id = i32::try_from(child.id()).ok();
        Self {
            spec,
            child,
            process_group_id,
            torn_down: false,
        }
    }

    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Whether the child is still running (reaps a finished child's status).
    pub(super) fn is_running(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    pub(super) fn status(&mut self) -> ServiceProcessStatus {
        let exit_status = self.child.try_wait().ok().flatten();
        ServiceProcessStatus {
            service_id: self.spec.key.service_id.clone(),
            service_instance_id: self.spec.service_instance_id(),
            pid: self.child.id(),
            process_group_id: self.process_group_id,
            running: exit_status.is_none(),
            exit_status: exit_status.and_then(|status| status.code()),
            socket_path: self.spec.socket_path.clone(),
        }
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

/// Remount the service's workspace overlay inside the running child's
/// namespaces (the refresh swap step).
pub(super) fn remount_workspace_overlay(
    launcher: &dyn NsRunnerLauncher,
    target_pid: u32,
    workspace_root: &str,
    overlay: &PluginServiceOverlay,
    timeout: Duration,
) -> Result<(), PluginRuntimeError> {
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
    launcher
        .remount_in(target_pid, &request, timeout)
        .map_err(PluginRuntimeError::from)
}

/// SIGTERM the process group, give it a brief grace window, then SIGKILL.
pub(crate) fn terminate_process_group(process_group_id: Option<i32>) {
    let Some(pgid) = process_group_id else {
        return;
    };
    let pid = nix::unistd::Pid::from_raw(pgid);
    if nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGTERM).is_ok() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL);
}

fn bind_ppc_listener(socket_path: &Path) -> Result<UnixListener, PluginRuntimeError> {
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
) -> Result<PpcClient, PluginRuntimeError> {
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
#[path = "../tests/plugin/unit/process.rs"]
mod tests;
