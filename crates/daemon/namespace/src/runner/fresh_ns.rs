//! Fresh-namespace mode: unshare, mount overlay, spawn the tool.

#[cfg(target_os = "linux")]
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
use rustix::io::Errno;
#[cfg(target_os = "linux")]
use rustix::mount::{mount_change, MountPropagationFlags};
#[cfg(target_os = "linux")]
use rustix::process::{kill_process_group, setsid, Pid, Signal};
#[cfg(target_os = "linux")]
use rustix::thread::{set_thread_gid, set_thread_uid, unshare, UnshareFlags};

#[cfg(target_os = "linux")]
use overlay::OverlayHandle;

use super::RunnerError;
#[cfg(target_os = "linux")]
use crate::protocol::RunnerVerb;
use crate::protocol::{RunRequest, RunResult};
#[cfg(target_os = "linux")]
use serde_json::{json, Value};

#[cfg(target_os = "linux")]
mod child;
#[cfg(target_os = "linux")]
mod command;

#[cfg(target_os = "linux")]
use child::*;
#[cfg(target_os = "linux")]
use command::*;

#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub(crate) struct RunnerPhaseTimings {
    values: serde_json::Map<String, Value>,
}

#[cfg(target_os = "linux")]
impl RunnerPhaseTimings {
    pub(crate) fn insert_s(&mut self, key: &'static str, value: f64) {
        self.values.insert(key.to_owned(), json!(value));
    }

    fn insert_elapsed(&mut self, key: &'static str, started: Instant) {
        self.insert_s(key, started.elapsed().as_secs_f64());
    }

    fn extend(&mut self, other: Self) {
        self.values.extend(other.values);
    }

    fn into_json(mut self, run_start: Instant) -> Value {
        self.insert_elapsed("workspace.tool_s", run_start);
        Value::Object(self.values)
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn run_fresh_ns(
    request: &RunRequest,
    config: &super::config::RunnerConfig,
) -> Result<RunResult, RunnerError> {
    let mut timings = RunnerPhaseTimings::default();
    let namespace_start = Instant::now();
    let namespace_timings = enter_fresh_namespace()?;
    timings.extend(namespace_timings);
    timings.insert_elapsed("workspace.namespace_enter_s", namespace_start);
    if matches!(request.tool_call.verb, RunnerVerb::PluginSetup) {
        return execute_tool(
            request,
            timings,
            Instant::now(),
            Some(&config.mount_mask.hidden_paths),
        );
    }
    let upperdir = request
        .upperdir
        .as_ref()
        .ok_or_else(|| RunnerError::InvalidRequest("fresh-ns requires upperdir".to_owned()))?;
    let workdir = request
        .workdir
        .as_ref()
        .ok_or_else(|| RunnerError::InvalidRequest("fresh-ns requires workdir".to_owned()))?;
    let mount_start = Instant::now();
    let handle = OverlayHandle {
        layer_paths: request.layer_paths.clone(),
        upperdir: upperdir.clone(),
        workdir: workdir.clone(),
    };
    let mount_guard = overlay::mount_overlay(&request.workspace_root.0, &handle)?;
    let mount_s = mount_start.elapsed().as_secs_f64();
    timings.insert_s("workspace.mount_s", mount_s);
    timings.insert_s("workspace.overlay_mount_s", mount_s);

    let mut result = execute_tool(
        request,
        timings,
        Instant::now(),
        Some(&config.mount_mask.hidden_paths),
    )?;
    record_overlay_teardown(&mut result, mount_guard, request.layer_paths.len());
    Ok(result)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn run_fresh_ns(
    _request: &RunRequest,
    _config: &super::config::RunnerConfig,
) -> Result<RunResult, RunnerError> {
    Err(RunnerError::Unsupported)
}

#[cfg(target_os = "linux")]
fn enter_fresh_namespace() -> Result<RunnerPhaseTimings, RunnerError> {
    let mut timings = RunnerPhaseTimings::default();
    let parent_uid = rustix::process::getuid().as_raw();
    let parent_gid = rustix::process::getgid().as_raw();

    let setsid_start = Instant::now();
    if let Err(err) = setsid() {
        // Docker exec may launch the runner as a process-group leader. In that
        // case setsid(2) returns EPERM, but the spawned tool below still gets
        // its own process group for timeout/cancel cleanup.
        if err != Errno::PERM {
            return Err(RunnerError::Syscall(std::io::Error::from(err)));
        }
    }
    timings.insert_elapsed("workspace.namespace_setsid_s", setsid_start);
    let unshare_start = Instant::now();
    unshare(UnshareFlags::NEWUSER | UnshareFlags::NEWNS).map_syscall()?;
    timings.insert_elapsed("workspace.namespace_unshare_s", unshare_start);
    let uid_gid_start = Instant::now();
    write_if_exists("/proc/self/setgroups", "deny\n")?;
    fs::write("/proc/self/uid_map", format!("0 {parent_uid} 1\n")).map_err(RunnerError::Syscall)?;
    fs::write("/proc/self/gid_map", format!("0 {parent_gid} 1\n")).map_err(RunnerError::Syscall)?;
    set_thread_gid(rustix::process::Gid::ROOT).map_syscall()?;
    set_thread_uid(rustix::process::Uid::ROOT).map_syscall()?;
    timings.insert_elapsed("workspace.namespace_uid_gid_map_s", uid_gid_start);
    let mount_private_start = Instant::now();
    mount_change(
        "/",
        MountPropagationFlags::PRIVATE | MountPropagationFlags::REC,
    )
    .map_syscall()?;
    timings.insert_elapsed("workspace.namespace_mount_private_s", mount_private_start);
    Ok(timings)
}

#[cfg(target_os = "linux")]
pub(crate) fn execute_tool(
    request: &RunRequest,
    timings: RunnerPhaseTimings,
    run_start: Instant,
    hidden_paths: Option<&[PathBuf]>,
) -> Result<RunResult, RunnerError> {
    match &request.tool_call.verb {
        RunnerVerb::ExecCommand => execute_shell(request, timings, run_start, hidden_paths),
        RunnerVerb::PluginService => {
            execute_plugin_service(request, timings, run_start, hidden_paths)
        }
        RunnerVerb::PluginSetup => execute_plugin_setup(request, timings, run_start, hidden_paths),
        RunnerVerb::Unknown(verb) => Ok(error_result(
            2,
            "unsupported_runner_verb",
            &format!("fresh namespace runner does not support verb {}", verb),
        )),
    }
}

#[cfg(target_os = "linux")]
fn execute_plugin_setup(
    request: &RunRequest,
    mut timings: RunnerPhaseTimings,
    run_start: Instant,
    hidden_paths: Option<&[PathBuf]>,
) -> Result<RunResult, RunnerError> {
    let prepare_start = Instant::now();
    let argv = plugin_setup_argv(request)?;
    let cwd = plugin_setup_cwd(request)?;
    let setup_tmp_root = setup_path_arg(request, "setup_tmp_root")?;
    mask_plugin_setup_paths(hidden_paths)?;

    let capture_dir = setup_tmp_root.join("tmp");
    fs::create_dir_all(&capture_dir).map_err(RunnerError::Child)?;
    let unique = format!("setup-{}", std::process::id());
    let stdout_path = capture_dir.join(format!("{unique}.stdout"));
    let stderr_path = capture_dir.join(format!("{unique}.stderr"));
    let stdout = fs::File::create(&stdout_path).map_err(RunnerError::Child)?;
    let stderr = fs::File::create(&stderr_path).map_err(RunnerError::Child)?;

    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(setup_environment(&request.tool_call.args))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .process_group(0);
    timings.insert_elapsed("workspace.plugin_prepare_s", prepare_start);

    let spawn_start = Instant::now();
    let mut child = command.spawn().map_err(RunnerError::Child)?;
    timings.insert_elapsed("workspace.plugin_spawn_s", spawn_start);
    let child_pid = Pid::from_child(&child);
    let wait_start = Instant::now();
    let (exit_code, timed_out) = match wait_for_child(&mut child, request.timeout_seconds) {
        Ok(exit_code) => (exit_code, false),
        Err(RunnerError::TimedOut) => (124, true),
        Err(err) => return Err(err),
    };
    timings.insert_elapsed("workspace.plugin_wait_s", wait_start);
    if timed_out {
        let _ = kill_process_group(child_pid, Signal::Kill);
    }
    let stdout_tail = read_tail(&stdout_path)?;
    let stderr_tail = read_tail(&stderr_path)?;
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    Ok(RunResult {
        exit_code,
        payload: serde_json::json!({
            "success": exit_code == 0,
            "workspace": "plugin_setup",
            "timings": timings.into_json(run_start),
            "status": result_status(exit_code, timed_out),
            "exit_code": exit_code,
            "timed_out": timed_out,
            "stdout_tail": stdout_tail,
            "stderr_tail": stderr_tail,
        }),
    })
}

#[cfg(target_os = "linux")]
fn execute_plugin_service(
    request: &RunRequest,
    mut timings: RunnerPhaseTimings,
    run_start: Instant,
    hidden_paths: Option<&[PathBuf]>,
) -> Result<RunResult, RunnerError> {
    let prepare_start = Instant::now();
    let argv = plugin_service_argv(request)?;
    let cwd = shell_cwd(request)?;
    mask_plugin_runtime_paths(hidden_paths)?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(command_environment(&request.tool_call.args))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    timings.insert_elapsed("workspace.plugin_prepare_s", prepare_start);

    let spawn_start = Instant::now();
    let mut child = command.spawn().map_err(RunnerError::Child)?;
    timings.insert_elapsed("workspace.plugin_spawn_s", spawn_start);
    let child_pid = Pid::from_child(&child);
    let wait_start = Instant::now();
    let (exit_code, timed_out) = match wait_for_child(&mut child, request.timeout_seconds) {
        Ok(exit_code) => (exit_code, false),
        Err(RunnerError::TimedOut) => (124, true),
        Err(err) => return Err(err),
    };
    timings.insert_elapsed("workspace.plugin_wait_s", wait_start);
    if timed_out || !matches!(request.mode, crate::protocol::RunMode::SetNs) {
        let _ = kill_process_group(child_pid, Signal::Kill);
    }
    Ok(RunResult {
        exit_code,
        payload: serde_json::json!({
            "success": exit_code == 0,
            "workspace": "ephemeral",
            "timings": timings.into_json(run_start),
            "status": result_status(exit_code, timed_out),
        }),
    })
}

#[cfg(target_os = "linux")]
fn mask_plugin_setup_paths(hidden_paths: Option<&[PathBuf]>) -> Result<(), RunnerError> {
    super::mask_model_shell_paths(&plugin_mask_paths(hidden_paths))
}

#[cfg(target_os = "linux")]
fn mask_plugin_runtime_paths(hidden_paths: Option<&[PathBuf]>) -> Result<(), RunnerError> {
    super::mask_model_shell_paths(&plugin_mask_paths(hidden_paths))
}

#[cfg(target_os = "linux")]
fn plugin_mask_paths(hidden_paths: Option<&[PathBuf]>) -> Vec<PathBuf> {
    let mut paths = hidden_paths
        .unwrap_or_default()
        .iter()
        .filter(|path| path.as_path() != Path::new("/eos"))
        .cloned()
        .collect::<Vec<_>>();
    paths.extend([PathBuf::from("/root"), PathBuf::from("/var")]);
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(target_os = "linux")]
fn read_tail(path: &Path) -> Result<String, RunnerError> {
    const MAX_TAIL_BYTES: usize = 4096;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(RunnerError::Child)?
        .read_to_end(&mut bytes)
        .map_err(RunnerError::Child)?;
    if bytes.len() > MAX_TAIL_BYTES {
        bytes = bytes[bytes.len() - MAX_TAIL_BYTES..].to_vec();
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(target_os = "linux")]
fn execute_shell(
    request: &RunRequest,
    mut timings: RunnerPhaseTimings,
    run_start: Instant,
    hidden_paths: Option<&[PathBuf]>,
) -> Result<RunResult, RunnerError> {
    let prepare_start = Instant::now();
    let argv = shell_argv(request)?;
    let cwd = shell_cwd(request)?;
    // Open a handle to /proc before applying the mount mask, so scope-wait can
    // still enumerate same-pgid background processes if a custom config hides it.
    let proc_dir = rustix::fs::open(
        "/proc",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok();
    if let Some(hidden_paths) = hidden_paths {
        super::mask_model_shell_paths(hidden_paths)?;
    }
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(command_environment(&request.tool_call.args))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    timings.insert_elapsed("workspace.shell_prepare_s", prepare_start);

    let spawn_start = Instant::now();
    let mut child = command.spawn().map_err(RunnerError::Child)?;
    timings.insert_elapsed("workspace.shell_spawn_s", spawn_start);
    let wait_start = Instant::now();
    let (exit_code, timed_out, scope_timing) = match wait_for_command_execution_scope(
        &mut child,
        request.timeout_seconds,
        proc_dir.as_ref().map(std::os::fd::AsFd::as_fd),
    ) {
        Ok((exit_code, timing)) => (exit_code, false, Some(timing)),
        Err(RunnerError::TimedOut) => (124, true, None),
        Err(err) => return Err(err),
    };
    if let Some(scope_timing) = scope_timing {
        record_command_scope_wait_timing(&mut timings, &scope_timing);
    }
    timings.insert_elapsed("workspace.shell_wait_s", wait_start);
    Ok(RunResult {
        exit_code,
        payload: serde_json::json!({
            "success": exit_code == 0,
            "workspace": "ephemeral",
            "timings": timings.into_json(run_start),
            "conflict": null,
            "conflict_reason": null,
            "changed_paths": [],
            "error": null,
            "changed_path_kinds": {},
            "mutation_source": "",
            "status": result_status(exit_code, timed_out),
            "exit_code": exit_code,
            "stdout": "",
            "stderr": "",
            "warnings": [],
        }),
    })
}

#[cfg(target_os = "linux")]
fn record_command_scope_wait_timing(
    timings: &mut RunnerPhaseTimings,
    scope_timing: &CommandExecutionScopeTiming,
) {
    if let Some(value) = scope_timing.root_exit_s {
        timings.insert_s("workspace.shell_wait_root_exit_s", value);
    }
    if let Some(value) = scope_timing.post_root_drain_s {
        timings.insert_s("workspace.shell_wait_post_root_drain_s", value);
    }
    timings.insert_s(
        "workspace.shell_wait_child_try_wait_s",
        scope_timing.child_try_wait_s,
    );
    timings.insert_s("workspace.shell_wait_proc_scan_s", scope_timing.proc_scan_s);
    timings.insert_s(
        "workspace.shell_wait_proc_scan_count",
        scope_timing.proc_scan_count as f64,
    );
    timings.insert_s(
        "workspace.shell_wait_poll_count",
        scope_timing.poll_count as f64,
    );
    timings.insert_s(
        "workspace.shell_wait_poll_sleep_s",
        scope_timing.poll_sleep_s,
    );
}

#[cfg(target_os = "linux")]
const fn result_status(exit_code: i32, timed_out: bool) -> &'static str {
    if timed_out {
        "timed_out"
    } else if exit_code == 0 {
        "ok"
    } else {
        "error"
    }
}

#[cfg(target_os = "linux")]
fn write_if_exists(path: impl AsRef<Path>, value: impl AsRef<OsStr>) -> Result<(), RunnerError> {
    match fs::write(path.as_ref(), value.as_ref().as_encoded_bytes()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(RunnerError::Syscall(err)),
    }
}

#[cfg(target_os = "linux")]
fn error_result(exit_code: i32, kind: &str, message: &str) -> RunResult {
    RunResult {
        exit_code,
        payload: serde_json::json!({
            "success": false,
            "workspace": "ephemeral",
            "status": "error",
            "error": {
                "kind": kind,
                "message": message,
            },
            "timings": {},
        }),
    }
}

#[cfg(target_os = "linux")]
fn record_overlay_teardown(
    result: &mut RunResult,
    mount_guard: overlay::OverlayMount,
    layer_count: usize,
) {
    let unmount_start = Instant::now();
    let unmount_result = mount_guard.unmount();
    let unmount_s = unmount_start.elapsed().as_secs_f64();
    let fsconfig_calls = layer_count.saturating_add(3);

    let Some(payload) = result.payload.as_object_mut() else {
        return;
    };
    let timings = payload.entry("timings").or_insert_with(|| json!({}));
    if let Some(timings) = timings.as_object_mut() {
        timings.insert("workspace.unmount_s".to_owned(), json!(unmount_s));
        timings.insert("workspace.layer_count".to_owned(), json!(layer_count));
        timings.insert("workspace.fsconfig_calls".to_owned(), json!(fsconfig_calls));
    }
    match unmount_result {
        Ok(()) => {
            payload.insert("workspace_unmount_error".to_owned(), Value::Null);
        }
        Err(err) => {
            let message = err.to_string();
            payload.insert("workspace_unmount_error".to_owned(), json!(message));
            let warnings = payload.entry("warnings").or_insert_with(|| json!([]));
            if let Some(warnings) = warnings.as_array_mut() {
                warnings.push(json!({
                    "kind": "workspace_unmount_failed",
                    "message": message,
                }));
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::plugin_mask_paths;

    #[test]
    fn plugin_mask_keeps_eos_runtime_available_for_remount() {
        let paths = plugin_mask_paths(Some(&[
            PathBuf::from("/eos"),
            PathBuf::from("/proc"),
            PathBuf::from("/sys/fs/cgroup"),
        ]));

        assert!(
            !paths.iter().any(|path| path == Path::new("/eos")),
            "plugin setup/service paths under /eos must remain available"
        );
        assert!(paths.iter().any(|path| path == Path::new("/proc")));
        assert!(
            !paths
                .iter()
                .any(|path| path == Path::new("/eos/runtime/daemon")),
            "plugin remount needs the daemon runtime directory visible"
        );
        assert!(
            !paths
                .iter()
                .any(|path| path == Path::new("/eos/runtime/daemon/eosd")),
            "plugin remount needs the daemon binary visible inside the service mount namespace"
        );
    }
}

#[cfg(target_os = "linux")]
trait SyscallResult<T> {
    fn map_syscall(self) -> Result<T, RunnerError>;
}

#[cfg(target_os = "linux")]
impl<T> SyscallResult<T> for rustix::io::Result<T> {
    fn map_syscall(self) -> Result<T, RunnerError> {
        self.map_err(|err| RunnerError::Syscall(std::io::Error::from(err)))
    }
}
