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
use std::fs::{self, File};
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
use rustix::process::{kill_process_group, setsid, Pid, Signal};
#[cfg(target_os = "linux")]
use rustix::thread::{set_thread_gid, set_thread_uid, unshare, UnshareFlags};

use crate::error::RunnerError;
use crate::mount::KernelMountPort;
#[cfg(target_os = "linux")]
use crate::mount::MountInputs;
use crate::request::{RunRequest, RunResult};

#[cfg(target_os = "linux")]
const CHILD_WAIT_POLL: Duration = Duration::from_millis(5);

/// Run one tool call in a freshly-unshared namespace.
///
/// # Safety (future)
///
/// Will call `setsid(2)` and `unshare(2)`, then spawn a child in the new
/// namespace. The namespace syscalls require the process to be single-threaded
/// (the crate-level invariant) and the caller to own the namespace it creates.
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
    let output_dir = upperdir
        .parent()
        .ok_or_else(|| {
            RunnerError::InvalidRequest("fresh-ns upperdir must have a parent".to_owned())
        })?
        .to_path_buf();

    let mount_start = Instant::now();
    let _mount_guard = mount.mount_overlay(&MountInputs {
        workspace_root: request.workspace_root.0.clone(),
        layer_paths: request.layer_paths.clone(),
        upperdir: upperdir.clone(),
        workdir: workdir.clone(),
    })?;
    let mount_s = mount_start.elapsed().as_secs_f64();

    execute_tool(request, mount_s, output_dir, Instant::now())
}

#[cfg(not(target_os = "linux"))]
pub fn run_fresh_ns(
    _request: &RunRequest,
    _mount: &dyn KernelMountPort,
) -> Result<RunResult, RunnerError> {
    Err(RunnerError::Unsupported)
}

#[cfg(target_os = "linux")]
fn enter_fresh_namespace() -> Result<(), RunnerError> {
    let host_uid = rustix::process::getuid().as_raw();
    let host_gid = rustix::process::getgid().as_raw();

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
    fs::write("/proc/self/uid_map", format!("0 {host_uid} 1\n")).map_err(RunnerError::Syscall)?;
    fs::write("/proc/self/gid_map", format!("0 {host_gid} 1\n")).map_err(RunnerError::Syscall)?;
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
fn execute_tool(
    request: &RunRequest,
    mount_s: f64,
    output_dir: PathBuf,
    run_start: Instant,
) -> Result<RunResult, RunnerError> {
    if request.tool_call.verb != "shell" {
        return Ok(error_result(
            2,
            "unsupported_runner_verb",
            format!(
                "fresh namespace runner currently supports shell only; got {}",
                request.tool_call.verb
            ),
        ));
    }

    let argv = shell_argv(&request.tool_call.args)?;
    let cwd = shell_cwd(request)?;
    fs::create_dir_all(&output_dir).map_err(RunnerError::Child)?;
    let stdout_path = output_dir.join(format!("{}.stdout", request.tool_call.invocation_id));
    let stderr_path = output_dir.join(format!("{}.stderr", request.tool_call.invocation_id));
    let stdout_file = File::create(&stdout_path).map_err(RunnerError::Child)?;
    let stderr_file = File::create(&stderr_path).map_err(RunnerError::Child)?;

    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&cwd)
        .env_clear()
        .envs(command_environment(&request.tool_call.args))
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .process_group(0);

    let mut child = command.spawn().map_err(RunnerError::Child)?;
    let exit_code = wait_for_child(&mut child, request.timeout_seconds)?;
    let stdout = fs::read_to_string(&stdout_path).unwrap_or_else(|_| String::new());
    let stderr = fs::read_to_string(&stderr_path).unwrap_or_else(|_| String::new());
    let tool_s = run_start.elapsed().as_secs_f64();

    let status = if exit_code == 0 { "ok" } else { "error" };
    Ok(RunResult {
        exit_code,
        tool_result: serde_json::json!({
            "success": true,
            "workspace": "ephemeral",
            "timings": {
                "workspace.mount_s": mount_s,
                "workspace.tool_s": tool_s,
            },
            "conflict": null,
            "conflict_reason": null,
            "changed_paths": [],
            "error": null,
            "changed_path_kinds": {},
            "mutation_source": "",
            "status": status,
            "exit_code": exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "warnings": [],
        }),
    })
}

#[cfg(target_os = "linux")]
fn wait_for_child(
    child: &mut std::process::Child,
    timeout_seconds: Option<f64>,
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
            let pid = Pid::from_child(child);
            let _ = kill_process_group(pid, Signal::Kill);
            let _ = child.wait();
            return Err(RunnerError::TimedOut);
        }
        thread::sleep(CHILD_WAIT_POLL);
    }
}

#[cfg(target_os = "linux")]
fn shell_argv(args: &serde_json::Value) -> Result<Vec<String>, RunnerError> {
    let Some(command) = args.get("command") else {
        return Err(RunnerError::InvalidRequest(
            "shell args require command".to_owned(),
        ));
    };
    if let Some(command) = command.as_str() {
        return Ok(vec![
            "bash".to_owned(),
            "-lc".to_owned(),
            command.to_owned(),
        ]);
    }
    if let Some(parts) = command.as_array() {
        let argv: Vec<String> = parts
            .iter()
            .map(|part| {
                part.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| part.to_string())
            })
            .collect();
        if argv.is_empty() {
            return Err(RunnerError::InvalidRequest(
                "shell command argv must not be empty".to_owned(),
            ));
        }
        return Ok(argv);
    }
    Err(RunnerError::InvalidRequest(
        "shell command must be a string or argv list".to_owned(),
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
    if let Some(extra) = args.get("env").and_then(serde_json::Value::as_object) {
        for (key, value) in extra {
            if !RESTRICTED.contains(&key.as_str()) {
                env.insert(
                    key.to_owned(),
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
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
fn error_result(exit_code: i32, kind: &str, message: String) -> RunResult {
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
    use super::{normalize_lexical, shell_argv};
    use std::path::Path;

    #[test]
    fn shell_string_uses_bash_lc() {
        let argv =
            shell_argv(&serde_json::json!({"command": "echo hi"})).expect("valid shell argv");
        assert_eq!(argv, vec!["bash", "-lc", "echo hi"]);
    }

    #[test]
    fn normalizes_paths_without_touching_fs() {
        assert_eq!(
            normalize_lexical(Path::new("/testbed/./a/../b")),
            Path::new("/testbed/b")
        );
    }
}
