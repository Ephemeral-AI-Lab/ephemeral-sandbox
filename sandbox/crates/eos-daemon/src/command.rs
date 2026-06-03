//! Command-session operations for the daemon dispatcher.

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
use std::sync::{mpsc as std_mpsc, Arc, Mutex, MutexGuard, OnceLock};
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use eos_terminal_pair::open_terminal_pair;
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
    insert_occ_route_timings, insert_tree_resource_timings, layer_change_kind,
    manifest_version_u64, merge_runner_timings, occ_route_metrics, overlay_daemon_error,
    resource_timings, TreeResourceStats,
};
use crate::dispatcher::{u64_to_f64_saturating, DispatchContext};
use crate::error::DaemonError;

/// `api.v1.exec_command` — command-session start contract.
pub fn op_exec_command(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let cmd = require_command_string(args, "cmd")?;
    #[cfg(not(target_os = "linux"))]
    let _ = &cmd;
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
        let yield_time_ms = optional_u64(args, "yield_time_ms").unwrap_or(1000);
        return start_isolated_command_session(args, &cmd, timeout_seconds, yield_time_ms, handle);
    }

    #[cfg(target_os = "linux")]
    {
        let yield_time_ms = optional_u64(args, "yield_time_ms").unwrap_or(1000);
        start_command_session(args, &cmd, timeout_seconds, yield_time_ms)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = timeout_seconds;
        Ok(command_result(
            "error",
            None,
            "",
            "command sessions are only supported on linux",
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
pub fn op_command_write_stdin(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        command_session_write_stdin(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(command_session_not_found())
    }
}

#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "dispatcher handlers share a fallible ABI"
    )
)]
pub fn op_command_cancel(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        command_session_cancel(args)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(command_session_not_found())
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_command_collect_completed(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        Ok(command_session_registry().collect_completed(args))
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
pub fn op_command_session_count(
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
        let count = command_session_registry().count_by_agent(&agent_id);
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

fn command_result(
    status: &str,
    exit_code: Option<i64>,
    stdout: &str,
    stderr: &str,
    command_session_id: Option<String>,
) -> Value {
    let mut response = json!({
        "status": status,
        "exit_code": exit_code,
        "output": {
            "stdout": stdout,
            "stderr": stderr,
        },
    });
    if let Some(command_session_id) = command_session_id {
        response["command_session_id"] = json!(command_session_id);
    }
    response
}

fn command_session_not_found() -> Value {
    command_result("error", None, "", "command_session_not_found", None)
}

#[cfg(any(target_os = "linux", test))]
const fn should_publish_command_session_completion(
    publish_completion: bool,
    cancelled: bool,
    owned_live_session: bool,
) -> bool {
    publish_completion && !cancelled && owned_live_session
}

#[cfg(target_os = "linux")]
const COMMAND_SESSION_RING_MAX_BYTES: usize = 1024 * 1024;
#[cfg(target_os = "linux")]
const COMMAND_SESSION_SPOOL_MAX_BYTES: u64 = 32 * 1024 * 1024;
#[cfg(target_os = "linux")]
const COMMAND_SESSION_OUTPUT_DRAIN_GRACE_MS: u64 = 500;

#[cfg(target_os = "linux")]
fn lock_command_session_state<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(target_os = "linux")]
struct CommandSessionOutput {
    chunks: Mutex<VecDeque<CommandSessionOutputChunk>>,
    bytes: Mutex<usize>,
    next_byte_offset: Mutex<u64>,
    spool_bytes: Mutex<u64>,
    spool_truncated: Mutex<bool>,
}

#[cfg(target_os = "linux")]
struct CommandSessionOutputChunk {
    start: u64,
    end: u64,
    text: String,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Default)]
struct CommandSessionOutputCursor {
    next_seq: u64,
    next_byte_offset: u64,
}

#[cfg(target_os = "linux")]
impl CommandSessionOutput {
    const fn new() -> Self {
        Self {
            chunks: Mutex::new(VecDeque::new()),
            bytes: Mutex::new(0),
            next_byte_offset: Mutex::new(0),
            spool_bytes: Mutex::new(0),
            spool_truncated: Mutex::new(false),
        }
    }

    fn append(&self, text: String) {
        let byte_len = text.len();
        let mut next_byte_offset = lock_command_session_state(&self.next_byte_offset);
        let start = *next_byte_offset;
        let end = start.saturating_add(u64::try_from(byte_len).unwrap_or(u64::MAX));
        *next_byte_offset = end;
        drop(next_byte_offset);
        let mut chunks = lock_command_session_state(&self.chunks);
        let mut bytes = lock_command_session_state(&self.bytes);
        chunks.push_back(CommandSessionOutputChunk { start, end, text });
        *bytes += byte_len;
        while *bytes > COMMAND_SESSION_RING_MAX_BYTES {
            let Some(chunk) = chunks.pop_front() else {
                break;
            };
            *bytes = bytes.saturating_sub(chunk.text.len());
        }
        drop(bytes);
        drop(chunks);
    }

    fn read_since(
        &self,
        cursor: &mut CommandSessionOutputCursor,
        max_tokens: Option<u64>,
    ) -> String {
        let chunks = lock_command_session_state(&self.chunks);
        let Some(first) = chunks.front() else {
            return String::new();
        };
        let mut out = String::new();
        if cursor.next_byte_offset < first.start {
            out.push_str("[output truncated before cursor]\n");
            cursor.next_byte_offset = first.start;
        }
        let max_bytes = max_output_bytes(max_tokens);
        for chunk in chunks.iter() {
            if chunk.end <= cursor.next_byte_offset {
                continue;
            }
            let start_offset = cursor.next_byte_offset.saturating_sub(chunk.start);
            let start = usize::try_from(start_offset).unwrap_or(usize::MAX);
            let text = slice_from_byte(&chunk.text, start);
            if text.is_empty() {
                continue;
            }
            let remaining = max_bytes.saturating_sub(out.len());
            if remaining == 0 {
                break;
            }
            let take = text.len().min(remaining);
            let take = floor_char_boundary(text, take);
            if take == 0 {
                break;
            }
            out.push_str(&text[..take]);
            cursor.next_byte_offset = cursor
                .next_byte_offset
                .saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
            cursor.next_seq = cursor.next_seq.saturating_add(1);
            if take < text.len() {
                break;
            }
        }
        out
    }

    fn all_recent(&self, max_tokens: Option<u64>) -> String {
        let chunks = lock_command_session_state(&self.chunks);
        let mut out = String::new();
        let max_bytes = max_output_bytes(max_tokens);
        for chunk in chunks.iter() {
            let remaining = max_bytes.saturating_sub(out.len());
            if remaining == 0 {
                break;
            }
            let take = floor_char_boundary(&chunk.text, chunk.text.len().min(remaining));
            if take == 0 {
                break;
            }
            out.push_str(&chunk.text[..take]);
        }
        out
    }

    fn note_spooled(&self, bytes: u64) -> bool {
        let mut spool_bytes = lock_command_session_state(&self.spool_bytes);
        if *spool_bytes >= COMMAND_SESSION_SPOOL_MAX_BYTES {
            *lock_command_session_state(&self.spool_truncated) = true;
            return false;
        }
        *spool_bytes = (*spool_bytes + bytes).min(COMMAND_SESSION_SPOOL_MAX_BYTES);
        true
    }

    fn spool_truncated(&self) -> bool {
        *lock_command_session_state(&self.spool_truncated)
    }

    /// The next byte offset (total bytes appended) — the progress signal
    /// `wait_for_yield` watches for quiet-after-output settling.
    fn next_byte_offset(&self) -> u64 {
        *lock_command_session_state(&self.next_byte_offset)
    }
}

#[cfg(target_os = "linux")]
fn max_output_bytes(max_tokens: Option<u64>) -> usize {
    max_tokens
        .and_then(|tokens| usize::try_from(tokens.saturating_mul(4)).ok())
        .filter(|value| *value > 0)
        .unwrap_or(80_000)
}

#[cfg(target_os = "linux")]
fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(target_os = "linux")]
fn slice_from_byte(text: &str, start: usize) -> &str {
    if start >= text.len() {
        return "";
    }
    let start = floor_char_boundary(text, start);
    &text[start..]
}

#[cfg(target_os = "linux")]
struct CommandSession {
    id: String,
    agent_id: String,
    command: String,
    started_at: Instant,
    pgid: i32,
    writer: Mutex<File>,
    output: Arc<CommandSessionOutput>,
    reader_done: Mutex<Option<std_mpsc::Receiver<()>>>,
    cancelled: Mutex<bool>,
    interrupted: Mutex<bool>,
    model_cursor: Mutex<CommandSessionOutputCursor>,
    notification_cursor: Mutex<CommandSessionOutputCursor>,
    // sense-2: the child lives in the session (was moved into a per-session
    // detached finalizer thread). One idempotent `try_finalize` reaps it.
    child: Mutex<Option<Child>>,
    workspace: CommandWorkspaceKind,
    finalized: Mutex<Option<Value>>,
    timeout_deadline: Option<Instant>,
}

#[cfg(target_os = "linux")]
impl CommandSession {
    fn read_model_output(&self, max_tokens: Option<u64>) -> String {
        let mut cursor = lock_command_session_state(&self.model_cursor);
        self.output.read_since(&mut cursor, max_tokens)
    }

    fn read_notification_output(&self, max_tokens: Option<u64>) -> String {
        let mut cursor = lock_command_session_state(&self.notification_cursor);
        self.output.read_since(&mut cursor, max_tokens)
    }

    /// Sense-2 idempotent finalize: returns the terminal result once the child
    /// has exited (caching it under the `finalized` latch so exec / write_stdin /
    /// reaper are at-most-once), or `None` while still running. `publish` parks
    /// the completion for the heartbeat (set by the reaper for unpolled exits;
    /// `false` for the inline tool-return path, so a polled session is never
    /// double-delivered).
    ///
    /// This subsumes the two former per-session detached finalizer threads;
    /// prologue/epilogue are shared and only the workspace-finalize body and the
    /// teardown branch on [`CommandWorkspaceKind`].
    fn try_finalize(&self, publish: bool) -> Option<Value> {
        let mut latch = lock_command_session_state(&self.finalized);
        if let Some(cached) = latch.as_ref() {
            return Some(cached.clone());
        }
        // Reap the child without blocking; bail while it is still running.
        let exit_status = {
            let mut child = lock_command_session_state(&self.child);
            match child.as_mut() {
                Some(handle) => match handle.try_wait() {
                    Ok(Some(status)) => {
                        let _ = child.take();
                        Some(status)
                    }
                    Ok(None) => return None,
                    // A wait error means the child is unwaitable; finalize anyway.
                    Err(_) => {
                        let _ = child.take();
                        None
                    }
                },
                // No child handle (already reaped) — finalize with the runner file.
                None => None,
            }
        };
        terminate_command_process_group(self.pgid);
        let runner = std::fs::read(self.workspace.output_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<RunResult>(&bytes).ok());
        let mut exit_code = runner
            .as_ref()
            .map(|result| i64::from(result.exit_code))
            .or_else(|| {
                exit_status.map(|status| {
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
        let cancelled = *lock_command_session_state(&self.cancelled);
        if cancelled
            || (*lock_command_session_state(&self.interrupted) && matches!(exit_code, 130 | -2))
        {
            "cancelled".clone_into(&mut command_status);
            exit_code = 130;
        }
        let stdout = completed_session_stdout(self);
        let response = match &self.workspace {
            CommandWorkspaceKind::Ephemeral(workspace) => finalize_command_workspace(
                self,
                workspace,
                &command_status,
                exit_code,
                &stdout,
                publish,
            ),
            CommandWorkspaceKind::Isolated(workspace) => finalize_isolated_command_workspace(
                self,
                workspace,
                runner.as_ref(),
                &command_status,
                exit_code,
                &stdout,
                publish,
            ),
        }
        .unwrap_or_else(|err| {
            command_result(
                "error",
                Some(exit_code),
                &stdout,
                &err.to_string(),
                Some(self.id.clone()),
            )
        });
        // Teardown MUST run even on a finalize Err, or the shared ephemeral lease
        // leaks. Isolated teardown is deferred to `exit_isolated_workspace`.
        match &self.workspace {
            CommandWorkspaceKind::Ephemeral(workspace) => {
                let _ = std::fs::remove_dir_all(&workspace.run_dir);
                let _ = LayerStack::open(workspace.root.clone())
                    .and_then(|mut stack| stack.release_lease(&workspace.lease_id));
            }
            CommandWorkspaceKind::Isolated(_) => {}
        }
        let owned_live_session = command_session_registry().remove(&self.id).is_some();
        if let CommandWorkspaceKind::Isolated(_) = &self.workspace {
            crate::isolated::unregister_command_session(&self.agent_id, &self.id);
        }
        if should_publish_command_session_completion(publish, cancelled, owned_live_session) {
            command_session_registry().push_completed(json!({
                "command_session_id": self.id,
                "agent_id": self.agent_id,
                "command": self.command,
                "result": response_with_stdout(response.clone(), self.read_model_output(None)),
                "notification_result": response_with_stdout(
                    response.clone(),
                    self.read_notification_output(None),
                ),
            }));
        }
        *latch = Some(response.clone());
        Some(response)
    }
}

/// Whether `wait_for_yield` finalized the session inline or it is still running.
#[cfg(target_os = "linux")]
enum WaitOutcome {
    /// The child exited; the terminal result is ready to return.
    Completed(Value),
    /// Still running; the model-facing output captured so far.
    Running(String),
}

/// Quiet window: after output appears, a settled gap this long lets a session
/// that "responded and went quiet" (e.g. a REPL prompt) yield early.
#[cfg(target_os = "linux")]
const COMMAND_SESSION_QUIET_MS: u64 = 50;

/// Sense-2 unified wait shared by `exec_command` and `write_stdin`: early-return
/// on completion (inline finalize) or on quiet-after-output, capped at the
/// caller's `yield_time_ms`.
#[cfg(target_os = "linux")]
fn wait_for_yield(
    session: &Arc<CommandSession>,
    yield_time_ms: u64,
    max_tokens: Option<u64>,
) -> WaitOutcome {
    let deadline = Instant::now() + Duration::from_millis(yield_time_ms);
    let start_off = session.output.next_byte_offset();
    let (mut last_off, mut last_change) = (start_off, Instant::now());
    loop {
        if let Some(result) = session.try_finalize(false) {
            return WaitOutcome::Completed(result);
        }
        let off = session.output.next_byte_offset();
        if off != last_off {
            last_off = off;
            last_change = Instant::now();
        }
        if off > start_off
            && last_change.elapsed() >= Duration::from_millis(COMMAND_SESSION_QUIET_MS)
        {
            return WaitOutcome::Running(session.read_model_output(max_tokens));
        }
        if Instant::now() >= deadline {
            return WaitOutcome::Running(session.read_model_output(max_tokens));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
struct CommandSessionRegistry {
    sessions: Mutex<HashMap<String, Arc<CommandSession>>>,
    /// Parked terminal completions awaiting heartbeat collection or a late
    /// `write_stdin` poll. Entries are removed on first delivery (by
    /// `collect_completed`) or claim (by `take_completed_result`), so the map
    /// stays bounded.
    completed: Mutex<HashMap<String, Value>>,
    counter: AtomicU64,
}

#[cfg(target_os = "linux")]
impl CommandSessionRegistry {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            completed: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
        }
    }

    fn next_id(&self) -> String {
        format!("cmd_{}", self.counter.fetch_add(1, Ordering::Relaxed))
    }

    fn insert(&self, session: Arc<CommandSession>) {
        lock_command_session_state(&self.sessions).insert(session.id.clone(), session);
    }

    fn get(&self, id: &str) -> Option<Arc<CommandSession>> {
        lock_command_session_state(&self.sessions).get(id).cloned()
    }

    fn remove(&self, id: &str) -> Option<Arc<CommandSession>> {
        lock_command_session_state(&self.sessions).remove(id)
    }

    fn count_by_agent(&self, agent_id: &str) -> usize {
        lock_command_session_state(&self.sessions)
            .values()
            .filter(|session| agent_id.is_empty() || session.agent_id == agent_id)
            .count()
    }

    /// A snapshot of the live sessions (for the reaper sweep).
    fn live(&self) -> Vec<Arc<CommandSession>> {
        lock_command_session_state(&self.sessions)
            .values()
            .cloned()
            .collect()
    }

    fn push_completed(&self, completion: Value) {
        let id = completion
            .get("command_session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if id.is_empty() {
            return;
        }
        lock_command_session_state(&self.completed).insert(id, completion);
    }

    fn take_completed_result(&self, id: &str) -> Option<Value> {
        lock_command_session_state(&self.completed)
            .remove(id)
            .and_then(|completion| completion.get("result").cloned())
    }

    /// Collect (and **remove**, so the map stays bounded) the parked completions
    /// matching the requested ids/agent. Removal on delivery is the exactly-once
    /// gate: a later `write_stdin` poll finds the entry gone and recovers the
    /// terse already-reported result from the agent-core supervisor (§8/D8).
    fn collect_completed(&self, args: &Value) -> Value {
        let wanted: Option<HashSet<String>> = args
            .get("command_session_ids")
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
        let mut completed = lock_command_session_state(&self.completed);
        let matched: Vec<String> = completed
            .iter()
            .filter(|(id, completion)| {
                let item_agent = completion
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let id_matches = wanted.as_ref().is_none_or(|ids| ids.contains(*id));
                let agent_matches = agent_id.is_empty() || agent_id == item_agent;
                id_matches && agent_matches
            })
            .map(|(id, _)| id.clone())
            .collect();
        let returned: Vec<Value> = matched
            .iter()
            .filter_map(|id| completed.remove(id))
            .map(|mut completion| {
                if let Some(notification_result) = completion.get("notification_result").cloned() {
                    completion["result"] = notification_result;
                }
                completion
            })
            .collect();
        drop(completed);
        json!({"success": true, "completions": returned})
    }
}

#[cfg(target_os = "linux")]
fn command_session_registry() -> &'static CommandSessionRegistry {
    static REGISTRY: OnceLock<CommandSessionRegistry> = OnceLock::new();
    REGISTRY.get_or_init(CommandSessionRegistry::new)
}

#[cfg(target_os = "linux")]
fn completed_session_stdout(session: &CommandSession) -> String {
    let reader_done = lock_command_session_state(&session.reader_done).take();
    if let Some(reader_done) = reader_done {
        let _ =
            reader_done.recv_timeout(Duration::from_millis(COMMAND_SESSION_OUTPUT_DRAIN_GRACE_MS));
    }
    session.output.all_recent(None)
}

#[cfg(target_os = "linux")]
struct EphemeralCommandWorkspace {
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
struct IsolatedCommandWorkspace {
    handle: crate::isolated::CommandHandle,
    output_path: PathBuf,
    final_path: PathBuf,
}

/// Which workspace a command session finalizes into (sense-2 §4). The notify
/// `publish` flag is orthogonal to this — both kinds can be parked.
#[cfg(target_os = "linux")]
enum CommandWorkspaceKind {
    /// Shared ephemeral overlay: finalize publishes via OCC and releases the
    /// per-session lease + run dir.
    Ephemeral(EphemeralCommandWorkspace),
    /// Isolated private workspace: finalize captures record-only; lease/scratch
    /// teardown is deferred to `exit_isolated_workspace`.
    Isolated(IsolatedCommandWorkspace),
}

#[cfg(target_os = "linux")]
impl CommandWorkspaceKind {
    /// The runner `--output` result file path (used by `try_finalize`).
    fn output_path(&self) -> &Path {
        match self {
            Self::Ephemeral(workspace) => &workspace.output_path,
            Self::Isolated(workspace) => &workspace.output_path,
        }
    }
}

#[cfg(target_os = "linux")]
struct CommandSessionStartSpec {
    id: String,
    invocation_id: String,
    agent_id: String,
    command: String,
    timeout_seconds: Option<f64>,
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
fn start_isolated_command_session(
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
    let spec = CommandSessionStartSpec {
        id: command_session_registry().next_id(),
        invocation_id,
        agent_id: handle.agent_id.clone(),
        command: cmd.to_owned(),
        timeout_seconds,
    };
    let id = spec.id.clone();
    let session = prepare_isolated_command_session(&spec, handle)?;
    match wait_for_yield(&session, yield_time_ms, optional_u64(args, "max_output_tokens")) {
        WaitOutcome::Completed(response) => Ok(strip_session_id(response)),
        WaitOutcome::Running(stdout) => {
            command_session_registry().insert(Arc::clone(&session));
            crate::isolated::register_command_session(&session.agent_id, &session.id);
            Ok(command_result("running", None, &stdout, "", Some(id)))
        }
    }
}

#[cfg(target_os = "linux")]
fn start_command_session(
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
    let lease = stack.acquire_snapshot(&format!("command_session:{agent_id}:{invocation_id}"))?;
    let spec = CommandSessionStartSpec {
        id: command_session_registry().next_id(),
        invocation_id,
        agent_id,
        command: cmd.to_owned(),
        timeout_seconds,
    };
    let id = spec.id.clone();
    match prepare_command_session(&root, &binding, &lease, &spec) {
        Ok(session) => {
            match wait_for_yield(&session, yield_time_ms, optional_u64(args, "max_output_tokens")) {
                WaitOutcome::Completed(response) => Ok(strip_session_id(response)),
                WaitOutcome::Running(stdout) => {
                    command_session_registry().insert(Arc::clone(&session));
                    Ok(command_result("running", None, &stdout, "", Some(id)))
                }
            }
        }
        Err(err) => {
            // prepare failed before the session owns the lease — release it here
            // (else the per-session lease leaks; sense-2 §12 guarantee).
            let _ = stack.release_lease(&lease.lease_id);
            Err(err)
        }
    }
}

#[cfg(target_os = "linux")]
fn prepare_isolated_command_session(
    spec: &CommandSessionStartSpec,
    handle: crate::isolated::CommandHandle,
) -> Result<Arc<CommandSession>, DaemonError> {
    let session_dir = handle.scratch_dir.join("command-sessions").join(&spec.id);
    std::fs::create_dir_all(&session_dir)?;
    let transcript_path = session_dir.join("transcript.log");
    let final_path = session_dir.join("final.json");
    let output_path = session_dir.join("runner-result.json");
    let request_path = session_dir.join("runner-request.json");
    std::fs::write(
        session_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "command_session_id": spec.id,
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
    let workspace = CommandWorkspaceKind::Isolated(IsolatedCommandWorkspace {
        handle,
        output_path,
        final_path,
    });
    spawn_command_runner_session(spec, &request_path, transcript_path, workspace)
}

#[cfg(target_os = "linux")]
fn prepare_command_session(
    root: &Path,
    binding: &WorkspaceBinding,
    lease: &Lease,
    spec: &CommandSessionStartSpec,
) -> Result<Arc<CommandSession>, DaemonError> {
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
    let session_dir = runtime_root.join("command-sessions").join(&spec.id);
    std::fs::create_dir_all(&session_dir)?;
    let transcript_path = session_dir.join("transcript.log");
    let final_path = session_dir.join("final.json");
    std::fs::write(
        session_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&json!({
            "command_session_id": spec.id,
            "agent_id": spec.agent_id,
            "invocation_id": spec.invocation_id,
            "command": spec.command,
            "status": "running",
        }))
        .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    let output_path = dirs.run_dir.join("command-runner-result.json");
    let request_path = dirs.run_dir.join("command-runner-request.json");
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
    let workspace = CommandWorkspaceKind::Ephemeral(EphemeralCommandWorkspace {
        root: root.to_path_buf(),
        lease_id: lease.lease_id.clone(),
        manifest: lease.manifest.clone(),
        manifest_version: lease.manifest_version,
        upperdir: dirs.upperdir,
        run_dir: dirs.run_dir,
        output_path,
        final_path,
    });
    spawn_command_runner_session(spec, &request_path, transcript_path, workspace)
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
fn spawn_command_runner_session(
    spec: &CommandSessionStartSpec,
    request_path: &Path,
    transcript_path: PathBuf,
    workspace: CommandWorkspaceKind,
) -> Result<Arc<CommandSession>, DaemonError> {
    let terminal_pair = open_terminal_pair()
        .map_err(|err| DaemonError::OverlayPipeline(format!("open terminal pair: {err}")))?;
    let master = terminal_pair.controller;
    let slave = terminal_pair.attached;
    let mut child_command = Command::new(std::env::current_exe()?);
    child_command
        .arg("ns-runner")
        .arg("--request")
        .arg(request_path)
        .arg("--output")
        .arg(workspace.output_path())
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
    let output = Arc::new(CommandSessionOutput::new());
    let writer = master.try_clone()?;
    let reader_done = spawn_command_output_reader(master, Arc::clone(&output), transcript_path);
    let started_at = Instant::now();
    let timeout_deadline = spec
        .timeout_seconds
        .map(|seconds| started_at + Duration::from_secs_f64(seconds));
    let session = Arc::new(CommandSession {
        id: spec.id.clone(),
        agent_id: spec.agent_id.clone(),
        command: spec.command.clone(),
        started_at,
        pgid,
        writer: Mutex::new(writer),
        output: Arc::clone(&output),
        reader_done: Mutex::new(Some(reader_done)),
        cancelled: Mutex::new(false),
        interrupted: Mutex::new(false),
        model_cursor: Mutex::new(CommandSessionOutputCursor::default()),
        notification_cursor: Mutex::new(CommandSessionOutputCursor::default()),
        child: Mutex::new(Some(child)),
        workspace,
        finalized: Mutex::new(None),
        timeout_deadline,
    });
    Ok(session)
}

/// Number of leading bytes to decode and consume now: everything except a
/// trailing *incomplete* multibyte sequence (≤3 bytes), which is carried to the
/// next read instead of being corrupted into replacement characters at the
/// 8 KiB read boundary (sense-2 §2.6). A genuinely *invalid* byte is consumed
/// (lossily decoded to U+FFFD), never carried — otherwise a single non-UTF-8
/// byte (binary output, a killed program dumping bytes) would wedge the carry
/// buffer and withhold all further model output until EOF. Pure
/// (platform-independent) so it is unit tested on every host.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn utf8_consumable_prefix_len(bytes: &[u8]) -> usize {
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(_) => return bytes.len(),
            // An incomplete trailing multibyte sequence carries to the next read.
            Err(err) if err.error_len().is_none() => return offset + err.valid_up_to(),
            // An invalid byte run is consumed (lossy); keep scanning the rest.
            Err(err) => {
                offset += err.valid_up_to() + err.error_len().unwrap_or(1);
            }
        }
    }
    bytes.len()
}

#[cfg(target_os = "linux")]
fn spawn_command_output_reader(
    mut master: File,
    output: Arc<CommandSessionOutput>,
    transcript_path: PathBuf,
) -> std_mpsc::Receiver<()> {
    let (done_tx, done_rx) = std_mpsc::channel();
    thread::spawn(move || {
        let mut transcript = OpenOptions::new()
            .create(true)
            .append(true)
            .open(transcript_path)
            .ok();
        let mut buf = [0_u8; 8192];
        // Carry-over buffer: holds an incomplete trailing multibyte sequence
        // until the next read completes it (§2.6).
        let mut carry: Vec<u8> = Vec::new();
        loop {
            match master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Transcript: byte-exact raw stream (decode-independent).
                    if output.note_spooled(u64::try_from(n).unwrap_or(u64::MAX)) {
                        if let Some(file) = transcript.as_mut() {
                            let _ = file.write_all(&buf[..n]);
                        }
                    }
                    // Model output: decode the consumable prefix, retain only an
                    // incomplete trailing multibyte tail.
                    carry.extend_from_slice(&buf[..n]);
                    let consume = utf8_consumable_prefix_len(&carry);
                    if consume > 0 {
                        output.append(String::from_utf8_lossy(&carry[..consume]).into_owned());
                        carry.drain(..consume);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        // EOF: flush any remaining (truly incomplete) bytes lossily.
        if !carry.is_empty() {
            output.append(String::from_utf8_lossy(&carry).into_owned());
        }
        let _ = done_tx.send(());
    });
    done_rx
}

#[cfg(target_os = "linux")]
fn finalize_isolated_command_workspace(
    session: &CommandSession,
    workspace: &IsolatedCommandWorkspace,
    runner: Option<&RunResult>,
    status: &str,
    exit_code: i64,
    stdout: &str,
    include_session_id: bool,
) -> Result<Value, DaemonError> {
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
        response["command_session_id"] = json!(session.id.clone());
    }
    std::fs::write(
        &workspace.final_path,
        serde_json::to_vec_pretty(&response)
            .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    let duration_s = session.started_at.elapsed().as_secs_f64();
    let duration_ms = duration_s * 1000.0;
    crate::isolated::record_tool_call(
        &workspace.handle.agent_id,
        json!({
            "workspace_handle_id": workspace.handle.workspace_handle_id.clone(),
            "exit_code": exit_code,
            "argv0": "bash",
            "status": status,
            "changed_paths": response["changed_paths"].clone(),
            "published": false,
            "command_session_id": session.id.clone(),
            "duration_s": duration_s,
            "total_ms": duration_ms,
            "phases_ms": {
                "exec": duration_ms,
            },
        }),
    );
    Ok(response)
}

#[cfg(target_os = "linux")]
fn finalize_command_workspace(
    session: &CommandSession,
    workspace: &EphemeralCommandWorkspace,
    status: &str,
    exit_code: i64,
    stdout: &str,
    include_session_id: bool,
) -> Result<Value, DaemonError> {
    let upperdir_stats = TreeResourceStats::collect(&workspace.upperdir);
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
    insert_tree_resource_timings(
        &mut timings,
        "resource.command_exec.upperdir",
        &upperdir_stats,
    );
    insert_occ_route_timings(&mut timings, route_metrics, route_s, occ_s);
    let mut response =
        guarded_changeset_response("exec_command", &changeset, timings, Instant::now(), None);
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
        response["command_session_id"] = json!(session.id);
    }
    std::fs::write(
        &workspace.final_path,
        serde_json::to_vec_pretty(&response)
            .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    Ok(response)
}

#[cfg(target_os = "linux")]
fn strip_session_id(mut response: Value) -> Value {
    if let Some(object) = response.as_object_mut() {
        object.remove("command_session_id");
    }
    response
}

#[cfg(target_os = "linux")]
fn response_with_stdout(mut response: Value, stdout: String) -> Value {
    response["output"]["stdout"] = json!(stdout);
    response["stdout"] = response["output"]["stdout"].clone();
    response
}

#[cfg(target_os = "linux")]
fn terminate_command_process_group(pgid: i32) {
    if killpg(Pid::from_raw(pgid), Signal::SIGTERM).is_ok() {
        thread::sleep(Duration::from_millis(50));
        let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
    }
}

/// How long `cancel`/exit-cleanup wait for the SIGKILLed child to exit so the
/// finalize (lease release + isolated unregister) runs inline.
#[cfg(target_os = "linux")]
const COMMAND_SESSION_CANCEL_WAIT_MS: u64 = 500;

#[cfg(target_os = "linux")]
fn command_session_write_stdin(args: &Value) -> Result<Value, DaemonError> {
    let id = require_string(args, "command_session_id")?;
    let chars = args
        .get("chars")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let yield_time_ms = optional_u64(args, "yield_time_ms").unwrap_or(1000);
    let max_tokens = optional_u64(args, "max_output_tokens");
    // sense-2 D7: `terminate` is the explicit teardown channel, decoupled from
    // `\x03` (which is SIGINT/interrupt only).
    let terminate = args
        .get("terminate")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let registry = command_session_registry();
    let Some(session) = registry.get(&id) else {
        // The live session is gone; a reaper-parked completion may remain.
        if let Some(result) = registry.take_completed_result(&id) {
            return Ok(result);
        }
        return Ok(command_session_not_found());
    };
    {
        let mut writer = lock_command_session_state(&session.writer);
        writer.write_all(chars.as_bytes())?;
    }
    // `\x03` interrupts the foreground program (SIGINT) only — teardown is a
    // separate concern (sense-2 D7).
    if chars.contains('\u{3}') {
        *lock_command_session_state(&session.interrupted) = true;
        let _ = killpg(Pid::from_raw(session.pgid), Signal::SIGINT);
    }
    // `terminate: true` tears the session down (SIGTERM→SIGKILL); `wait_for_yield`
    // then finalizes it inline with a `cancelled` status.
    if terminate {
        *lock_command_session_state(&session.cancelled) = true;
        terminate_command_process_group(session.pgid);
    }
    // Unified wait: early-return on completion (inline finalize) or
    // quiet-after-output, capped at `yield_time_ms` (sense-2 §2.3).
    match wait_for_yield(&session, yield_time_ms, max_tokens) {
        WaitOutcome::Completed(result) => Ok(result),
        WaitOutcome::Running(stdout) => Ok(command_result("running", None, &stdout, "", Some(id))),
    }
}

#[cfg(target_os = "linux")]
fn command_session_cancel(args: &Value) -> Result<Value, DaemonError> {
    let id = require_string(args, "command_session_id")?;
    let registry = command_session_registry();
    let Some(session) = registry.get(&id) else {
        if let Some(result) = registry.take_completed_result(&id) {
            return Ok(result);
        }
        return Ok(command_session_not_found());
    };
    *lock_command_session_state(&session.cancelled) = true;
    terminate_command_process_group(session.pgid);
    // Finalize inline so the lease/scratch is reclaimed and the cancelled status
    // is stamped; if the child is somehow still alive, the reaper finalizes it.
    match wait_for_yield(
        &session,
        COMMAND_SESSION_CANCEL_WAIT_MS,
        optional_u64(args, "max_output_tokens"),
    ) {
        WaitOutcome::Completed(result) => Ok(result),
        WaitOutcome::Running(stdout) => Ok(command_result("cancelled", None, &stdout, "", None)),
    }
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "isolated exit cleanup keeps the same fallible helper signature across cfgs"
)]
pub fn cancel_command_session_for_exit(id: &str) -> Result<bool, DaemonError> {
    let Some(session) = command_session_registry().get(id) else {
        crate::isolated::unregister_command_session_id(id);
        return Ok(false);
    };
    *lock_command_session_state(&session.cancelled) = true;
    terminate_command_process_group(session.pgid);
    // Reap promptly so `try_finalize` releases the session and (for isolated)
    // unregisters it before the namespace is torn down; the reaper is the
    // backstop if the child outlives the window.
    let deadline = Instant::now() + Duration::from_millis(COMMAND_SESSION_CANCEL_WAIT_MS);
    while session.try_finalize(false).is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
// Keep the same fallible public helper signature as Linux so isolated exit can
// call it without cfg-splitting the cleanup path.
#[expect(
    clippy::unnecessary_wraps,
    reason = "non-Linux parity keeps the Linux fallible helper signature"
)]
pub const fn cancel_command_session_for_exit(_id: &str) -> Result<bool, DaemonError> {
    Ok(false)
}

/// Wall-clock cap (seconds) for a session started WITHOUT an explicit `timeout`
/// (anchor §3). Without it, a fire-and-forget no-timeout command is unbounded in
/// both the runner and the reaper. Large default (6 h) — a safety net, not a
/// policy; override with `EOS_COMMAND_SESSION_MAX_S` (`0`/invalid → default).
#[cfg(target_os = "linux")]
fn command_session_max_seconds() -> u64 {
    static MAX_SECONDS: OnceLock<u64> = OnceLock::new();
    *MAX_SECONDS.get_or_init(|| {
        std::env::var("EOS_COMMAND_SESSION_MAX_S")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|seconds| *seconds > 0)
            .unwrap_or(6 * 60 * 60)
    })
}

/// Periodic reaper (sense-2 §2.4, §3): enforce the per-session timeout backstop
/// and finalize any session whose child has exited without a live poller,
/// parking the completion for the heartbeat. The runner enforces the per-call
/// timeout internally (primary); this is the backstop for a wedged or
/// no-timeout runner and the only finalizer for fire-and-forget sessions. A
/// session started without an explicit `timeout` falls back to the
/// `EOS_COMMAND_SESSION_MAX_S` wall-clock cap so it can never run forever.
#[cfg(target_os = "linux")]
pub fn command_session_reaper_sweep() {
    let now = Instant::now();
    for session in command_session_registry().live() {
        let deadline = session.timeout_deadline.unwrap_or_else(|| {
            session.started_at + Duration::from_secs(command_session_max_seconds())
        });
        if now > deadline {
            terminate_command_process_group(session.pgid);
        }
        let _ = session.try_finalize(true);
    }
}

/// Startup recovery (sense-2 §2.4): a previous daemon may have left ephemeral
/// command-session metadata behind. Park an `orphan_reaped` completion for each
/// so a recovering agent learns the session is dead, then remove the stale dir.
///
/// We deliberately do **not** `killpg` the old children: their pgids are not
/// persisted, so a restarted daemon could otherwise signal a reused PID. Their
/// own runner timeout reclaims them; lease cleanup is left to LayerStack GC.
#[cfg(target_os = "linux")]
pub fn recover_orphaned_command_sessions() {
    let Ok(runtime_root) = overlay_writable_root() else {
        return;
    };
    let dir = runtime_root.join("runtime").join("command-sessions");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Ok(bytes) = std::fs::read(path.join("metadata.json")) {
            if let Ok(meta) = serde_json::from_slice::<Value>(&bytes) {
                let id = meta
                    .get("command_session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !id.is_empty() {
                    let agent_id = meta
                        .get("agent_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let command = meta
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let result = command_result(
                        "error",
                        Some(1),
                        "",
                        "orphan_reaped: daemon restarted",
                        Some(id.to_owned()),
                    );
                    command_session_registry().push_completed(json!({
                        "command_session_id": id,
                        "agent_id": agent_id,
                        "command": command,
                        "result": result.clone(),
                        "notification_result": result,
                    }));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn command_session_reaper_sweep() {}

#[cfg(not(target_os = "linux"))]
pub fn recover_orphaned_command_sessions() {}

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
    fn utf8_carry_over_excludes_split_multibyte_tail() {
        // A 3-byte char (€ = E2 82 AC) split across a read boundary: the first
        // read ends mid-sequence, so only the complete prefix decodes; the tail
        // is carried over and completed on the next read.
        let euro = "€".as_bytes(); // [0xE2, 0x82, 0xAC]
        let mut first = b"ab".to_vec();
        first.extend_from_slice(&euro[..1]); // "ab" + first byte of €
        let consume = utf8_consumable_prefix_len(&first);
        assert_eq!(consume, 2, "the split multibyte tail is carried, not consumed");
        assert_eq!(&first[..consume], b"ab");

        // Completing the sequence makes the whole buffer consumable.
        let mut completed = first[consume..].to_vec();
        completed.extend_from_slice(&euro[1..]);
        assert_eq!(utf8_consumable_prefix_len(&completed), completed.len());
        assert_eq!(String::from_utf8_lossy(&completed), "€");

        // Fully valid input is consumed whole.
        assert_eq!(utf8_consumable_prefix_len(b"plain ascii"), 11);
    }

    #[test]
    fn utf8_consumable_prefix_consumes_invalid_bytes_so_the_buffer_never_wedges() {
        // A genuinely invalid byte (0xFF) mid-stream must be CONSUMED (lossily
        // decoded to U+FFFD), never carried — otherwise the carry buffer wedges
        // and withholds all further output until EOF.
        let invalid = [b'a', 0xFF, b'b'];
        assert_eq!(utf8_consumable_prefix_len(&invalid), 3);
        assert_eq!(String::from_utf8_lossy(&invalid), "a\u{FFFD}b");

        // An invalid byte followed by an incomplete multibyte lead: consume
        // through the invalid byte, carry only the trailing incomplete tail.
        let mut mixed = vec![0xFF];
        mixed.extend_from_slice(&"€".as_bytes()[..1]); // 0xFF then start of €
        assert_eq!(utf8_consumable_prefix_len(&mixed), 1);

        // A lone leading continuation byte (invalid start) is consumed, not held.
        assert_eq!(utf8_consumable_prefix_len(&[0x80]), 1);
    }

    #[test]
    fn optional_u64_accepts_unsigned_and_nonnegative_signed_numbers() {
        assert_eq!(optional_u64(&json!({"timeout": 7_u64}), "timeout"), Some(7));
        assert_eq!(optional_u64(&json!({"timeout": 7_i64}), "timeout"), Some(7));
        assert_eq!(optional_u64(&json!({"timeout": -1_i64}), "timeout"), None);
    }

    #[test]
    fn command_session_cancel_suppresses_background_completion_publication() {
        assert!(should_publish_command_session_completion(true, false, true));
        assert!(!should_publish_command_session_completion(true, true, true));
        assert!(!should_publish_command_session_completion(
            true, false, false
        ));
        assert!(!should_publish_command_session_completion(
            false, false, true
        ));
        assert!(!should_publish_command_session_completion(
            false, true, false
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn command_session_completion_result_can_be_claimed_by_control_tool() -> TestResult {
        let registry = CommandSessionRegistry::new();
        registry.push_completed(json!({
            "command_session_id": "cmd_keep",
            "result": {"status": "ok", "exit_code": 0},
        }));
        registry.push_completed(json!({
            "command_session_id": "cmd_done",
            "result": {"status": "ok", "exit_code": 0},
        }));

        let result = registry
            .take_completed_result("cmd_done")
            .ok_or("matching completion should be returned")?;
        assert_eq!(result["status"], "ok");
        assert!(registry.take_completed_result("cmd_done").is_none());

        let remaining = registry.collect_completed(&json!({"command_session_ids": ["cmd_keep"]}));
        assert_eq!(
            remaining["completions"]
                .as_array()
                .ok_or("completions should be an array")?
                .len(),
            1
        );

        // Remove-on-deliver: a second collect finds nothing — the map is bounded,
        // not accumulating delivered entries forever.
        let redelivered =
            registry.collect_completed(&json!({"command_session_ids": ["cmd_keep"]}));
        assert_eq!(
            redelivered["completions"]
                .as_array()
                .ok_or("completions should be an array")?
                .len(),
            0
        );
        Ok(())
    }

    /// A minimal live `CommandSession` for registry/count tests. The workspace is
    /// an empty isolated stub (never finalized here), so only `id`/`agent_id`
    /// matter. One constructor keeps the 16-field literal in a single place.
    #[cfg(target_os = "linux")]
    fn test_command_session(id: &str, agent_id: &str) -> TestResult<CommandSession> {
        let writer = Mutex::new(OpenOptions::new().read(true).write(true).open("/dev/null")?);
        Ok(CommandSession {
            id: id.to_owned(),
            agent_id: agent_id.to_owned(),
            command: "test".to_owned(),
            started_at: Instant::now(),
            pgid: 0,
            writer,
            output: Arc::new(CommandSessionOutput::new()),
            reader_done: Mutex::new(None),
            cancelled: Mutex::new(false),
            interrupted: Mutex::new(false),
            model_cursor: Mutex::new(CommandSessionOutputCursor::default()),
            notification_cursor: Mutex::new(CommandSessionOutputCursor::default()),
            child: Mutex::new(None),
            workspace: CommandWorkspaceKind::Isolated(IsolatedCommandWorkspace {
                handle: crate::isolated::CommandHandle {
                    agent_id: String::new(),
                    workspace_handle_id: String::new(),
                    layer_stack_root: PathBuf::new(),
                    manifest_version: 0,
                    manifest_root_hash: String::new(),
                    workspace_root: PathBuf::new(),
                    scratch_dir: PathBuf::new(),
                    upperdir: PathBuf::new(),
                    workdir: PathBuf::new(),
                    layer_paths: Vec::new(),
                    ns_fds: HashMap::new(),
                    cgroup_path: None,
                },
                output_path: PathBuf::new(),
                final_path: PathBuf::new(),
            }),
            finalized: Mutex::new(None),
            timeout_deadline: None,
        })
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn command_session_count_counts_live_sessions_by_agent() -> TestResult {
        let registry = CommandSessionRegistry::new();
        registry.insert(Arc::new(test_command_session("cmd_a", "agent-a")?));
        registry.insert(Arc::new(test_command_session("cmd_b", "agent-b")?));

        assert_eq!(registry.count_by_agent("agent-a"), 1);
        assert_eq!(registry.count_by_agent("agent-b"), 1);
        assert_eq!(registry.count_by_agent(""), 2);
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn command_session_write_stdin_returns_completed_result_when_live_session_is_gone() -> TestResult
    {
        let id = "cmd_stdin_done_unit";
        command_session_registry().push_completed(json!({
            "command_session_id": id,
            "result": {
                "status": "ok",
                "exit_code": 0,
                "output": {"stdout": "written\n", "stderr": ""},
            },
        }));

        let response =
            command_session_write_stdin(&json!({"command_session_id": id, "chars": "ignored"}))?;

        assert_eq!(response["status"], "ok");
        assert_eq!(response["output"]["stdout"], "written\n");
        assert!(command_session_registry()
            .take_completed_result(id)
            .is_none());
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn command_session_cancel_returns_completed_result_when_live_session_is_gone() -> TestResult {
        let id = "command_session_cancel_done_unit";
        command_session_registry().push_completed(json!({
            "command_session_id": id,
            "result": {
                "status": "ok",
                "exit_code": 0,
                "output": {"stdout": "already-finished\n", "stderr": ""},
            },
        }));

        let response = command_session_cancel(&json!({"command_session_id": id}))?;

        assert_eq!(response["status"], "ok");
        assert_eq!(response["output"]["stdout"], "already-finished\n");
        assert!(command_session_registry()
            .take_completed_result(id)
            .is_none());
        Ok(())
    }
}
