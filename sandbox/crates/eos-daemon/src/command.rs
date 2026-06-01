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
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex, OnceLock};
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
use eos_layerstack::{require_workspace_binding, LayerStack};
#[cfg(target_os = "linux")]
use eos_overlay::{allocate_overlay_writable_dirs, capture_upperdir, overlay_writable_root};
#[cfg(target_os = "linux")]
use eos_protocol::Intent;
#[cfg(target_os = "linux")]
use eos_runner::{RunMode, RunRequest, RunResult, ToolCall, WorkspaceRoot};

#[cfg(target_os = "linux")]
use crate::dispatcher::{
    apply_occ_changeset, base_hashes_for_snapshot, guarded_changeset_response,
    insert_occ_route_timings, layer_change_kind, occ_route_metrics, overlay_daemon_error,
    resource_timings,
};
use crate::dispatcher::{run_shell_overlay, DispatchContext};
use crate::error::DaemonError;

/// `api.v1.exec_command` — final Phase 3T command contract.
pub(crate) fn op_exec_command(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let cmd = require_command_string(args, "cmd")?;
    let tty = args.get("tty").and_then(Value::as_bool).unwrap_or(false);
    let timeout_seconds = optional_u64(args, "timeout")
        .or_else(|| optional_u64(args, "timeout_seconds"))
        .map(|value| value as f64);
    if !tty {
        let mut shell_args = args.clone();
        shell_args["command"] = json!(cmd);
        shell_args["cwd"] = json!(".");
        if let Some(timeout) = timeout_seconds {
            shell_args["timeout_seconds"] = json!(timeout);
        }
        let shell = run_shell_overlay(&shell_args, Instant::now())?;
        return Ok(command_response_from_shell(&shell, None));
    }

    #[cfg(target_os = "linux")]
    {
        let yield_time_ms = optional_u64(args, "yield_time_ms").unwrap_or(1000);
        return start_pty_command(args, cmd, timeout_seconds, yield_time_ms);
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

pub(crate) fn op_pty_write_stdin(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        return pty_write_stdin(args);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(pty_not_found())
    }
}

pub(crate) fn op_pty_progress(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        return pty_progress(args);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(pty_not_found())
    }
}

pub(crate) fn op_pty_cancel(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        return pty_cancel(args);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(pty_not_found())
    }
}

pub(crate) fn op_pty_collect_completed(
    args: &Value,
    _context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    #[cfg(target_os = "linux")]
    {
        return Ok(pty_registry().collect_completed(args));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(json!({"success": true, "completions": []}))
    }
}

fn require_command_string(args: &Value, key: &str) -> Result<String, DaemonError> {
    let value = require_string(args, key)?;
    if value.trim().is_empty() {
        return Err(DaemonError::InvalidEnvelope(format!(
            "{key} must be non-empty"
        )));
    }
    Ok(value)
}

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
            .or_else(|| value.as_i64().filter(|v| *v >= 0).map(|v| v as u64))
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

#[cfg(target_os = "linux")]
const PTY_RING_MAX_BYTES: usize = 1024 * 1024;
#[cfg(target_os = "linux")]
const PTY_SPOOL_MAX_BYTES: u64 = 32 * 1024 * 1024;

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
    fn new() -> Self {
        Self {
            chunks: Mutex::new(VecDeque::new()),
            bytes: Mutex::new(0),
            spool_bytes: Mutex::new(0),
            spool_truncated: Mutex::new(false),
        }
    }

    fn append(&self, text: String) {
        let byte_len = text.len();
        let mut chunks = self.chunks.lock().expect("pty output ring poisoned");
        let mut bytes = self.bytes.lock().expect("pty output bytes poisoned");
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
    }

    fn recent_since(&self, since: Instant, max_tokens: Option<u64>) -> String {
        let chunks = self.chunks.lock().expect("pty output ring poisoned");
        bounded_output(
            chunks
                .iter()
                .filter(|chunk| chunk.at >= since)
                .map(|chunk| chunk.text.as_str()),
            max_tokens,
        )
    }

    fn all_recent(&self, max_tokens: Option<u64>) -> String {
        let chunks = self.chunks.lock().expect("pty output ring poisoned");
        bounded_output(chunks.iter().map(|chunk| chunk.text.as_str()), max_tokens)
    }

    fn note_spooled(&self, bytes: u64) -> bool {
        let mut spool_bytes = self.spool_bytes.lock().expect("pty spool bytes poisoned");
        if *spool_bytes >= PTY_SPOOL_MAX_BYTES {
            *self
                .spool_truncated
                .lock()
                .expect("pty spool truncation flag poisoned") = true;
            return false;
        }
        *spool_bytes = (*spool_bytes + bytes).min(PTY_SPOOL_MAX_BYTES);
        true
    }

    fn spool_truncated(&self) -> bool {
        *self
            .spool_truncated
            .lock()
            .expect("pty spool truncation flag poisoned")
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
        self.sessions
            .lock()
            .expect("pty registry poisoned")
            .insert(session.id.clone(), session);
    }

    fn get(&self, id: &str) -> Option<Arc<PtySession>> {
        self.sessions
            .lock()
            .expect("pty registry poisoned")
            .get(id)
            .cloned()
    }

    fn remove(&self, id: &str) -> Option<Arc<PtySession>> {
        self.sessions
            .lock()
            .expect("pty registry poisoned")
            .remove(id)
    }

    fn push_completed(&self, completion: Value) {
        self.completed
            .lock()
            .expect("pty completion mailbox poisoned")
            .push(completion);
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
        let mut completed = self
            .completed
            .lock()
            .expect("pty completion mailbox poisoned");
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
fn start_pty_command(
    args: &Value,
    cmd: String,
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
    let id = pty_registry().next_id();
    let prepare_result: Result<(PtyWorkspace, Arc<PtySession>, std::process::Child), DaemonError> =
        (|| {
            let runtime_root = overlay_writable_root()
                .map_err(|err| overlay_daemon_error("overlay writable root", err))?
                .join("runtime");
            let run_root = runtime_root.join("sandbox-overlay").join(format!(
                "{}-{}",
                std::process::id(),
                sanitize_path_component(&invocation_id)
            ));
            let dirs = allocate_overlay_writable_dirs(&run_root)
                .map_err(|err| overlay_daemon_error("allocate overlay dirs", err))?;
            let session_dir = runtime_root.join("pty-sessions").join(&id);
            std::fs::create_dir_all(&session_dir)?;
            let transcript_path = session_dir.join("transcript.log");
            let final_path = session_dir.join("final.json");
            std::fs::write(
                session_dir.join("metadata.json"),
                serde_json::to_vec_pretty(&json!({
                    "pty_session_id": id.clone(),
                    "agent_id": agent_id.clone(),
                    "invocation_id": invocation_id.clone(),
                    "command": cmd.clone(),
                    "status": "running",
                }))
                .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
            )?;
            let output_path = dirs.run_dir.join("pty-runner-result.json");
            let request_path = dirs.run_dir.join("pty-runner-request.json");
            let request = RunRequest {
                mode: RunMode::FreshNs,
                tool_call: ToolCall {
                    invocation_id: invocation_id.clone(),
                    agent_id: agent_id.clone(),
                    verb: "exec_command".to_owned(),
                    intent: Intent::WriteAllowed,
                    args: json!({
                        "command": cmd.clone(),
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
                timeout_seconds,
            };
            std::fs::write(
                &request_path,
                serde_json::to_vec(&request)
                    .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
            )?;

            let pty = openpty(None, None).map_err(|err| {
                DaemonError::Ephemeral(eos_ephemeral::EphemeralError::Overlay(format!(
                    "open pty: {err}"
                )))
            })?;
            let master: File = pty.master.into();
            let slave: File = pty.slave.into();
            let mut child_command = Command::new(std::env::current_exe()?);
            child_command
                .arg("ns-runner")
                .arg("--request")
                .arg(&request_path)
                .arg("--output")
                .arg(&output_path)
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave))
                .process_group(0);
            let child = child_command.spawn()?;
            let pgid = child.id() as i32;
            let output = Arc::new(PtyOutput::new());
            let session = Arc::new(PtySession {
                id: id.clone(),
                agent_id: agent_id.clone(),
                command: cmd.clone(),
                pgid,
                writer: Mutex::new(master.try_clone()?),
                output: Arc::clone(&output),
                cancelled: Mutex::new(false),
            });
            spawn_pty_reader(master, output, transcript_path);
            Ok((
                PtyWorkspace {
                    root: root.clone(),
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
        })();

    match prepare_result {
        Ok((workspace, session, mut child)) => {
            let deadline = Instant::now() + Duration::from_millis(yield_time_ms);
            while Instant::now() < deadline {
                if child.try_wait()?.is_some() {
                    let response = finish_pty_session(session, child, workspace, false);
                    return Ok(strip_session_id(response));
                }
                thread::sleep(Duration::from_millis(5));
            }
            if child.try_wait()?.is_some() {
                let response = finish_pty_session(session, child, workspace, false);
                return Ok(strip_session_id(response));
            }
            let stdout = session.output.all_recent(None);
            pty_registry().insert(Arc::clone(&session));
            thread::spawn(move || {
                let _ = finish_pty_session(session, child, workspace, true);
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
                    if output.note_spooled(n as u64) {
                        if let Some(file) = transcript.as_mut() {
                            let _ = file.write_all(&buf[..n]);
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

#[cfg(target_os = "linux")]
fn finish_pty_session(
    session: Arc<PtySession>,
    mut child: std::process::Child,
    workspace: PtyWorkspace,
    publish_completion: bool,
) -> Value {
    let status = child.wait();
    terminate_pty_process_group(session.pgid);
    let runner = std::fs::read(&workspace.output_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RunResult>(&bytes).ok());
    let mut exit_code = runner
        .as_ref()
        .map(|result| result.exit_code as i64)
        .or_else(|| {
            status.ok().map(|status| {
                status
                    .code()
                    .or_else(|| status.signal().map(|signal| -signal))
                    .unwrap_or(1) as i64
            })
        })
        .unwrap_or(1);
    let mut command_status = runner
        .as_ref()
        .and_then(|result| result.tool_result.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("error")
        .to_owned();
    if *session
        .cancelled
        .lock()
        .expect("pty cancelled flag poisoned")
    {
        command_status = "cancelled".to_owned();
        if exit_code == 0 {
            exit_code = 130;
        }
    }
    let response = finalize_pty_workspace(
        &session,
        &workspace,
        &command_status,
        exit_code,
        publish_completion,
    )
    .unwrap_or_else(|err| {
        command_result(
            "error",
            Some(exit_code),
            &session.output.all_recent(None),
            &err.to_string(),
            Some(session.id.clone()),
        )
    });
    let _ = std::fs::remove_dir_all(&workspace.run_dir);
    let _ = LayerStack::open(workspace.root.clone())
        .and_then(|mut stack| stack.release_lease(&workspace.lease_id));
    let _ = pty_registry().remove(&session.id);
    if publish_completion {
        pty_registry().push_completed(json!({
            "pty_session_id": session.id,
            "agent_id": session.agent_id,
            "command": session.command,
            "result": response,
        }));
    }
    response
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
        .map_err(|err| overlay_daemon_error("capture upperdir", err))?;
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
        Some(workspace.manifest_version as u64),
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
    let Some(session) = pty_registry().get(&id) else {
        return Ok(pty_not_found());
    };
    let since = Instant::now();
    session
        .writer
        .lock()
        .expect("pty writer poisoned")
        .write_all(chars.as_bytes())?;
    thread::sleep(Duration::from_millis(yield_time_ms));
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
    let Some(session) = pty_registry().get(&id) else {
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
    let Some(session) = pty_registry().remove(&id) else {
        return Ok(pty_not_found());
    };
    *session
        .cancelled
        .lock()
        .expect("pty cancelled flag poisoned") = true;
    let _ = killpg(Pid::from_raw(session.pgid), Signal::SIGTERM);
    thread::sleep(Duration::from_millis(50));
    Ok(command_result(
        "cancelled",
        None,
        &session.output.all_recent(optional_u64(args, "max_tokens")),
        "",
        None,
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn exec_command_requires_string_wire_shape() {
        assert!(require_command_string(&json!({"cmd": "echo hi"}), "cmd").is_ok());
        assert!(require_command_string(&json!({"cmd": ["true"]}), "cmd").is_err());
    }
}
