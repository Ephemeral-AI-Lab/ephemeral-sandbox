//! Command and PTY operations for the daemon dispatcher.

#[cfg(target_os = "linux")]
use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::process::{CommandExt, ExitStatusExt};
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::Duration;
use std::time::Instant;

#[cfg(target_os = "linux")]
use nix::pty::openpty;
#[cfg(target_os = "linux")]
use nix::sys::signal::{killpg, Signal};
#[cfg(target_os = "linux")]
use nix::unistd::Pid;
use serde_json::{json, Value};

#[cfg(target_os = "linux")]
use eos_layerstack::{require_workspace_binding, LayerStack, Lease, WorkspaceBinding};
#[cfg(target_os = "linux")]
use eos_overlay::{allocate_overlay_writable_dirs, capture_upperdir, overlay_writable_root};
#[cfg(target_os = "linux")]
use eos_protocol::Intent;
#[cfg(target_os = "linux")]
use eos_runner::{Fd, NsFds, RunMode, RunRequest, RunResult, ToolCall, WorkspaceRoot};

#[cfg(target_os = "linux")]
use crate::dispatcher::{
    apply_occ_changeset, base_hashes_for_snapshot, guarded_changeset_response,
    insert_occ_route_timings, layer_change_kind, manifest_version_u64, merge_runner_timings,
    occ_route_metrics, overlay_daemon_error, resource_timings, run_ns_runner_child,
};
use crate::dispatcher::{run_shell_overlay, u64_to_f64_saturating, DispatchContext};
use crate::error::DaemonError;

/// `api.v1.exec_command` — final Phase 3T command contract.
pub fn op_exec_command(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let cmd = require_command_string(args, "cmd")?;
    let tty = args.get("tty").and_then(Value::as_bool).unwrap_or(false);
    let timeout_seconds = optional_u64(args, "timeout")
        .or_else(|| optional_u64(args, "timeout_seconds"))
        .map(u64_to_f64_saturating);
    #[cfg(not(target_os = "linux"))]
    if crate::isolated::agent_has_active_handle(agent_id_arg(args)) {
        return Ok(command_result(
            "error",
            None,
            "",
            "isolated exec_command is only supported on linux",
            None,
        ));
    }
    #[cfg(target_os = "linux")]
    if let Some(handle) = crate::isolated::command_handle_for_args(args) {
        if tty {
            let yield_time_ms = optional_u64(args, "yield_time_ms").unwrap_or(1000);
            return start_isolated_pty_command(args, &cmd, timeout_seconds, yield_time_ms, handle);
        }
        return run_isolated_command(args, &cmd, timeout_seconds, &handle, Instant::now());
    }
    if !tty {
        let mut shell_args = args.clone();
        shell_args["command"] = json!(cmd);
        shell_args["cwd"] = json!(".");
        if let Some(timeout) = timeout_seconds {
            shell_args["timeout_seconds"] = json!(timeout);
        }
        let shell = run_shell_overlay(&shell_args, Instant::now(), None)?;
        return Ok(command_response_from_shell(&shell, None));
    }

    #[cfg(target_os = "linux")]
    {
        let yield_time_ms = optional_u64(args, "yield_time_ms").unwrap_or(1000);
        start_pty_command(args, &cmd, timeout_seconds, yield_time_ms)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = timeout_seconds;
        Ok(command_result(
            "error",
            None,
            "",
            "pty commands are only supported on linux",
            None,
        ))
    }
}

// Dispatcher op handlers share the `Result<Value, DaemonError>` ABI even when
// a specific op encodes all domain failures in its JSON response.
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "dispatcher handlers share a fallible ABI"
    )
)]
pub fn op_pty_write_stdin(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        pty_write_stdin(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(pty_not_found())
    }
}

#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "dispatcher handlers share a fallible ABI"
    )
)]
pub fn op_pty_progress(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        pty_progress(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(pty_not_found())
    }
}

#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "dispatcher handlers share a fallible ABI"
    )
)]
pub fn op_pty_cancel(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        pty_cancel(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(pty_not_found())
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_pty_collect_completed(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        Ok(pty_registry().collect_completed(args))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(json!({"success": true, "completions": []}))
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_pty_session_count(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    #[cfg(target_os = "linux")]
    {
        let count = pty_registry().count_by_agent(&agent_id);
        Ok(json!({"success": true, "agent_id": agent_id, "count": count}))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(json!({"success": true, "agent_id": agent_id, "count": 0}))
    }
}

fn require_command_string(args: &Value, key: &str) -> Result<String, DaemonError> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| DaemonError::InvalidEnvelope(format!("{key} is required")))?;
    if value.trim().is_empty() {
        return Err(DaemonError::InvalidEnvelope(format!(
            "{key} must be non-empty"
        )));
    }
    Ok(value.to_owned())
}

#[cfg(not(target_os = "linux"))]
fn agent_id_arg(args: &Value) -> &str {
    args.get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
}

#[cfg(target_os = "linux")]
fn require_string(args: &Value, key: &str) -> Result<String, DaemonError> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if value.is_empty() {
        return Err(DaemonError::InvalidEnvelope(format!("{key} is required")));
    }
    Ok(value)
}

fn optional_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

#[cfg(target_os = "linux")]
fn sanitize_path_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "op".to_owned()
    } else {
        cleaned
    }
}

fn command_response_from_shell(shell: &Value, pty_session_id: Option<String>) -> Value {
    let status = shell
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("error");
    let exit_code = shell.get("exit_code").and_then(Value::as_i64);
    let stdout = shell.get("stdout").and_then(Value::as_str).unwrap_or("");
    let stderr = shell.get("stderr").and_then(Value::as_str).unwrap_or("");
    let mut response = command_result(status, exit_code, stdout, stderr, pty_session_id);
    for key in [
        "success",
        "workspace",
        "timings",
        "conflict",
        "conflict_reason",
        "changed_paths",
        "changed_path_kinds",
        "mutation_source",
        "error",
        "warnings",
    ] {
        if let Some(value) = shell.get(key) {
            response[key] = value.clone();
        }
    }
    response
}

fn command_result(
    status: &str,
    exit_code: Option<i64>,
    stdout: &str,
    stderr: &str,
    pty_session_id: Option<String>,
) -> Value {
    let mut response = json!({
        "status": status,
        "exit_code": exit_code,
        "output": {
            "stdout": stdout,
            "stderr": stderr,
        },
    });
    if let Some(pty_session_id) = pty_session_id {
        response["pty_session_id"] = json!(pty_session_id);
    }
    response
}

fn pty_not_found() -> Value {
    command_result("error", None, "", "pty_session_not_found", None)
}

#[cfg(any(target_os = "linux", test))]
const fn should_publish_pty_completion(
    publish_completion: bool,
    cancelled: bool,
    finalizer_owned_live_session: bool,
) -> bool {
    publish_completion && !cancelled && finalizer_owned_live_session
}

#[cfg(target_os = "linux")]
const PTY_RING_MAX_BYTES: usize = 1024 * 1024;
#[cfg(target_os = "linux")]
const PTY_SPOOL_MAX_BYTES: u64 = 32 * 1024 * 1024;

#[cfg(target_os = "linux")]
fn lock_pty_state<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(target_os = "linux")]
struct PtyOutput {
    chunks: Mutex<VecDeque<PtyChunk>>,
    bytes: Mutex<usize>,
    spool_bytes: Mutex<u64>,
    spool_truncated: Mutex<bool>,
}

#[cfg(target_os = "linux")]
struct PtyChunk {
    at: Instant,
    text: String,
}

#[cfg(target_os = "linux")]
impl PtyOutput {
    const fn new() -> Self {
        Self {
            chunks: Mutex::new(VecDeque::new()),
            bytes: Mutex::new(0),
            spool_bytes: Mutex::new(0),
            spool_truncated: Mutex::new(false),
        }
    }

    fn append(&self, text: String) {
        let byte_len = text.len();
        let mut chunks = lock_pty_state(&self.chunks);
        let mut bytes = lock_pty_state(&self.bytes);
        chunks.push_back(PtyChunk {
            at: Instant::now(),
            text,
        });
        *bytes += byte_len;
        while *bytes > PTY_RING_MAX_BYTES {
            let Some(chunk) = chunks.pop_front() else {
                break;
            };
            *bytes = bytes.saturating_sub(chunk.text.len());
        }
        drop(bytes);
        drop(chunks);
    }

    fn recent_since(&self, since: Instant, max_tokens: Option<u64>) -> String {
        let chunks = lock_pty_state(&self.chunks);
        bounded_output(
            chunks
                .iter()
                .filter(|chunk| chunk.at >= since)
                .map(|chunk| chunk.text.as_str()),
            max_tokens,
        )
    }

    fn all_recent(&self, max_tokens: Option<u64>) -> String {
        let chunks = lock_pty_state(&self.chunks);
        bounded_output(chunks.iter().map(|chunk| chunk.text.as_str()), max_tokens)
    }

    fn note_spooled(&self, bytes: u64) -> bool {
        let mut spool_bytes = lock_pty_state(&self.spool_bytes);
        if *spool_bytes >= PTY_SPOOL_MAX_BYTES {
            *lock_pty_state(&self.spool_truncated) = true;
            return false;
        }
        *spool_bytes = (*spool_bytes + bytes).min(PTY_SPOOL_MAX_BYTES);
        true
    }

    fn spool_truncated(&self) -> bool {
        *lock_pty_state(&self.spool_truncated)
    }
}

#[cfg(target_os = "linux")]
fn bounded_output<'a>(chunks: impl Iterator<Item = &'a str>, max_tokens: Option<u64>) -> String {
    let max_chars = max_tokens
        .and_then(|tokens| usize::try_from(tokens.saturating_mul(4)).ok())
        .filter(|value| *value > 0)
        .unwrap_or(80_000);
    let mut out = String::new();
    for chunk in chunks {
        out.push_str(chunk);
        if out.len() > max_chars {
            let keep_from = out.len().saturating_sub(max_chars);
            out = out[keep_from..].to_owned();
        }
    }
    out
}

#[cfg(target_os = "linux")]
struct PtySession {
    id: String,
    agent_id: String,
    command: String,
    pgid: i32,
    writer: Mutex<File>,
    output: Arc<PtyOutput>,
    cancelled: Mutex<bool>,
}

#[cfg(target_os = "linux")]
struct PtyRegistry {
    sessions: Mutex<HashMap<String, Arc<PtySession>>>,
    completed: Mutex<Vec<Value>>,
    counter: AtomicU64,
}

#[cfg(target_os = "linux")]
impl PtyRegistry {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            completed: Mutex::new(Vec::new()),
            counter: AtomicU64::new(1),
        }
    }

    fn next_id(&self) -> String {
        format!("pty_{}", self.counter.fetch_add(1, Ordering::Relaxed))
    }

    fn insert(&self, session: Arc<PtySession>) {
        lock_pty_state(&self.sessions).insert(session.id.clone(), session);
    }

    fn get(&self, id: &str) -> Option<Arc<PtySession>> {
        lock_pty_state(&self.sessions).get(id).cloned()
    }

    fn remove(&self, id: &str) -> Option<Arc<PtySession>> {
        lock_pty_state(&self.sessions).remove(id)
    }

    fn count_by_agent(&self, agent_id: &str) -> usize {
        lock_pty_state(&self.sessions)
            .values()
            .filter(|session| agent_id.is_empty() || session.agent_id == agent_id)
            .count()
    }

    fn push_completed(&self, completion: Value) {
        lock_pty_state(&self.completed).push(completion);
    }

    fn take_completed_result(&self, id: &str) -> Option<Value> {
        let mut completed = lock_pty_state(&self.completed);
        let mut kept = Vec::with_capacity(completed.len());
        let mut found = None;
        for item in completed.drain(..) {
            let item_id = item
                .get("pty_session_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if found.is_none() && item_id == id {
                found = item.get("result").cloned();
            } else {
                kept.push(item);
            }
        }
        *completed = kept;
        found
    }

    fn collect_completed(&self, args: &Value) -> Value {
        let wanted: Option<HashSet<String>> = args
            .get("pty_session_ids")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            });
        let agent_id = args
            .get("agent_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut completed = lock_pty_state(&self.completed);
        let mut kept = Vec::new();
        let mut returned = Vec::new();
        for item in completed.drain(..) {
            let id = item
                .get("pty_session_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let item_agent = item
                .get("agent_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let id_matches = wanted.as_ref().is_none_or(|ids| ids.contains(id));
            let agent_matches = agent_id.is_empty() || agent_id == item_agent;
            if id_matches && agent_matches {
                returned.push(item);
            } else {
                kept.push(item);
            }
        }
        *completed = kept;
        drop(completed);
        json!({"success": true, "completions": returned})
    }
}

#[cfg(target_os = "linux")]
fn pty_registry() -> &'static PtyRegistry {
    static REGISTRY: OnceLock<PtyRegistry> = OnceLock::new();
    REGISTRY.get_or_init(PtyRegistry::new)
}

#[cfg(target_os = "linux")]
struct PtyWorkspace {
    root: PathBuf,
    lease_id: String,
    manifest: eos_layerstack::Manifest,
    manifest_version: i64,
    upperdir: PathBuf,
    run_dir: PathBuf,
    output_path: PathBuf,
    final_path: PathBuf,
}

#[cfg(target_os = "linux")]
struct IsolatedPtyWorkspace {
    handle: crate::isolated::CommandHandle,
    output_path: PathBuf,
    final_path: PathBuf,
}

#[cfg(target_os = "linux")]
struct PtyStartSpec {
    id: String,
    invocation_id: String,
    agent_id: String,
    command: String,
    timeout_seconds: Option<f64>,
}

#[cfg(target_os = "linux")]
fn run_isolated_command(
    args: &Value,
    cmd: &str,
    timeout_seconds: Option<f64>,
    handle: &crate::isolated::CommandHandle,
    total_start: Instant,
) -> Result<Value, DaemonError> {
    let invocation_id = args
        .get("invocation_id")
        .and_then(Value::as_str)
        .unwrap_or("exec_command")
        .to_owned();
    let cwd = args
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or(".")
        .to_owned();
    let ns_fds = runner_ns_fds(&handle.ns_fds);
    let request = RunRequest {
        mode: runner_mode(ns_fds.as_ref()),
        tool_call: ToolCall {
            invocation_id,
            agent_id: handle.agent_id.clone(),
            verb: "exec_command".to_owned(),
            intent: Intent::WriteAllowed,
            args: json!({
                "command": cmd,
                "cwd": cwd,
            }),
            background: false,
        },
        workspace_root: WorkspaceRoot(handle.workspace_root.clone()),
        layer_paths: handle.layer_paths.clone(),
        upperdir: Some(handle.upperdir.clone()),
        workdir: Some(handle.workdir.clone()),
        ns_fds,
        cgroup_path: handle.cgroup_path.clone(),
        timeout_seconds,
    };
    let runner = run_ns_runner_child(&request, None)?;
    isolated_response_from_runner(handle, &runner, total_start, false)
}

#[cfg(target_os = "linux")]
fn isolated_response_from_runner(
    handle: &crate::isolated::CommandHandle,
    runner: &RunResult,
    total_start: Instant,
    include_pty_session_id: bool,
) -> Result<Value, DaemonError> {
    let (path_kinds, capture_s) = capture_isolated_path_kinds(handle)?;
    let changed_paths: Vec<String> = path_kinds.iter().map(|(path, _)| path.clone()).collect();
    let timings =
        isolated_runner_timings(handle, runner, path_kinds.len(), capture_s, total_start)?;
    let status = runner
        .tool_result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or(if runner.exit_code == 0 { "ok" } else { "error" });
    let stdout = runner
        .tool_result
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = runner
        .tool_result
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut response = json!({
        "success": true,
        "workspace": "isolated",
        "workspace_mode": "isolated",
        "status": status,
        "exit_code": runner.exit_code,
        "output": {
            "stdout": stdout,
            "stderr": stderr,
        },
        "stdout": stdout,
        "stderr": stderr,
        "conflict": null,
        "conflict_reason": null,
        "changed_paths": changed_paths,
        "changed_path_kinds": Value::Object(
            path_kinds
                .iter()
                .map(|(path, kind)| (path.clone(), json!(kind)))
                .collect()
        ),
        "mutation_source": "isolated_workspace",
        "isolated_workspace": {
            "agent_id": handle.agent_id.clone(),
            "workspace_handle_id": handle.workspace_handle_id.clone(),
            "manifest_version": handle.manifest_version,
            "manifest_root_hash": handle.manifest_root_hash.clone(),
            "published": false,
        },
        "timings": Value::Object(timings),
        "warnings": runner
            .tool_result
            .get("warnings")
            .cloned()
            .unwrap_or_else(|| json!([])),
    });
    if include_pty_session_id {
        if let Some(pty_session_id) = runner.tool_result.get("pty_session_id").cloned() {
            response["pty_session_id"] = pty_session_id;
        }
    }
    record_isolated_runner_tool_call(handle, runner, status, &response, total_start);
    Ok(response)
}

#[cfg(target_os = "linux")]
fn capture_isolated_path_kinds(
    handle: &crate::isolated::CommandHandle,
) -> Result<(Vec<(String, String)>, f64), DaemonError> {
    let capture_start = Instant::now();
    let changes = capture_upperdir(&handle.upperdir)
        .map_err(|err| overlay_daemon_error("capture isolated upperdir", &err))?;
    let capture_s = capture_start.elapsed().as_secs_f64();
    let path_kinds = changes
        .iter()
        .map(|change| {
            (
                change.path().as_str().to_owned(),
                layer_change_kind(change).to_owned(),
            )
        })
        .collect();
    Ok((path_kinds, capture_s))
}

#[cfg(target_os = "linux")]
fn isolated_runner_timings(
    handle: &crate::isolated::CommandHandle,
    runner: &RunResult,
    changed_path_count: usize,
    capture_s: f64,
    total_start: Instant,
) -> Result<serde_json::Map<String, Value>, DaemonError> {
    let manifest = LayerStack::open(handle.layer_stack_root.clone())?.read_active_manifest()?;
    let mut timings = resource_timings(&manifest, changed_path_count);
    merge_runner_timings(&mut timings, runner);
    timings.insert(
        "command_exec.capture_upperdir_s".to_owned(),
        json!(capture_s),
    );
    timings.insert("command_exec.occ_apply_s".to_owned(), json!(0.0));
    timings.insert(
        "command_exec.total_s".to_owned(),
        json!(total_start.elapsed().as_secs_f64()),
    );
    timings.insert(
        "api.exec_command.total_s".to_owned(),
        json!(total_start.elapsed().as_secs_f64()),
    );
    Ok(timings)
}

#[cfg(target_os = "linux")]
fn record_isolated_runner_tool_call(
    handle: &crate::isolated::CommandHandle,
    runner: &RunResult,
    status: &str,
    response: &Value,
    total_start: Instant,
) {
    crate::isolated::record_tool_call(
        &handle.agent_id,
        json!({
            "workspace_handle_id": handle.workspace_handle_id.clone(),
            "exit_code": runner.exit_code,
            "argv0": "bash",
            "status": status,
            "changed_paths": response["changed_paths"].clone(),
            "published": false,
            "duration_s": total_start.elapsed().as_secs_f64(),
            "total_ms": total_start.elapsed().as_secs_f64() * 1000.0,
            "phases_ms": {
                "exec": total_start.elapsed().as_secs_f64() * 1000.0,
            },
        }),
    );
}

#[cfg(target_os = "linux")]
fn runner_ns_fds(map: &HashMap<String, i32>) -> Option<NsFds> {
    if map.is_empty() {
        return None;
    }
    Some(NsFds {
        user: map.get("user").copied().map(Fd),
        mnt: map.get("mnt").copied().map(Fd),
        pid: map.get("pid").copied().map(Fd),
        net: map.get("net").copied().map(Fd),
    })
}

#[cfg(target_os = "linux")]
const fn runner_mode(ns_fds: Option<&NsFds>) -> RunMode {
    if ns_fds.is_some() {
        RunMode::SetNs
    } else {
        RunMode::FreshNs
    }
}

#[cfg(target_os = "linux")]
fn start_isolated_pty_command(
    args: &Value,
    cmd: &str,
    timeout_seconds: Option<f64>,
    yield_time_ms: u64,
    handle: crate::isolated::CommandHandle,
) -> Result<Value, DaemonError> {
    let invocation_id = args
        .get("invocation_id")
        .and_then(Value::as_str)
        .unwrap_or("exec_command")
        .to_owned();
    let spec = PtyStartSpec {
        id: pty_registry().next_id(),
        invocation_id,
        agent_id: handle.agent_id.clone(),
        command: cmd.to_owned(),
        timeout_seconds,
    };
    let id = spec.id.clone();
    let (workspace, session, mut child) = prepare_isolated_pty_command(&spec, handle)?;
    let deadline = Instant::now() + Duration::from_millis(yield_time_ms);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            let response = IsolatedPtyFinalizer {
                session,
                child,
                workspace,
            }
            .finish(false);
            return Ok(strip_session_id(response));
        }
        thread::sleep(Duration::from_millis(5));
    }
    if child.try_wait()?.is_some() {
        let response = IsolatedPtyFinalizer {
            session,
            child,
            workspace,
        }
        .finish(false);
        return Ok(strip_session_id(response));
    }
    let stdout = session.output.all_recent(None);
    pty_registry().insert(Arc::clone(&session));
    crate::isolated::register_pty(&session.agent_id, &session.id);
    thread::spawn(move || {
        let _ = IsolatedPtyFinalizer {
            session,
            child,
            workspace,
        }
        .finish(true);
    });
    Ok(command_result("running", None, &stdout, "", Some(id)))
}

#[cfg(target_os = "linux")]
fn start_pty_command(
    args: &Value,
    cmd: &str,
    timeout_seconds: Option<f64>,
    yield_time_ms: u64,
) -> Result<Value, DaemonError> {
    let root = PathBuf::from(require_string(args, "layer_stack_root")?);
    let invocation_id = args
        .get("invocation_id")
        .and_then(Value::as_str)
        .unwrap_or("exec_command")
        .to_owned();
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_owned();
    let binding = require_workspace_binding(&root)?;
    let mut stack = LayerStack::open(root.clone())?;
    let lease = stack.acquire_snapshot(&format!("pty:{agent_id}:{invocation_id}"))?;
    let spec = PtyStartSpec {
        id: pty_registry().next_id(),
        invocation_id,
        agent_id,
        command: cmd.to_owned(),
        timeout_seconds,
    };
    let id = spec.id.clone();
    let prepare_result = prepare_pty_command(&root, &binding, &lease, &spec);

    match prepare_result {
        Ok((workspace, session, mut child)) => {
            let deadline = Instant::now() + Duration::from_millis(yield_time_ms);
            while Instant::now() < deadline {
                if child.try_wait()?.is_some() {
                    let response = PtyFinalizer {
                        session,
                        child,
                        workspace,
                    }
                    .finish(false);
                    return Ok(strip_session_id(response));
                }
                thread::sleep(Duration::from_millis(5));
            }
            if child.try_wait()?.is_some() {
                let response = PtyFinalizer {
                    session,
                    child,
                    workspace,
                }
                .finish(false);
                return Ok(strip_session_id(response));
            }
            let stdout = session.output.all_recent(None);
            pty_registry().insert(Arc::clone(&session));
            thread::spawn(move || {
                let _ = PtyFinalizer {
                    session,
                    child,
                    workspace,
                }
                .finish(true);
            });
            Ok(command_result("running", None, &stdout, "", Some(id)))
        }
        Err(err) => {
            let _ = stack.release_lease(&lease.lease_id);
            Err(err)
        }
    }
}

#[cfg(target_os = "linux")]
fn prepare_isolated_pty_command(
    spec: &PtyStartSpec,
    handle: crate::isolated::CommandHandle,
) -> Result<(IsolatedPtyWorkspace, Arc<PtySession>, Child), DaemonError> {
    let session_dir = handle.scratch_dir.join("pty-sessions").join(&spec.id);
    std::fs::create_dir_all(&session_dir)?;
    let transcript_path = session_dir.join("transcript.log");
    let final_path = session_dir.join("final.json");
    let output_path = session_dir.join("runner-result.json");
    let request_path = session_dir.join("runner-request.json");
    std::fs::write(
        session_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "pty_session_id": spec.id,
            "agent_id": handle.agent_id,
            "invocation_id": spec.invocation_id,
            "workspace": "isolated",
            "workspace_handle_id": handle.workspace_handle_id,
            "command": spec.command,
            "status": "running",
        }))
        .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    let ns_fds = runner_ns_fds(&handle.ns_fds);
    let request = RunRequest {
        mode: runner_mode(ns_fds.as_ref()),
        tool_call: ToolCall {
            invocation_id: spec.invocation_id.clone(),
            agent_id: handle.agent_id.clone(),
            verb: "exec_command".to_owned(),
            intent: Intent::WriteAllowed,
            args: json!({
                "command": spec.command,
                "cwd": ".",
                "tty": true,
            }),
            background: false,
        },
        workspace_root: WorkspaceRoot(handle.workspace_root.clone()),
        layer_paths: handle.layer_paths.clone(),
        upperdir: Some(handle.upperdir.clone()),
        workdir: Some(handle.workdir.clone()),
        ns_fds,
        cgroup_path: handle.cgroup_path.clone(),
        timeout_seconds: spec.timeout_seconds,
    };
    write_run_request(&request_path, &request)?;
    let (session, child) =
        spawn_pty_runner_session(spec, &request_path, &output_path, transcript_path)?;
    Ok((
        IsolatedPtyWorkspace {
            handle,
            output_path,
            final_path,
        },
        session,
        child,
    ))
}

#[cfg(target_os = "linux")]
fn prepare_pty_command(
    root: &Path,
    binding: &WorkspaceBinding,
    lease: &Lease,
    spec: &PtyStartSpec,
) -> Result<(PtyWorkspace, Arc<PtySession>, Child), DaemonError> {
    let runtime_root = overlay_writable_root()
        .map_err(|err| overlay_daemon_error("overlay writable root", &err))?
        .join("runtime");
    let run_root = runtime_root.join("sandbox-overlay").join(format!(
        "{}-{}",
        std::process::id(),
        sanitize_path_component(&spec.invocation_id)
    ));
    let dirs = allocate_overlay_writable_dirs(&run_root)
        .map_err(|err| overlay_daemon_error("allocate overlay dirs", &err))?;
    let session_dir = runtime_root.join("pty-sessions").join(&spec.id);
    std::fs::create_dir_all(&session_dir)?;
    let transcript_path = session_dir.join("transcript.log");
    let final_path = session_dir.join("final.json");
    std::fs::write(
        session_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "pty_session_id": spec.id,
            "agent_id": spec.agent_id,
            "invocation_id": spec.invocation_id,
            "command": spec.command,
            "status": "running",
        }))
        .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    let output_path = dirs.run_dir.join("pty-runner-result.json");
    let request_path = dirs.run_dir.join("pty-runner-request.json");
    let request = RunRequest {
        mode: RunMode::FreshNs,
        tool_call: ToolCall {
            invocation_id: spec.invocation_id.clone(),
            agent_id: spec.agent_id.clone(),
            verb: "exec_command".to_owned(),
            intent: Intent::WriteAllowed,
            args: json!({
                "command": spec.command,
                "cwd": ".",
                "tty": true,
            }),
            background: false,
        },
        workspace_root: WorkspaceRoot(PathBuf::from(&binding.workspace_root)),
        layer_paths: lease.layer_paths.iter().map(PathBuf::from).collect(),
        upperdir: Some(dirs.upperdir.clone()),
        workdir: Some(dirs.workdir.clone()),
        ns_fds: None,
        cgroup_path: None,
        timeout_seconds: spec.timeout_seconds,
    };
    write_run_request(&request_path, &request)?;
    let (session, child) =
        spawn_pty_runner_session(spec, &request_path, &output_path, transcript_path)?;
    Ok((
        PtyWorkspace {
            root: root.to_path_buf(),
            lease_id: lease.lease_id.clone(),
            manifest: lease.manifest.clone(),
            manifest_version: lease.manifest_version,
            upperdir: dirs.upperdir,
            run_dir: dirs.run_dir,
            output_path,
            final_path,
        },
        session,
        child,
    ))
}

#[cfg(target_os = "linux")]
fn write_run_request(path: &Path, request: &RunRequest) -> Result<(), DaemonError> {
    std::fs::write(
        path,
        serde_json::to_vec(request).map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn spawn_pty_runner_session(
    spec: &PtyStartSpec,
    request_path: &Path,
    output_path: &Path,
    transcript_path: PathBuf,
) -> Result<(Arc<PtySession>, Child), DaemonError> {
    let pty = openpty(None, None)
        .map_err(|err| DaemonError::OverlayPipeline(format!("open pty: {err}")))?;
    let master: File = pty.master.into();
    let slave: File = pty.slave.into();
    let mut child_command = Command::new(std::env::current_exe()?);
    child_command
        .arg("ns-runner")
        .arg("--request")
        .arg(request_path)
        .arg("--output")
        .arg(output_path)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave))
        .process_group(0);
    let child = child_command.spawn()?;
    let pgid = i32::try_from(child.id()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("child pid does not fit i32: {}", child.id()),
        )
    })?;
    let output = Arc::new(PtyOutput::new());
    let session = Arc::new(PtySession {
        id: spec.id.clone(),
        agent_id: spec.agent_id.clone(),
        command: spec.command.clone(),
        pgid,
        writer: Mutex::new(master.try_clone()?),
        output: Arc::clone(&output),
        cancelled: Mutex::new(false),
    });
    spawn_pty_reader(master, output, transcript_path);
    Ok((session, child))
}

#[cfg(target_os = "linux")]
fn spawn_pty_reader(mut master: File, output: Arc<PtyOutput>, transcript_path: PathBuf) {
    thread::spawn(move || {
        let mut transcript = OpenOptions::new()
            .create(true)
            .append(true)
            .open(transcript_path)
            .ok();
        let mut buf = [0_u8; 8192];
        loop {
            match master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                    output.append(text);
                    if output.note_spooled(u64::try_from(n).unwrap_or(u64::MAX)) {
                        if let Some(file) = transcript.as_mut() {
                            let _ = file.write_all(&buf[..n]);
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
}

#[cfg(target_os = "linux")]
struct PtyFinalizer {
    session: Arc<PtySession>,
    child: Child,
    workspace: PtyWorkspace,
}

#[cfg(target_os = "linux")]
impl PtyFinalizer {
    fn finish(mut self, publish_completion: bool) -> Value {
        let status = self.child.wait();
        terminate_pty_process_group(self.session.pgid);
        let runner = std::fs::read(&self.workspace.output_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RunResult>(&bytes).ok());
        let mut exit_code = runner
            .as_ref()
            .map(|result| i64::from(result.exit_code))
            .or_else(|| {
                status.ok().map(|status| {
                    status
                        .code()
                        .map(i64::from)
                        .or_else(|| status.signal().map(|signal| -i64::from(signal)))
                        .unwrap_or(1)
                })
            })
            .unwrap_or(1);
        let mut command_status = runner
            .as_ref()
            .and_then(|result| result.tool_result.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_owned();
        let cancelled = *lock_pty_state(&self.session.cancelled);
        if cancelled {
            "cancelled".clone_into(&mut command_status);
            if exit_code == 0 {
                exit_code = 130;
            }
        }
        let response = finalize_pty_workspace(
            &self.session,
            &self.workspace,
            &command_status,
            exit_code,
            publish_completion,
        )
        .unwrap_or_else(|err| {
            command_result(
                "error",
                Some(exit_code),
                &self.session.output.all_recent(None),
                &err.to_string(),
                Some(self.session.id.clone()),
            )
        });
        let _ = std::fs::remove_dir_all(&self.workspace.run_dir);
        let _ = LayerStack::open(self.workspace.root.clone())
            .and_then(|mut stack| stack.release_lease(&self.workspace.lease_id));
        let finalizer_owned_live_session = pty_registry().remove(&self.session.id).is_some();
        if should_publish_pty_completion(
            publish_completion,
            cancelled,
            finalizer_owned_live_session,
        ) {
            pty_registry().push_completed(json!({
                "pty_session_id": self.session.id,
                "agent_id": self.session.agent_id,
                "command": self.session.command,
                "result": response,
            }));
        }
        response
    }
}

#[cfg(target_os = "linux")]
struct IsolatedPtyFinalizer {
    session: Arc<PtySession>,
    child: Child,
    workspace: IsolatedPtyWorkspace,
}

#[cfg(target_os = "linux")]
impl IsolatedPtyFinalizer {
    fn finish(mut self, publish_completion: bool) -> Value {
        let status = self.child.wait();
        terminate_pty_process_group(self.session.pgid);
        let runner = std::fs::read(&self.workspace.output_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RunResult>(&bytes).ok());
        let mut exit_code = runner
            .as_ref()
            .map(|result| i64::from(result.exit_code))
            .or_else(|| {
                status.ok().map(|status| {
                    status
                        .code()
                        .map(i64::from)
                        .or_else(|| status.signal().map(|signal| -i64::from(signal)))
                        .unwrap_or(1)
                })
            })
            .unwrap_or(1);
        let mut command_status = runner
            .as_ref()
            .and_then(|result| result.tool_result.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_owned();
        let cancelled = *lock_pty_state(&self.session.cancelled);
        if cancelled {
            "cancelled".clone_into(&mut command_status);
            if exit_code == 0 {
                exit_code = 130;
            }
        }
        let response = finalize_isolated_pty_workspace(
            &self.session,
            &self.workspace,
            runner.as_ref(),
            &command_status,
            exit_code,
            publish_completion,
        )
        .unwrap_or_else(|err| {
            command_result(
                "error",
                Some(exit_code),
                &self.session.output.all_recent(None),
                &err.to_string(),
                Some(self.session.id.clone()),
            )
        });
        let finalizer_owned_live_session = pty_registry().remove(&self.session.id).is_some();
        crate::isolated::unregister_pty(&self.session.agent_id, &self.session.id);
        if should_publish_pty_completion(
            publish_completion,
            cancelled,
            finalizer_owned_live_session,
        ) {
            pty_registry().push_completed(json!({
                "pty_session_id": self.session.id,
                "agent_id": self.session.agent_id,
                "command": self.session.command,
                "result": response,
            }));
        }
        response
    }
}

#[cfg(target_os = "linux")]
fn finalize_isolated_pty_workspace(
    session: &PtySession,
    workspace: &IsolatedPtyWorkspace,
    runner: Option<&RunResult>,
    status: &str,
    exit_code: i64,
    include_session_id: bool,
) -> Result<Value, DaemonError> {
    let stdout = session.output.all_recent(None);
    let capture_start = Instant::now();
    let changes = capture_upperdir(&workspace.handle.upperdir)
        .map_err(|err| overlay_daemon_error("capture isolated upperdir", &err))?;
    let capture_s = capture_start.elapsed().as_secs_f64();
    let path_kinds: Vec<(String, String)> = changes
        .iter()
        .map(|change| {
            (
                change.path().as_str().to_owned(),
                layer_change_kind(change).to_owned(),
            )
        })
        .collect();
    let manifest =
        LayerStack::open(workspace.handle.layer_stack_root.clone())?.read_active_manifest()?;
    let mut timings = resource_timings(&manifest, path_kinds.len());
    if let Some(runner) = runner {
        merge_runner_timings(&mut timings, runner);
    }
    timings.insert(
        "command_exec.capture_upperdir_s".to_owned(),
        json!(capture_s),
    );
    timings.insert("command_exec.occ_apply_s".to_owned(), json!(0.0));
    let changed_paths: Vec<String> = path_kinds.iter().map(|(path, _)| path.clone()).collect();
    let changed_path_kinds = Value::Object(
        path_kinds
            .into_iter()
            .map(|(path, kind)| (path, json!(kind)))
            .collect(),
    );
    let mut response = json!({
        "success": true,
        "workspace": "isolated",
        "workspace_mode": "isolated",
        "status": status,
        "exit_code": exit_code,
        "output": {
            "stdout": stdout,
            "stderr": "",
        },
        "stdout": stdout,
        "stderr": "",
        "conflict": null,
        "conflict_reason": null,
        "changed_paths": changed_paths,
        "changed_path_kinds": changed_path_kinds,
        "mutation_source": "isolated_workspace",
        "isolated_workspace": {
            "agent_id": workspace.handle.agent_id.clone(),
            "workspace_handle_id": workspace.handle.workspace_handle_id.clone(),
            "manifest_version": workspace.handle.manifest_version,
            "manifest_root_hash": workspace.handle.manifest_root_hash.clone(),
            "published": false,
        },
        "timings": Value::Object(timings),
        "warnings": [],
        "spool_truncated": session.output.spool_truncated(),
    });
    if include_session_id {
        response["pty_session_id"] = json!(session.id.clone());
    }
    std::fs::write(
        &workspace.final_path,
        serde_json::to_vec_pretty(&response)
            .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    crate::isolated::record_tool_call(
        &workspace.handle.agent_id,
        json!({
            "workspace_handle_id": workspace.handle.workspace_handle_id.clone(),
            "exit_code": exit_code,
            "argv0": "bash",
            "status": status,
            "changed_paths": response["changed_paths"].clone(),
            "published": false,
            "pty_session_id": session.id.clone(),
            "duration_s": 0.0,
            "total_ms": 0.0,
            "phases_ms": {
                "exec": 0.0,
            },
        }),
    );
    Ok(response)
}

#[cfg(target_os = "linux")]
fn finalize_pty_workspace(
    session: &PtySession,
    workspace: &PtyWorkspace,
    status: &str,
    exit_code: i64,
    include_session_id: bool,
) -> Result<Value, DaemonError> {
    let stdout = session.output.all_recent(None);
    let capture_start = Instant::now();
    let changes = capture_upperdir(&workspace.upperdir)
        .map_err(|err| overlay_daemon_error("capture upperdir", &err))?;
    let capture_s = capture_start.elapsed().as_secs_f64();
    let path_kinds: Vec<(String, String)> = changes
        .iter()
        .map(|change| {
            (
                change.path().as_str().to_owned(),
                layer_change_kind(change).to_owned(),
            )
        })
        .collect();
    let route_start = Instant::now();
    let route_metrics = occ_route_metrics(&workspace.root, &changes)?;
    let route_s = route_start.elapsed().as_secs_f64();
    let base_hashes = base_hashes_for_snapshot(&workspace.root, &workspace.manifest, &changes)?;
    let occ_start = Instant::now();
    let changeset = apply_occ_changeset(
        &workspace.root,
        Some(manifest_version_u64(workspace.manifest_version)?),
        &changes,
        &base_hashes,
    )?;
    let occ_s = occ_start.elapsed().as_secs_f64();
    let manifest = LayerStack::open(workspace.root.clone())?.read_active_manifest()?;
    let mut timings = resource_timings(&manifest, path_kinds.len());
    insert_occ_route_timings(&mut timings, route_metrics, route_s, occ_s);
    let mut response =
        guarded_changeset_response("shell", &changeset, timings, Instant::now(), None);
    response["status"] = json!(status);
    response["exit_code"] = json!(exit_code);
    response["output"] = json!({"stdout": stdout, "stderr": ""});
    response["stdout"] = response["output"]["stdout"].clone();
    response["stderr"] = json!("");
    response["changed_path_kinds"] = Value::Object(
        path_kinds
            .into_iter()
            .map(|(path, kind)| (path, json!(kind)))
            .collect(),
    );
    response["timings"]["command_exec.capture_upperdir_s"] = json!(capture_s);
    response["timings"]["command_exec.occ_apply_s"] = json!(occ_s);
    response["spool_truncated"] = json!(session.output.spool_truncated());
    if include_session_id {
        response["pty_session_id"] = json!(session.id);
    }
    std::fs::write(
        &workspace.final_path,
        serde_json::to_vec_pretty(&response)
            .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    Ok(command_result(
        status,
        Some(exit_code),
        response["output"]["stdout"].as_str().unwrap_or_default(),
        "",
        if include_session_id {
            Some(session.id.clone())
        } else {
            None
        },
    ))
}

#[cfg(target_os = "linux")]
fn strip_session_id(mut response: Value) -> Value {
    if let Some(object) = response.as_object_mut() {
        object.remove("pty_session_id");
    }
    response
}

#[cfg(target_os = "linux")]
fn terminate_pty_process_group(pgid: i32) {
    if killpg(Pid::from_raw(pgid), Signal::SIGTERM).is_ok() {
        thread::sleep(Duration::from_millis(50));
        let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
    }
}

#[cfg(target_os = "linux")]
fn pty_write_stdin(args: &Value) -> Result<Value, DaemonError> {
    let id = require_string(args, "pty_session_id")?;
    let chars = args
        .get("chars")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let yield_time_ms = optional_u64(args, "yield_time_ms").unwrap_or(1000);
    let max_tokens = optional_u64(args, "max_tokens");
    let registry = pty_registry();
    let Some(session) = registry.get(&id) else {
        if let Some(result) = registry.take_completed_result(&id) {
            return Ok(result);
        }
        return Ok(pty_not_found());
    };
    let since = Instant::now();
    {
        let mut writer = lock_pty_state(&session.writer);
        writer.write_all(chars.as_bytes())?;
    }
    thread::sleep(Duration::from_millis(yield_time_ms));
    if let Some(result) = pty_registry().take_completed_result(&id) {
        return Ok(result);
    }
    Ok(command_result(
        "running",
        None,
        &session.output.recent_since(since, max_tokens),
        "",
        Some(id),
    ))
}

#[cfg(target_os = "linux")]
fn pty_progress(args: &Value) -> Result<Value, DaemonError> {
    let id = require_string(args, "pty_session_id")?;
    let seconds = args.get("time").and_then(Value::as_f64).unwrap_or(1.0);
    let max_tokens = optional_u64(args, "max_tokens");
    let registry = pty_registry();
    let Some(session) = registry.get(&id) else {
        if let Some(result) = registry.take_completed_result(&id) {
            return Ok(result);
        }
        return Ok(pty_not_found());
    };
    let window = if seconds.is_finite() && seconds > 0.0 {
        Duration::from_secs_f64(seconds)
    } else {
        Duration::from_secs(0)
    };
    let since = Instant::now()
        .checked_sub(window)
        .unwrap_or_else(Instant::now);
    Ok(command_result(
        "running",
        None,
        &session.output.recent_since(since, max_tokens),
        "",
        Some(id),
    ))
}

#[cfg(target_os = "linux")]
fn pty_cancel(args: &Value) -> Result<Value, DaemonError> {
    let id = require_string(args, "pty_session_id")?;
    let registry = pty_registry();
    let Some(session) = registry.remove(&id) else {
        if let Some(result) = registry.take_completed_result(&id) {
            return Ok(result);
        }
        return Ok(pty_not_found());
    };
    *lock_pty_state(&session.cancelled) = true;
    terminate_pty_process_group(session.pgid);
    crate::isolated::unregister_pty(&session.agent_id, &id);
    Ok(command_result(
        "cancelled",
        None,
        &session.output.all_recent(optional_u64(args, "max_tokens")),
        "",
        None,
    ))
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "isolated exit cleanup keeps the same fallible helper signature across cfgs"
)]
pub fn cancel_pty_session_for_exit(id: &str) -> Result<bool, DaemonError> {
    let Some(session) = pty_registry().remove(id) else {
        crate::isolated::unregister_pty_id(id);
        return Ok(false);
    };
    *lock_pty_state(&session.cancelled) = true;
    terminate_pty_process_group(session.pgid);
    crate::isolated::unregister_pty(&session.agent_id, id);
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
// Keep the same fallible public helper signature as Linux so isolated exit can
// call it without cfg-splitting the cleanup path.
#[expect(
    clippy::unnecessary_wraps,
    reason = "non-Linux parity keeps the Linux fallible helper signature"
)]
pub const fn cancel_pty_session_for_exit(_id: &str) -> Result<bool, DaemonError> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn exec_command_requires_string_wire_shape() {
        assert!(require_command_string(&json!({"cmd": "echo hi"}), "cmd").is_ok());
        assert!(require_command_string(&json!({"cmd": ["true"]}), "cmd").is_err());
    }

    #[test]
    fn exec_command_preserves_shell_string_bytes_after_validation() -> TestResult {
        assert_eq!(
            require_command_string(&json!({"cmd": "  printf hi\n"}), "cmd")?,
            "  printf hi\n"
        );
        Ok(())
    }

    #[test]
    fn optional_u64_accepts_unsigned_and_nonnegative_signed_numbers() {
        assert_eq!(optional_u64(&json!({"timeout": 7_u64}), "timeout"), Some(7));
        assert_eq!(optional_u64(&json!({"timeout": 7_i64}), "timeout"), Some(7));
        assert_eq!(optional_u64(&json!({"timeout": -1_i64}), "timeout"), None);
    }

    #[test]
    fn pty_cancel_suppresses_background_completion_publication() {
        assert!(should_publish_pty_completion(true, false, true));
        assert!(!should_publish_pty_completion(true, true, true));
        assert!(!should_publish_pty_completion(true, false, false));
        assert!(!should_publish_pty_completion(false, false, true));
        assert!(!should_publish_pty_completion(false, true, false));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pty_completion_result_can_be_claimed_by_control_tool() -> TestResult {
        let registry = PtyRegistry::new();
        registry.push_completed(json!({
            "pty_session_id": "pty_keep",
            "result": {"status": "ok", "exit_code": 0},
        }));
        registry.push_completed(json!({
            "pty_session_id": "pty_done",
            "result": {"status": "ok", "exit_code": 0},
        }));

        let result = registry
            .take_completed_result("pty_done")
            .ok_or("matching completion should be returned")?;
        assert_eq!(result["status"], "ok");
        assert!(registry.take_completed_result("pty_done").is_none());

        let remaining = registry.collect_completed(&json!({"pty_session_ids": ["pty_keep"]}));
        assert_eq!(
            remaining["completions"]
                .as_array()
                .ok_or("completions should be an array")?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pty_session_count_counts_live_sessions_by_agent() -> TestResult {
        let registry = PtyRegistry::new();
        let output = Arc::new(PtyOutput::new());
        let writer = || -> TestResult<_> {
            Ok(Mutex::new(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/null")?,
            ))
        };
        registry.insert(Arc::new(PtySession {
            id: "pty_a".to_owned(),
            agent_id: "agent-a".to_owned(),
            command: "python".to_owned(),
            pgid: 1,
            writer: writer()?,
            output: Arc::clone(&output),
            cancelled: Mutex::new(false),
        }));
        registry.insert(Arc::new(PtySession {
            id: "pty_b".to_owned(),
            agent_id: "agent-b".to_owned(),
            command: "bash".to_owned(),
            pgid: 2,
            writer: writer()?,
            output,
            cancelled: Mutex::new(false),
        }));

        assert_eq!(registry.count_by_agent("agent-a"), 1);
        assert_eq!(registry.count_by_agent("agent-b"), 1);
        assert_eq!(registry.count_by_agent(""), 2);
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pty_write_stdin_returns_completed_result_when_live_session_is_gone() -> TestResult {
        let id = "pty_stdin_done_unit";
        pty_registry().push_completed(json!({
            "pty_session_id": id,
            "result": {
                "status": "ok",
                "exit_code": 0,
                "output": {"stdout": "written\n", "stderr": ""},
            },
        }));

        let response = pty_write_stdin(&json!({"pty_session_id": id, "chars": "ignored"}))?;

        assert_eq!(response["status"], "ok");
        assert_eq!(response["output"]["stdout"], "written\n");
        assert!(pty_registry().take_completed_result(id).is_none());
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pty_progress_returns_completed_result_when_live_session_is_gone() -> TestResult {
        let id = "pty_progress_done_unit";
        pty_registry().push_completed(json!({
            "pty_session_id": id,
            "result": {
                "status": "ok",
                "exit_code": 0,
                "output": {"stdout": "done\n", "stderr": ""},
            },
        }));

        let response = pty_progress(&json!({"pty_session_id": id, "time": 0.01}))?;

        assert_eq!(response["status"], "ok");
        assert_eq!(response["output"]["stdout"], "done\n");
        assert!(pty_registry().take_completed_result(id).is_none());
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn pty_cancel_returns_completed_result_when_live_session_is_gone() -> TestResult {
        let id = "pty_cancel_done_unit";
        pty_registry().push_completed(json!({
            "pty_session_id": id,
            "result": {
                "status": "ok",
                "exit_code": 0,
                "output": {"stdout": "already-finished\n", "stderr": ""},
            },
        }));

        let response = pty_cancel(&json!({"pty_session_id": id}))?;

        assert_eq!(response["status"], "ok");
        assert_eq!(response["output"]["stdout"], "already-finished\n");
        assert!(pty_registry().take_completed_result(id).is_none());
        Ok(())
    }
}
