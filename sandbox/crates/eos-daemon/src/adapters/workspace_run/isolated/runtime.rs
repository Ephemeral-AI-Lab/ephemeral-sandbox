use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, IntoRawFd};
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use eos_layerstack::LayerStack;
#[cfg(target_os = "linux")]
use eos_protocol::Intent;
#[cfg(target_os = "linux")]
use eos_runner::{RunMode, RunRequest, ToolCall, WorkspaceRoot};
use eos_workspace_runtime::isolated::{
    IsolatedError, LayerStackSnapshotPort, NamespaceRuntimePort, SnapshotLease, WorkspaceHandle,
};
#[cfg(target_os = "linux")]
use super::IsolatedCommandHandle;
#[cfg(target_os = "linux")]
use nix::fcntl::OFlag;
#[cfg(target_os = "linux")]
use nix::sys::signal::{kill, Signal};
#[cfg(target_os = "linux")]
use nix::unistd::{close, pipe2, Pid};
#[cfg(target_os = "linux")]
use serde_json::json;

#[cfg(target_os = "linux")]
use super::ns_runner::{
    clear_cloexec, expect_line, lock_holder_children, ns_fds_from_map, open_inheritable_fd,
    run_ns_runner_configure_dns_child, run_ns_runner_mount_overlay_child, set_nonblocking,
    write_all_fd,
};
use super::{setup_error, test_runtime_stub_enabled};

#[derive(Clone)]
pub(super) struct DaemonLayerStackPort {
    pub(super) stack: Arc<Mutex<LayerStack>>,
}

impl LayerStackSnapshotPort for DaemonLayerStackPort {
    fn acquire_snapshot(&self, request_id: &str) -> Result<SnapshotLease, IsolatedError> {
        let lease = {
            let stack = self
                .stack
                .lock()
                .map_err(|_| setup_error("layer stack lock poisoned"))?;
            stack.acquire_snapshot(request_id).map_err(setup_error)?
        };
        Ok(SnapshotLease {
            lease_id: lease.lease_id,
            manifest_version: lease.manifest_version,
            manifest_root_hash: lease.root_hash,
            layer_paths: lease.layer_paths.into_iter().map(PathBuf::from).collect(),
        })
    }

    fn release_lease(&self, lease_id: &str) -> Result<bool, IsolatedError> {
        let mut stack = self
            .stack
            .lock()
            .map_err(|_| setup_error("layer stack lock poisoned"))?;
        stack.release_lease(lease_id).map_err(setup_error)
    }

    fn active_lease_count(&self) -> Result<Option<usize>, IsolatedError> {
        let stack = self
            .stack
            .lock()
            .map_err(|_| setup_error("layer stack lock poisoned"))?;
        Ok(Some(stack.active_lease_count()))
    }
}

#[derive(Default)]
pub(super) struct DaemonNamespaceRuntime;

impl NamespaceRuntimePort for DaemonNamespaceRuntime {
    fn spawn_ns_holder(
        &self,
        handle: &mut WorkspaceHandle,
        setup_timeout_s: f64,
    ) -> Result<i32, IsolatedError> {
        if test_runtime_stub_enabled() {
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
                .stderr(Stdio::null())
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
                let _ = child.kill();
                let _ = child.wait();
                let _ = close(readiness_fd);
                let _ = close(control_fd);
                return Err(error);
            }
            let Ok(holder_pid) = i32::try_from(child.id()) else {
                let _ = child.kill();
                let _ = child.wait();
                let _ = close(readiness_fd);
                let _ = close(control_fd);
                return Err(setup_error(format!(
                    "ns-holder pid does not fit i32: {}",
                    child.id()
                )));
            };
            lock_holder_children()?.insert(holder_pid, child);
            Ok(holder_pid)
        }
    }

    fn open_ns_fds(&self, holder_pid: i32) -> Result<HashMap<String, i32>, IsolatedError> {
        if test_runtime_stub_enabled() || holder_pid <= 0 {
            return Ok(HashMap::new());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = holder_pid;
            Ok(HashMap::new())
        }
        #[cfg(target_os = "linux")]
        {
            let paths = [
                ("user", format!("/proc/{holder_pid}/ns/user")),
                ("mnt", format!("/proc/{holder_pid}/ns/mnt")),
                ("pid", format!("/proc/{holder_pid}/ns/pid_for_children")),
                ("net", format!("/proc/{holder_pid}/ns/net")),
            ];
            paths
                .into_iter()
                .map(|(name, path)| Ok((name.to_owned(), open_inheritable_fd(path)?)))
                .collect()
        }
    }

    fn mount_overlay(
        &self,
        handle: &WorkspaceHandle,
        layer_paths: &[PathBuf],
    ) -> Result<(), IsolatedError> {
        if test_runtime_stub_enabled() || handle.holder_pid <= 0 {
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (handle, layer_paths);
        }
        #[cfg(target_os = "linux")]
        {
            let request = RunRequest {
                mode: RunMode::SetNs,
                tool_call: ToolCall {
                    invocation_id: format!("isolated-mount-{}", handle.workspace_handle_id.0),
                    caller_id: handle.caller_id.0.clone(),
                    verb: "setns_overlay_mount".into(),
                    intent: Intent::WriteAllowed,
                    args: json!({}),
                    background: false,
                },
                workspace_root: WorkspaceRoot(PathBuf::from(&handle.workspace_root)),
                layer_paths: layer_paths.to_vec(),
                upperdir: Some(handle.upperdir.clone()),
                workdir: Some(handle.workdir.clone()),
                ns_fds: ns_fds_from_map(&handle.ns_fds),
                cgroup_path: handle.cgroup_path.clone(),
                timeout_seconds: None,
            };
            run_ns_runner_mount_overlay_child(&request)?;
        }
        Ok(())
    }

    fn configure_dns(
        &self,
        handle: &WorkspaceHandle,
        fallback_dns: &str,
    ) -> Result<bool, IsolatedError> {
        if test_runtime_stub_enabled() || handle.holder_pid <= 0 {
            return Ok(false);
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (handle, fallback_dns);
            Ok(false)
        }
        #[cfg(target_os = "linux")]
        {
            let request = RunRequest {
                mode: RunMode::SetNs,
                tool_call: ToolCall {
                    invocation_id: format!(
                        "isolated-configure-dns-{}",
                        handle.workspace_handle_id.0
                    ),
                    caller_id: handle.caller_id.0.clone(),
                    verb: "configure_dns".into(),
                    intent: Intent::ReadOnly,
                    args: json!({"fallback_dns": fallback_dns}),
                    background: false,
                },
                workspace_root: WorkspaceRoot(PathBuf::from(&handle.workspace_root)),
                layer_paths: vec![],
                upperdir: Some(handle.upperdir.clone()),
                workdir: Some(handle.workdir.clone()),
                ns_fds: ns_fds_from_map(&handle.ns_fds),
                cgroup_path: handle.cgroup_path.clone(),
                timeout_seconds: None,
            };
            run_ns_runner_configure_dns_child(&request)
        }
    }

    fn signal_net_ready(
        &self,
        handle: &WorkspaceHandle,
        setup_timeout_s: f64,
    ) -> Result<(), IsolatedError> {
        if test_runtime_stub_enabled() || handle.holder_pid <= 0 {
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (handle, setup_timeout_s);
        }
        #[cfg(target_os = "linux")]
        {
            let payload = handle.veth.as_ref().map_or_else(
                || "net-ready\n".to_owned(),
                |veth| {
                    format!(
                        "net-ready {} {} {} {}\n",
                        veth.ns_name,
                        veth.ns_ip,
                        eos_workspace_runtime::isolated::BRIDGE_PREFIX_LEN,
                        eos_workspace_runtime::isolated::GATEWAY
                    )
                },
            );
            write_all_fd(handle.control_fd, payload.as_bytes())?;
            expect_line(handle.readiness_fd, b"ready", setup_timeout_s)?;
        }
        Ok(())
    }

    fn create_cgroup(&self, handle: &WorkspaceHandle) -> Result<PathBuf, IsolatedError> {
        if test_runtime_stub_enabled() {
            return Ok(PathBuf::new());
        }
        let path = PathBuf::from(eos_workspace_runtime::isolated::CGROUP_ROOT).join(format!(
            "{}{}",
            eos_workspace_runtime::isolated::HANDLE_PREFIX,
            handle.workspace_handle_id.0
        ));
        std::fs::create_dir_all(&path).map_err(setup_error)?;
        Ok(path)
    }

    fn kill_holder(&self, holder_pid: i32, grace_s: f64) -> Result<(), IsolatedError> {
        if test_runtime_stub_enabled() || holder_pid <= 0 {
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = grace_s;
        }
        #[cfg(target_os = "linux")]
        {
            let _ = kill(Pid::from_raw(holder_pid), Signal::SIGTERM);
            let child = lock_holder_children()?.remove(&holder_pid);
            if let Some(mut child) = child {
                let deadline = Instant::now() + Duration::from_secs_f64(grace_s.max(0.0));
                while Instant::now() < deadline {
                    if child.try_wait().map_err(setup_error)?.is_some() {
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                let _ = kill(Pid::from_raw(holder_pid), Signal::SIGKILL);
                let _ = child.wait();
            } else {
                thread::sleep(Duration::from_secs_f64(grace_s.max(0.0)));
                let _ = kill(Pid::from_raw(holder_pid), Signal::SIGKILL);
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub(super) fn command_handle_from(
    layer_stack_root: &std::path::Path,
    handle: WorkspaceHandle,
) -> IsolatedCommandHandle {
    IsolatedCommandHandle {
        caller_id: handle.caller_id.0,
        workspace_handle_id: handle.workspace_handle_id.0,
        layer_stack_root: layer_stack_root.to_path_buf(),
        manifest_version: handle.manifest_version,
        manifest_root_hash: handle.manifest_root_hash,
        workspace_root: PathBuf::from(handle.workspace_root),
        scratch_dir: handle.scratch_dir,
        upperdir: handle.upperdir,
        workdir: handle.workdir,
        layer_paths: handle.layer_paths,
        ns_fds: handle.ns_fds,
        cgroup_path: handle.cgroup_path,
    }
}
