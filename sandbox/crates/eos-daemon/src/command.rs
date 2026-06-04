//! Command-session operations for the daemon dispatcher.

#[cfg(any(target_os = "linux", test))]
mod output;
#[cfg(target_os = "linux")]
mod pty;
#[cfg(any(target_os = "linux", test))]
mod session;

#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::{mpsc as std_mpsc, Arc, Mutex, OnceLock};
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use nix::sys::signal::{killpg, Signal};
#[cfg(target_os = "linux")]
use nix::unistd::Pid;
use serde_json::{json, Value};

#[cfg(target_os = "linux")]
use eos_layerstack::{require_workspace_binding, LayerStack, Lease, WorkspaceBinding};
#[cfg(target_os = "linux")]
use eos_overlay::{capture_upperdir, overlay_writable_root};
#[cfg(target_os = "linux")]
use eos_protocol::Intent;
#[cfg(target_os = "linux")]
use eos_runner::{Fd, NsFds, RunMode, RunRequest, RunResult, ToolCall, WorkspaceRoot};

#[cfg(target_os = "linux")]
use output::{CommandSessionOutput, CommandSessionOutputCursor};
#[cfg(target_os = "linux")]
use pty::open_pty_pair;
#[cfg(target_os = "linux")]
use session::{
    command_session_registry, lock_command_session_state, wait_for_yield, CommandSession,
    WaitOutcome,
};

use crate::dispatcher::DispatchContext;
use crate::error::DaemonError;
#[cfg(target_os = "linux")]
use crate::occ_writer::{
    apply_occ_changeset, base_hashes_for_snapshot, insert_occ_route_timings, manifest_version_u64,
    occ_route_metrics,
};
#[cfg(target_os = "linux")]
use crate::overlay_runner::{overlay_daemon_error, overlay_run_dirs, RunDirCleanup};
use crate::response_timings::u64_to_f64_saturating;
#[cfg(target_os = "linux")]
use crate::response_timings::{
    guarded_changeset_response, insert_tree_resource_timings, layer_change_kind,
    merge_runner_timings, resource_timings, TreeResourceStats,
};

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
    command_session_registry().insert(Arc::clone(&session));
    crate::isolated::register_command_session(&session.agent_id, &session.id);
    match wait_for_yield(
        &session,
        yield_time_ms,
        optional_u64(args, "max_output_tokens"),
    ) {
        WaitOutcome::Completed(response) => Ok(strip_session_id(response)),
        WaitOutcome::Running(stdout) => Ok(command_result("running", None, &stdout, "", Some(id))),
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
            command_session_registry().insert(Arc::clone(&session));
            match wait_for_yield(
                &session,
                yield_time_ms,
                optional_u64(args, "max_output_tokens"),
            ) {
                WaitOutcome::Completed(response) => Ok(strip_session_id(response)),
                WaitOutcome::Running(stdout) => {
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
            verb: "exec_command".into(),
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
    let dirs = overlay_run_dirs("sandbox-overlay", &spec.invocation_id)?;
    let run_dir_cleanup = RunDirCleanup::new(dirs.run_dir.clone());
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
            verb: "exec_command".into(),
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
    let session = spawn_command_runner_session(spec, &request_path, transcript_path, workspace);
    if session.is_ok() {
        run_dir_cleanup.disarm();
    }
    session
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
    let (master, slave) = open_pty_pair()
        .map_err(|err| DaemonError::OverlayPipeline(format!("open pty pair: {err}")))?;
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
                    let consume = output::utf8_consumable_prefix_len(&carry);
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
    let total_s = session.started_at.elapsed().as_secs_f64();
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
    timings.insert("command_exec.total_s".to_owned(), json!(total_s));
    timings.insert(
        "api.exec_command.dispatch_total_s".to_owned(),
        json!(total_s),
    );
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
    let total_s = session.started_at.elapsed().as_secs_f64();
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
    response["timings"]["command_exec.total_s"] = json!(total_s);
    response["timings"]["api.exec_command.dispatch_total_s"] = json!(total_s);
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
/// Best-effort lifecycle backstop for callers that bypass the model-facing
/// `RequireNoBackgroundSessions` hook.
pub fn cleanup_command_sessions_for_agent(agent_id: &str, grace_s: Option<f64>) -> usize {
    let agent_id = agent_id.trim();
    if agent_id.is_empty() {
        return 0;
    }
    let sessions: Vec<Arc<CommandSession>> = command_session_registry()
        .live()
        .into_iter()
        .filter(|session| session.agent_id == agent_id)
        .collect();
    if sessions.is_empty() {
        return 0;
    }
    for session in &sessions {
        *lock_command_session_state(&session.cancelled) = true;
        terminate_command_process_group(session.pgid);
    }

    let wait_s = grace_s
        .unwrap_or(COMMAND_SESSION_CANCEL_WAIT_MS as f64 / 1000.0)
        .max(COMMAND_SESSION_CANCEL_WAIT_MS as f64 / 1000.0);
    let deadline = Instant::now() + Duration::from_secs_f64(wait_s);
    let mut pending = sessions.clone();
    loop {
        pending.retain(|session| session.try_finalize(true).is_none());
        if pending.is_empty() || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    for session in &pending {
        let _ = session.try_finalize(true);
    }
    sessions.len()
}

#[cfg(not(target_os = "linux"))]
pub const fn cleanup_command_sessions_for_agent(_agent_id: &str, _grace_s: Option<f64>) -> usize {
    0
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
#[path = "../tests/command/mod.rs"]
mod tests;
