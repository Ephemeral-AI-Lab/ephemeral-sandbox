#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use ::linux_namespace_subprocess::protocol::{RunMode, RunnerVerb, ToolCall, WorkspaceRoot};
use ::linux_namespace_subprocess::protocol::{RunRequest, RunResult};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
#[cfg(target_os = "linux")]
use serde_json::json;
use serde_json::Value;

#[cfg(target_os = "linux")]
use crate::isolated_network_setup::{BRIDGE_PREFIX_LEN, GATEWAY};
use crate::isolated_workspace::error::IsolatedError;
use crate::isolated_workspace::manager::DnsConfiguration;
use crate::isolated_workspace::manager::WorkspaceHandle;
use crate::lifecycle::remount::{RemountOverlayReport, RemountProbe};

#[cfg(target_os = "linux")]
use super::fds::{expect_line, ns_fds_from_map, write_all_fd};
#[cfg(target_os = "linux")]
use super::holder::ns_holder_runtime_error;
use super::{setup_error, NamespaceRuntime};

impl NamespaceRuntime {
    pub(crate) fn mount_overlay(
        &self,
        handle: &WorkspaceHandle,
        layer_paths: &[PathBuf],
        setup_timeout_s: f64,
    ) -> Result<(), IsolatedError> {
        if self.stub || handle.holder_pid <= 0 {
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (handle, layer_paths, setup_timeout_s);
        }
        #[cfg(target_os = "linux")]
        {
            let request = ns_runner_request(
                handle,
                "mount",
                "setns_overlay_mount",
                json!({}),
                layer_paths.to_vec(),
            );
            mount_overlay_child(&request, setup_timeout_s)?;
        }
        Ok(())
    }

    pub(crate) fn remount_overlay(
        &self,
        handle: &WorkspaceHandle,
        layer_paths: &[PathBuf],
        probe: &RemountProbe,
        setup_timeout_s: f64,
    ) -> Result<RemountOverlayReport, IsolatedError> {
        if self.stub || handle.holder_pid <= 0 {
            return Ok(RemountOverlayReport::verified_stub(layer_paths.len()));
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (handle, layer_paths, probe, setup_timeout_s);
            Ok(RemountOverlayReport::default())
        }
        #[cfg(target_os = "linux")]
        {
            let request = ns_runner_request(
                handle,
                "remount",
                "remount_overlay",
                json!({
                    "probe_path": probe
                        .path
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    "probe_content": probe.expected_content.as_deref(),
                }),
                layer_paths.to_vec(),
            );
            remount_overlay_child(&request, setup_timeout_s)
        }
    }

    pub(crate) fn configure_dns(
        &self,
        handle: &WorkspaceHandle,
        fallback_dns: &str,
        setup_timeout_s: f64,
    ) -> Result<DnsConfiguration, IsolatedError> {
        if self.stub || handle.holder_pid <= 0 {
            return Ok(DnsConfiguration::default());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (handle, fallback_dns, setup_timeout_s);
            Ok(DnsConfiguration::default())
        }
        #[cfg(target_os = "linux")]
        {
            let request = ns_runner_request(
                handle,
                "configure-dns",
                "configure_dns",
                json!({"fallback_dns": fallback_dns}),
                Vec::new(),
            );
            configure_dns_child(&request, setup_timeout_s)
        }
    }

    pub(crate) fn signal_net_ready(
        &self,
        handle: &WorkspaceHandle,
        setup_timeout_s: f64,
    ) -> Result<(), IsolatedError> {
        if self.stub || handle.holder_pid <= 0 {
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
                        veth.ns_name, veth.ns_ip, BRIDGE_PREFIX_LEN, GATEWAY
                    )
                },
            );
            write_all_fd(handle.control_fd, payload.as_bytes())?;
            if let Err(error) = expect_line(handle.readiness_fd, b"ready", setup_timeout_s) {
                return Err(ns_holder_runtime_error(error, handle.holder_pid)?);
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn ns_runner_request(
    handle: &WorkspaceHandle,
    invocation: &str,
    verb: &str,
    args: serde_json::Value,
    layer_paths: Vec<PathBuf>,
) -> RunRequest {
    RunRequest {
        mode: RunMode::SetNs,
        tool_call: ToolCall {
            invocation_id: format!("isolated-{invocation}-{}", handle.workspace_id.0),
            caller_id: handle.caller_id.clone(),
            verb: RunnerVerb::from(verb),
            args,
            background: false,
        },
        workspace_root: WorkspaceRoot(PathBuf::from(&handle.workspace_root)),
        layer_paths,
        upperdir: Some(handle.dirs.upperdir.clone()),
        workdir: Some(handle.dirs.workdir.clone()),
        ns_fds: ns_fds_from_map(&handle.ns_fds),
        cgroup_path: handle.cgroup_path.clone(),
        timeout_seconds: None,
    }
}

pub(super) fn mount_overlay_child(
    request: &RunRequest,
    setup_timeout_s: f64,
) -> Result<(), IsolatedError> {
    let output = run_child(request, "--mount-overlay", Stdio::null(), setup_timeout_s)?;
    if output.status.success() {
        return Ok(());
    }
    Err(IsolatedError::SetupFailed {
        step: format!(
            "ns-runner mount overlay failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ),
    })
}

pub(super) fn remount_overlay_child(
    request: &RunRequest,
    setup_timeout_s: f64,
) -> Result<RemountOverlayReport, IsolatedError> {
    let output = run_child(
        request,
        "--remount-overlay",
        Stdio::piped(),
        setup_timeout_s,
    )?;
    if output.status.success() {
        let result = serde_json::from_slice::<RunResult>(&output.stdout).map_err(|err| {
            IsolatedError::SetupFailed {
                step: format!("invalid ns-runner remount overlay output: {err}"),
            }
        })?;
        return Ok(RemountOverlayReport::from_payload(&result.payload));
    }
    Err(IsolatedError::SetupFailed {
        step: format!(
            "ns-runner remount overlay failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ),
    })
}

pub(super) fn configure_dns_child(
    request: &RunRequest,
    setup_timeout_s: f64,
) -> Result<DnsConfiguration, IsolatedError> {
    let output = run_child(request, "--configure-dns", Stdio::piped(), setup_timeout_s)?;
    if !output.status.success() {
        return Err(IsolatedError::SetupFailed {
            step: format!(
                "ns-runner configure dns failed with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    let result = serde_json::from_slice::<RunResult>(&output.stdout).map_err(|err| {
        IsolatedError::SetupFailed {
            step: format!("invalid ns-runner configure dns output: {err}"),
        }
    })?;
    Ok(DnsConfiguration {
        fallback_applied: result
            .payload
            .get("applied_fallback")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        previous_first_nameserver: result
            .payload
            .get("previous_first_nameserver")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn run_child(
    request: &RunRequest,
    mode_arg: &str,
    stdout: Stdio,
    setup_timeout_s: f64,
) -> Result<Output, IsolatedError> {
    let payload = serde_json::to_vec(request).map_err(setup_error)?;
    let mut child = Command::new(std::env::current_exe().map_err(setup_error)?)
        .arg("ns-runner")
        .arg(mode_arg)
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(setup_error)?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| IsolatedError::SetupFailed {
            step: "ns-runner stdin unavailable".to_owned(),
        })?
        .write_all(&payload)
        .map_err(setup_error)?;
    drop(child.stdin.take());
    let status = wait_for_child(&mut child, mode_arg, setup_timeout_s)?;
    let stdout = read_pipe(child.stdout.take())?;
    let stderr = read_pipe(child.stderr.take())?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn wait_for_child(
    child: &mut Child,
    mode_arg: &str,
    setup_timeout_s: f64,
) -> Result<ExitStatus, IsolatedError> {
    let deadline = Instant::now() + Duration::from_secs_f64(setup_timeout_s.max(0.0));
    loop {
        if let Some(status) = child.try_wait().map_err(setup_error)? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            terminate_child(child, Signal::SIGTERM);
            let grace_deadline = Instant::now() + Duration::from_millis(100);
            while Instant::now() < grace_deadline {
                if let Some(status) = child.try_wait().map_err(setup_error)? {
                    let _ = status;
                    return Err(IsolatedError::SetupFailed {
                        step: format!("ns-runner {mode_arg} timed out"),
                    });
                }
                thread::sleep(Duration::from_millis(10));
            }
            terminate_child(child, Signal::SIGKILL);
            let _ = child.wait();
            return Err(IsolatedError::SetupFailed {
                step: format!("ns-runner {mode_arg} timed out"),
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_child(child: &mut Child, signal: Signal) {
    let Ok(pid) = i32::try_from(child.id()) else {
        let _ = child.kill();
        return;
    };
    let _ = kill(Pid::from_raw(-pid), signal);
    let _ = kill(Pid::from_raw(pid), signal);
}

fn read_pipe<R: Read>(pipe: Option<R>) -> Result<Vec<u8>, IsolatedError> {
    let Some(mut pipe) = pipe else {
        return Ok(Vec::new());
    };
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).map_err(setup_error)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ns_runner_wait_times_out_and_reaps_child_group() -> Result<(), Box<dyn std::error::Error>> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;

        let error = wait_for_child(&mut child, "--test-timeout", 0.01)
            .expect_err("sleeping child should time out");

        assert!(error.to_string().contains("timed out"));
        assert!(
            child.try_wait()?.is_some(),
            "timed out child should be reaped"
        );
        Ok(())
    }
}
