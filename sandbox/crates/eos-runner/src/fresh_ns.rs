//! Fresh-namespace mode: `unshare` → `uid_map` → mount overlay → spawn tool.
//!
//! This is the daemon's standard per-tool-call path. The Python target spawns
//! `unshare -Urm python -m sandbox.overlay.namespace_entrypoint <payload>` with
//! `start_new_session=True`; the Rust port does the `unshare(CLONE_NEWUSER|
//! CLONE_NEWNS)` itself in this single-threaded child, writes the uid/gid maps,
//! delegates the overlay mount to [`KernelMountPort`], then spawns the tool in
//! its own process group so timeout cleanup can kill the whole tree.
//!
//! The syscall boundary is kept behind safe `rustix` wrappers here. If this
//! later needs a raw libc gap, the block must carry a focused `// SAFETY:` note
//! and the crate's `#![deny(unsafe_op_in_unsafe_fn)]` forces that discipline.

#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::process::{CommandExt, ExitStatusExt};
#[cfg(target_os = "linux")]
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use rustix::io::Errno;
#[cfg(target_os = "linux")]
use rustix::mount::{mount_change, MountPropagationFlags};
#[cfg(target_os = "linux")]
use rustix::process::{getpgrp, kill_process_group, setsid, Pid, Signal};
#[cfg(target_os = "linux")]
use rustix::thread::{set_thread_gid, set_thread_uid, unshare, UnshareFlags};

use crate::error::RunnerError;
use crate::mount::KernelMountPort;
#[cfg(target_os = "linux")]
use crate::mount::MountInputs;
use crate::request::{RunRequest, RunResult};
#[cfg(target_os = "linux")]
use crate::tool_primitives::{glob_tool_result, grep_tool_result};

#[cfg(target_os = "linux")]
const CHILD_WAIT_POLL: Duration = Duration::from_millis(5);

/// Run one tool call in a freshly-unshared namespace.
///
/// # Safety (future)
///
/// Will call `setsid(2)` and `unshare(2)`, then spawn a child in the new
/// namespace. The namespace syscalls require the process to be single-threaded
/// (the crate-level invariant) and the caller to own the namespace it creates.
///
/// # Errors
///
/// Returns [`RunnerError`] when namespace setup, overlay mounting, request
/// validation, or child execution fails.
// PORT backend/src/sandbox/overlay/namespace_runner.py:72 — _run_tool_call_in_fresh_namespace
// PORT backend/src/sandbox/overlay/namespace_runner.py:227-250 — _run_namespace_entrypoint_async (unshare -Urm, start_new_session=True)
// PORT backend/src/sandbox/overlay/namespace_entrypoint.py:92-135 — mount_and_execute_tool_payload (mount overlay then exec)
#[cfg(target_os = "linux")]
pub fn run_fresh_ns(
    request: &RunRequest,
    mount: &dyn KernelMountPort,
) -> Result<RunResult, RunnerError> {
    // PORT backend/src/sandbox/overlay/namespace_runner.py:72-135 — full fresh-ns
    //   sequence: unshare(CLONE_NEWUSER|CLONE_NEWNS) on this single-threaded child,
    //   write /proc/self/{uid_map,setgroups,gid_map}, KernelMountPort::mount_overlay
    //   at workspace_root, setsid + spawn the tool, then build the result JSON
    //   and reap the process group on timeout.
    enter_fresh_namespace()?;
    let upperdir = request
        .upperdir
        .as_ref()
        .ok_or_else(|| RunnerError::InvalidRequest("fresh-ns requires upperdir".to_owned()))?;
    let workdir = request
        .workdir
        .as_ref()
        .ok_or_else(|| RunnerError::InvalidRequest("fresh-ns requires workdir".to_owned()))?;
    let mount_start = Instant::now();
    let _mount_guard = mount.mount_overlay(&MountInputs {
        workspace_root: request.workspace_root.0.clone(),
        layer_paths: request.layer_paths.clone(),
        upperdir: upperdir.clone(),
        workdir: workdir.clone(),
    })?;
    let mount_s = mount_start.elapsed().as_secs_f64();

    execute_tool(request, mount_s, Instant::now())
}

#[cfg(not(target_os = "linux"))]
/// Return the non-Linux unsupported error for fresh-namespace execution.
///
/// # Errors
///
/// Always returns [`RunnerError::Unsupported`] outside Linux because the
/// namespace syscalls do not exist.
pub fn run_fresh_ns(
    _request: &RunRequest,
    _mount: &dyn KernelMountPort,
) -> Result<RunResult, RunnerError> {
    Err(RunnerError::Unsupported)
}

#[cfg(target_os = "linux")]
fn enter_fresh_namespace() -> Result<(), RunnerError> {
    struct ParentIds {
        user: u32,
        group: u32,
    }

    let parent_ids = ParentIds {
        user: rustix::process::getuid().as_raw(),
        group: rustix::process::getgid().as_raw(),
    };

    if let Err(err) = setsid() {
        // Docker exec may launch the runner as a process-group leader. In that
        // case setsid(2) returns EPERM, but the spawned tool below still gets
        // its own process group for timeout/cancel cleanup.
        if err != Errno::PERM {
            return Err(RunnerError::Syscall(std::io::Error::from(err)));
        }
    }
    unshare(UnshareFlags::NEWUSER | UnshareFlags::NEWNS).map_syscall()?;
    write_if_exists("/proc/self/setgroups", "deny\n")?;
    fs::write("/proc/self/uid_map", format!("0 {} 1\n", parent_ids.user))
        .map_err(RunnerError::Syscall)?;
    fs::write("/proc/self/gid_map", format!("0 {} 1\n", parent_ids.group))
        .map_err(RunnerError::Syscall)?;
    set_thread_gid(rustix::process::Gid::ROOT).map_syscall()?;
    set_thread_uid(rustix::process::Uid::ROOT).map_syscall()?;
    mount_change(
        "/",
        MountPropagationFlags::PRIVATE | MountPropagationFlags::REC,
    )
    .map_syscall()?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn execute_tool(
    request: &RunRequest,
    mount_s: f64,
    run_start: Instant,
) -> Result<RunResult, RunnerError> {
    match request.tool_call.verb.as_str() {
        "exec_command" => execute_shell(request, mount_s, run_start),
        "plugin_service" => execute_plugin_service(request, mount_s, run_start),
        "glob" => Ok(RunResult {
            exit_code: 0,
            tool_result: glob_tool_result(
                &request.tool_call.args,
                &request.workspace_root.0,
                mount_s,
                run_start.elapsed().as_secs_f64(),
            )?,
        }),
        "grep" => Ok(RunResult {
            exit_code: 0,
            tool_result: grep_tool_result(
                &request.tool_call.args,
                &request.workspace_root.0,
                mount_s,
                run_start.elapsed().as_secs_f64(),
            )?,
        }),
        _ => Ok(error_result(
            2,
            "unsupported_runner_verb",
            &format!(
                "fresh namespace runner does not support verb {}",
                request.tool_call.verb
            ),
        )),
    }
}

#[cfg(target_os = "linux")]
fn execute_plugin_service(
    request: &RunRequest,
    mount_s: f64,
    run_start: Instant,
) -> Result<RunResult, RunnerError> {
    let argv = plugin_service_argv(request)?;
    let cwd = shell_cwd(request)?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .envs(command_environment(&request.tool_call.args))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);

    let mut child = command.spawn().map_err(RunnerError::Child)?;
    let child_pid = Pid::from_child(&child);
    let (exit_code, timed_out) = match wait_for_child(
        &mut child,
        request.timeout_seconds,
        TimeoutKill::ProcessGroup,
    ) {
        Ok(exit_code) => (exit_code, false),
        Err(RunnerError::TimedOut) => (124, true),
        Err(err) => return Err(err),
    };
    if timed_out || !matches!(request.mode, crate::request::RunMode::SetNs) {
        let _ = kill_process_group(child_pid, Signal::Kill);
    }
    let status = if timed_out {
        "timed_out"
    } else if exit_code == 0 {
        "ok"
    } else {
        "error"
    };
    Ok(RunResult {
        exit_code,
        tool_result: serde_json::json!({
            "success": exit_code == 0,
            "workspace": "ephemeral",
            "timings": {
                "workspace.mount_s": mount_s,
                "workspace.tool_s": run_start.elapsed().as_secs_f64(),
            },
            "status": status,
        }),
    })
}

#[cfg(target_os = "linux")]
fn execute_shell(
    request: &RunRequest,
    mount_s: f64,
    run_start: Instant,
) -> Result<RunResult, RunnerError> {
    let argv = shell_argv(request)?;
    let cwd = shell_cwd(request)?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(command_environment(&request.tool_call.args))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = command.spawn().map_err(RunnerError::Child)?;
    let (exit_code, timed_out) =
        match wait_for_command_execution_scope(&mut child, request.timeout_seconds) {
            Ok(exit_code) => (exit_code, false),
            Err(RunnerError::TimedOut) => (124, true),
            Err(err) => return Err(err),
        };
    let status = if timed_out {
        "timed_out"
    } else if exit_code == 0 {
        "ok"
    } else {
        "error"
    };
    Ok(RunResult {
        exit_code,
        tool_result: serde_json::json!({
            "success": exit_code == 0,
            "workspace": "ephemeral",
            "timings": {
                "workspace.mount_s": mount_s,
                "workspace.tool_s": run_start.elapsed().as_secs_f64(),
            },
            "conflict": null,
            "conflict_reason": null,
            "changed_paths": [],
            "error": null,
            "changed_path_kinds": {},
            "mutation_source": "",
            "status": status,
            "exit_code": exit_code,
            "stdout": "",
            "stderr": "",
            "warnings": [],
        }),
    })
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum TimeoutKill {
    ProcessGroup,
}

#[cfg(target_os = "linux")]
fn wait_for_child(
    child: &mut std::process::Child,
    timeout_seconds: Option<f64>,
    timeout_kill: TimeoutKill,
) -> Result<i32, RunnerError> {
    let deadline = timeout_seconds
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| Instant::now() + Duration::from_secs_f64(seconds));
    loop {
        if let Some(status) = child.try_wait().map_err(RunnerError::Child)? {
            return Ok(status
                .code()
                .or_else(|| status.signal().map(|sig| -sig))
                .unwrap_or(128));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            match timeout_kill {
                TimeoutKill::ProcessGroup => {
                    let pid = Pid::from_child(child);
                    let _ = kill_process_group(pid, Signal::Kill);
                }
            }
            let _ = child.wait();
            return Err(RunnerError::TimedOut);
        }
        thread::sleep(CHILD_WAIT_POLL);
    }
}

#[cfg(target_os = "linux")]
fn wait_for_command_execution_scope(
    child: &mut std::process::Child,
    timeout_seconds: Option<f64>,
) -> Result<i32, RunnerError> {
    let deadline = timeout_seconds
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| Instant::now() + Duration::from_secs_f64(seconds));
    let pgid = getpgrp().as_raw_nonzero().get();
    let self_pid = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    let mut root_exit_code = None;
    loop {
        if root_exit_code.is_none() {
            if let Some(status) = child.try_wait().map_err(RunnerError::Child)? {
                root_exit_code = Some(
                    status
                        .code()
                        .or_else(|| status.signal().map(|sig| -sig))
                        .unwrap_or(128),
                );
            }
        }
        if root_exit_code.is_some() && !process_group_has_other_live_members(pgid, self_pid) {
            return Ok(root_exit_code.unwrap_or(0));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if let Some(pid) = Pid::from_raw(pgid) {
                let _ = kill_process_group(pid, Signal::Kill);
            }
            let _ = child.wait();
            return Err(RunnerError::TimedOut);
        }
        thread::sleep(CHILD_WAIT_POLL);
    }
}

#[cfg(target_os = "linux")]
fn process_group_has_other_live_members(pgid: i32, self_pid: i32) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            return false;
        };
        if pid == self_pid {
            return false;
        }
        proc_stat_process_group(pid)
            .is_some_and(|(entry_pgid, state)| entry_pgid == pgid && state != 'Z')
    })
}

#[cfg(target_os = "linux")]
fn proc_stat_process_group(pid: i32) -> Option<(i32, char)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(") ")?;
    let fields: Vec<&str> = stat[close + 2..].split_whitespace().collect();
    let state = fields.first()?.chars().next()?;
    let pgrp = fields.get(2)?.parse::<i32>().ok()?;
    Some((pgrp, state))
}

#[cfg(target_os = "linux")]
fn plugin_service_argv(request: &RunRequest) -> Result<Vec<String>, RunnerError> {
    let Some(command) = request.tool_call.args.get("command") else {
        return Err(RunnerError::InvalidRequest(
            "plugin_service requires command argv".to_owned(),
        ));
    };
    let parts = command.as_array().ok_or_else(|| {
        RunnerError::InvalidRequest("plugin_service command must be an argv list".to_owned())
    })?;
    if parts.is_empty() {
        return Err(RunnerError::InvalidRequest(
            "plugin_service command argv must not be empty".to_owned(),
        ));
    }
    let argv: Result<Vec<String>, RunnerError> = parts
        .iter()
        .map(|part| {
            part.as_str().map_or_else(
                || {
                    Err(RunnerError::InvalidRequest(
                        "plugin_service command argv entries must be strings".to_owned(),
                    ))
                },
                |value| Ok(value.to_owned()),
            )
        })
        .collect();
    let argv = argv?;
    if argv[0].trim().is_empty() {
        return Err(RunnerError::InvalidRequest(
            "plugin_service command argv[0] must not be empty".to_owned(),
        ));
    }
    Ok(argv)
}

#[cfg(target_os = "linux")]
fn shell_argv(request: &RunRequest) -> Result<Vec<String>, RunnerError> {
    let shell_args = &request.tool_call.args;
    let Some(command) = shell_args.get("command") else {
        return Err(RunnerError::InvalidRequest(
            "shell args require command".to_owned(),
        ));
    };
    if let Some(value) = command.as_str() {
        let command = value.trim();
        if command.is_empty() {
            return Err(RunnerError::InvalidRequest(
                "shell command string must not be empty".to_owned(),
            ));
        }
        return Ok(vec![
            "/bin/bash".to_owned(),
            "--noprofile".to_owned(),
            "--norc".to_owned(),
            "-c".to_owned(),
            value.to_owned(),
        ]);
    }
    Err(RunnerError::InvalidRequest(
        "exec_command requires a shell-format command string".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn shell_cwd(request: &RunRequest) -> Result<PathBuf, RunnerError> {
    let raw = request
        .tool_call
        .args
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(".");
    let workspace_root = normalize_lexical(&request.workspace_root.0);
    let candidate = PathBuf::from(raw);
    let resolved = if candidate.is_absolute() {
        let candidate = normalize_lexical(&candidate);
        let rel = candidate.strip_prefix(&workspace_root).map_err(|_| {
            RunnerError::InvalidRequest(format!("cwd escapes workspace replacement root: {raw}"))
        })?;
        workspace_root.join(rel)
    } else {
        normalize_lexical(&workspace_root.join(candidate))
    };
    if !resolved.starts_with(&workspace_root) {
        return Err(RunnerError::InvalidRequest(format!(
            "cwd escapes workspace replacement root: {raw}"
        )));
    }
    fs::create_dir_all(&resolved).map_err(RunnerError::Child)?;
    Ok(resolved)
}

#[cfg(target_os = "linux")]
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(target_os = "linux")]
fn command_environment(args: &serde_json::Value) -> BTreeMap<String, String> {
    const HOST_KEYS: &[&str] = &["PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM", "TZ"];
    const RESTRICTED: &[&str] = &[
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "LD_AUDIT",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
        "PATH",
        "PYTHONPATH",
        "BASH_ENV",
        "ENV",
    ];

    let mut env = BTreeMap::new();
    for key in HOST_KEYS {
        if let Ok(value) = std::env::var(key) {
            env.insert((*key).to_owned(), value);
        }
    }
    if !env.contains_key("PATH") {
        env.insert(
            "PATH".to_owned(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
        );
    }
    let existing_path = env.get("PATH").cloned().unwrap_or_default();
    let suffix = if existing_path.is_empty() {
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned()
    } else {
        existing_path
    };
    env.insert(
        "PATH".to_owned(),
        format!("/opt/miniconda3/envs/testbed/bin:/opt/miniconda3/bin:{suffix}"),
    );
    if let Some(extra) = args.get("env").and_then(serde_json::Value::as_object) {
        for (key, value) in extra {
            if !RESTRICTED.contains(&key.as_str()) {
                env.insert(
                    key.to_owned(),
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned),
                );
            }
        }
    }
    env.insert("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned());
    env
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
        tool_result: serde_json::json!({
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
trait SyscallResult<T> {
    fn map_syscall(self) -> Result<T, RunnerError>;
}

#[cfg(target_os = "linux")]
impl<T> SyscallResult<T> for rustix::io::Result<T> {
    fn map_syscall(self) -> Result<T, RunnerError> {
        self.map_err(|err| RunnerError::Syscall(std::io::Error::from(err)))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{normalize_lexical, plugin_service_argv, shell_argv};
    use crate::request::{RunMode, RunRequest, ToolCall, WorkspaceRoot};
    use eos_protocol::Intent;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn exec_command_string_uses_non_login_bash() -> TestResult {
        let argv = shell_argv(&request(
            "exec_command",
            serde_json::json!({"command": "echo hi"}),
        ))?;
        assert_eq!(
            argv,
            ["/bin/bash", "--noprofile", "--norc", "-c", "echo hi"]
                .map(str::to_owned)
                .to_vec()
        );
        Ok(())
    }

    #[test]
    fn exec_command_rejects_raw_argv() -> TestResult {
        let error = match shell_argv(&request(
            "exec_command",
            serde_json::json!({"command": ["echo", "hi"]}),
        )) {
            Ok(argv) => {
                return Err(format!("exec_command raw argv unexpectedly accepted: {argv:?}").into())
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("shell-format command string"));
        Ok(())
    }

    #[test]
    fn plugin_service_requires_argv_command() -> TestResult {
        let argv = plugin_service_argv(&request(
            "plugin_service",
            serde_json::json!({"command": ["python3", "/eos/plugin/harness.py"]}),
        ))?;
        assert_eq!(
            argv,
            ["python3", "/eos/plugin/harness.py"]
                .map(str::to_owned)
                .to_vec()
        );

        let error = match plugin_service_argv(&request(
            "plugin_service",
            serde_json::json!({"command": "python3 /eos/plugin/harness.py"}),
        )) {
            Ok(argv) => {
                return Err(format!(
                    "plugin_service string command unexpectedly accepted: {argv:?}"
                )
                .into());
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("argv list"));
        Ok(())
    }

    #[test]
    fn normalizes_paths_without_touching_fs() {
        assert_eq!(
            normalize_lexical(Path::new("/testbed/./a/../b")),
            Path::new("/testbed/b")
        );
    }

    fn request(verb: &str, args: serde_json::Value) -> RunRequest {
        RunRequest {
            mode: RunMode::FreshNs,
            tool_call: ToolCall {
                invocation_id: "test".to_owned(),
                agent_id: "agent".to_owned(),
                verb: verb.to_owned(),
                intent: Intent::WriteAllowed,
                args,
                background: false,
            },
            workspace_root: WorkspaceRoot(Path::new("/testbed").to_path_buf()),
            layer_paths: vec![],
            upperdir: None,
            workdir: None,
            ns_fds: None,
            cgroup_path: None,
            timeout_seconds: None,
        }
    }
}
