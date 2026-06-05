//! Linux command-session build & spawn lifecycle.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use eos_ephemeral_workspace::command_session::types::{
    EphemeralCommandPrepareContext, EphemeralCommandSessionPort,
};
use eos_ephemeral_workspace::{
    EphemeralRunDirs, EphemeralSnapshot, EphemeralWorkspaceError, EphemeralWorkspaceOps,
};
use eos_isolated_workspace::command_session::types::{
    IsolatedCommandPrepareContext, IsolatedCommandSessionPort,
};
use eos_isolated_workspace::IsolatedWorkspaceOps;
use eos_layerstack::{require_workspace_binding, LayerStack, WorkspaceBinding};
use eos_workspace_api::{CommandWorkspaceOps, PrepareCommandRequest, WorkspaceApiError};

use super::finalize::strip_session_id;
use super::output;
use super::output::{CommandSessionOutput, CommandSessionOutputCursor};
use super::pty::open_pty_pair;
use super::session::{command_session_registry, wait_for_yield, CommandSession, WaitOutcome};
use super::{command_result, command_session_config, optional_u64};
use crate::error::DaemonError;
use crate::services::overlay::{ephemeral_dir_allocator, RunDirCleanup};

pub(crate) struct EphemeralCommandWorkspace {
    pub(crate) root: PathBuf,
    pub(crate) lease_id: String,
    pub(crate) manifest_version: i64,
    pub(crate) manifest_root_hash: String,
    pub(crate) layer_paths: Vec<PathBuf>,
    pub(crate) workspace_root: PathBuf,
    pub(crate) dirs: eos_ephemeral_workspace::EphemeralRunDirs,
}

pub(crate) struct IsolatedCommandWorkspace {
    pub(crate) handle: crate::services::isolated_workspace::CommandHandle,
    pub(crate) output_path: PathBuf,
    pub(crate) final_path: PathBuf,
}

/// Which workspace a command session finalizes into (sense-2 §4). The notify
/// `publish` flag is orthogonal to this — both kinds can be parked.
pub(crate) enum CommandWorkspaceKind {
    /// Shared ephemeral overlay: finalize publishes via OCC and releases the
    /// per-session lease + run dir.
    Ephemeral(EphemeralCommandWorkspace),
    /// Isolated private workspace: finalize captures record-only; lease/scratch
    /// teardown is deferred to `exit_isolated_workspace`.
    Isolated(IsolatedCommandWorkspace),
}

impl CommandWorkspaceKind {
    /// The runner `--output` result file path (used by `try_finalize`).
    pub(crate) fn output_path(&self) -> &Path {
        match self {
            Self::Ephemeral(workspace) => &workspace.dirs.output_path,
            Self::Isolated(workspace) => &workspace.output_path,
        }
    }
}

struct CommandSessionStartSpec {
    id: String,
    invocation_id: String,
    agent_id: String,
    command: String,
    timeout_seconds: Option<f64>,
}

pub(crate) fn require_string(args: &Value, key: &str) -> Result<String, DaemonError> {
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

pub(crate) fn start_isolated_command_session(
    args: &Value,
    cmd: &str,
    timeout_seconds: Option<f64>,
    yield_time_ms: u64,
    handle: crate::services::isolated_workspace::CommandHandle,
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
    crate::services::isolated_workspace::register_command_session(&session.agent_id, &session.id);
    match wait_for_yield(
        &session,
        yield_time_ms,
        optional_u64(args, "max_output_tokens"),
    ) {
        WaitOutcome::Completed(response) => Ok(strip_session_id(response)),
        WaitOutcome::Running(stdout) => Ok(command_result("running", None, &stdout, "", Some(id))),
    }
}

fn prepare_request(spec: &CommandSessionStartSpec) -> PrepareCommandRequest {
    PrepareCommandRequest {
        agent_id: spec.agent_id.clone(),
        command_session_id: spec.id.clone(),
        invocation_id: spec.invocation_id.clone(),
        cmd: spec.command.clone(),
        timeout_seconds: spec.timeout_seconds,
    }
}

fn command_workspace_error(error: WorkspaceApiError) -> DaemonError {
    DaemonError::InvalidEnvelope(error.to_string())
}

fn workspace_api_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("daemon_command_workspace_error", error.to_string())
}

struct EphemeralCommandPreparePort<'a> {
    root: &'a Path,
    binding: &'a WorkspaceBinding,
    session_dir: PathBuf,
    final_path: PathBuf,
}

impl EphemeralCommandSessionPort for EphemeralCommandPreparePort<'_> {
    fn prepare_context(&self) -> Result<EphemeralCommandPrepareContext, WorkspaceApiError> {
        Ok(EphemeralCommandPrepareContext {
            layer_stack_root: self.root.to_path_buf(),
            workspace_root: PathBuf::from(&self.binding.workspace_root),
            writable_root: ephemeral_dir_allocator()
                .map_err(workspace_api_error)?
                .writable_root,
            session_dir: self.session_dir.clone(),
            final_path: self.final_path.clone(),
        })
    }

    fn acquire_snapshot(
        &self,
        request_id: &str,
    ) -> Result<EphemeralSnapshot, EphemeralWorkspaceError> {
        let lease = LayerStack::open(self.root.to_path_buf())
            .and_then(|mut stack| stack.acquire_snapshot(request_id))
            .map_err(|error| EphemeralWorkspaceError::SnapshotAcquire {
                reason: error.to_string(),
            })?;
        let snapshot = EphemeralSnapshot {
            lease_id: lease.lease_id,
            manifest_version: lease.manifest_version,
            manifest_root_hash: lease.root_hash,
            layer_paths: lease.layer_paths.into_iter().map(PathBuf::from).collect(),
        };
        Ok(snapshot)
    }

    fn release_snapshot(&self, lease_id: &str) -> Result<(), EphemeralWorkspaceError> {
        LayerStack::open(self.root.to_path_buf())
            .and_then(|mut stack| stack.release_lease(lease_id))
            .map(|_| ())
            .map_err(|error| EphemeralWorkspaceError::LeaseRelease {
                lease_id: lease_id.to_owned(),
                reason: error.to_string(),
            })
    }
}

struct IsolatedCommandPreparePort {
    handle: crate::services::isolated_workspace::CommandHandle,
}

impl IsolatedCommandSessionPort for IsolatedCommandPreparePort {
    fn prepare_context(&self) -> Result<IsolatedCommandPrepareContext, WorkspaceApiError> {
        Ok(IsolatedCommandPrepareContext {
            workspace_handle_id: self.handle.workspace_handle_id.clone(),
            workspace_root: self.handle.workspace_root.clone(),
            scratch_dir: self.handle.scratch_dir.clone(),
            layer_paths: self.handle.layer_paths.clone(),
            upperdir: self.handle.upperdir.clone(),
            workdir: self.handle.workdir.clone(),
            ns_fds: self.handle.ns_fds.clone(),
            cgroup_path: self.handle.cgroup_path.clone(),
        })
    }
}

pub(crate) fn start_command_session(
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
    let spec = CommandSessionStartSpec {
        id: command_session_registry().next_id(),
        invocation_id,
        agent_id,
        command: cmd.to_owned(),
        timeout_seconds,
    };
    let id = spec.id.clone();
    match prepare_command_session(&root, &binding, &spec) {
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
        Err(err) => Err(err),
    }
}

fn prepare_isolated_command_session(
    spec: &CommandSessionStartSpec,
    handle: crate::services::isolated_workspace::CommandHandle,
) -> Result<Arc<CommandSession>, DaemonError> {
    let prepared = IsolatedWorkspaceOps::new(IsolatedCommandPreparePort {
        handle: handle.clone(),
    })
    .prepare_command_workspace(prepare_request(spec))
    .map_err(command_workspace_error)?;
    let session_dir = prepared
        .finalize_context
        .get("session_dir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            DaemonError::InvalidEnvelope("missing isolated command session_dir".to_owned())
        })?;
    let transcript_path = session_dir.join("transcript.log");
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
    write_run_request(&prepared.request_path, &prepared.run_request)?;
    let workspace = CommandWorkspaceKind::Isolated(IsolatedCommandWorkspace {
        handle,
        output_path: prepared.output_path,
        final_path: prepared.final_path,
    });
    spawn_command_runner_session(spec, &prepared.request_path, transcript_path, workspace)
}

fn prepare_command_session(
    root: &Path,
    binding: &WorkspaceBinding,
    spec: &CommandSessionStartSpec,
) -> Result<Arc<CommandSession>, DaemonError> {
    let session_root = command_session_scratch_root();
    let session_dir = session_root.join(&spec.id);
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
    let prepared = EphemeralWorkspaceOps::new(EphemeralCommandPreparePort {
        root,
        binding,
        session_dir,
        final_path,
    })
    .prepare_command_workspace(prepare_request(spec))
    .map_err(command_workspace_error)?;
    let snapshot: EphemeralSnapshot = serde_json::from_value(
        prepared
            .finalize_context
            .get("snapshot")
            .cloned()
            .ok_or_else(|| DaemonError::InvalidEnvelope("missing command snapshot".to_owned()))?,
    )
    .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?;
    let mut lease_cleanup = LeaseCleanup::new(root.to_path_buf(), snapshot.lease_id.clone());
    let dirs: EphemeralRunDirs = serde_json::from_value(
        prepared
            .finalize_context
            .get("dirs")
            .cloned()
            .ok_or_else(|| DaemonError::InvalidEnvelope("missing command dirs".to_owned()))?,
    )
    .map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?;
    let mut run_dir_cleanup = RunDirCleanup::new(dirs.run_dir.clone());
    write_run_request(&prepared.request_path, &prepared.run_request)?;
    let workspace = CommandWorkspaceKind::Ephemeral(EphemeralCommandWorkspace {
        root: root.to_path_buf(),
        lease_id: snapshot.lease_id,
        manifest_version: snapshot.manifest_version,
        manifest_root_hash: snapshot.manifest_root_hash,
        layer_paths: snapshot.layer_paths,
        workspace_root: PathBuf::from(&binding.workspace_root),
        dirs,
    });
    let session =
        spawn_command_runner_session(spec, &prepared.request_path, transcript_path, workspace);
    if session.is_ok() {
        run_dir_cleanup.disarm();
        lease_cleanup.disarm();
    }
    session
}

struct LeaseCleanup {
    root: PathBuf,
    lease_id: Option<String>,
}

impl LeaseCleanup {
    fn new(root: PathBuf, lease_id: String) -> Self {
        Self {
            root,
            lease_id: Some(lease_id),
        }
    }

    fn disarm(&mut self) {
        self.lease_id = None;
    }
}

impl Drop for LeaseCleanup {
    fn drop(&mut self) {
        if let Some(lease_id) = self.lease_id.take() {
            let _ = LayerStack::open(self.root.clone())
                .and_then(|mut stack| stack.release_lease(&lease_id));
        }
    }
}

fn write_run_request(path: &Path, request: &Value) -> Result<(), DaemonError> {
    std::fs::write(
        path,
        serde_json::to_vec(request).map_err(|err| DaemonError::InvalidEnvelope(err.to_string()))?,
    )?;
    Ok(())
}

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

pub(crate) fn command_session_scratch_root() -> PathBuf {
    command_session_config().scratch_root
}
