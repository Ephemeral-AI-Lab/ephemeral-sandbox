//! One command process: the child process, PTY transcript, and kill/exit state.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use sandbox_runtime_namespace_process::runner::protocol::NamespaceCommandRequest;
use sandbox_runtime_workspace::{
    CgroupCleanupState, CgroupMonitorConfig, CgroupMonitorSample, WorkspaceEntry,
};
use serde_json::json;

pub use crate::pty::KillReason;

use crate::cgroup::{CommandCgroup, CommandCgroupTarget};
use crate::pty::{
    spawn_current_exe_ns_runner, CommandCompletionStatus, PtyProcess, PtyProcessExit,
};
use crate::transcript::{read_full_transcript_stdout, read_transcript_since};
use crate::yield_wait_loop::CommandWaitTarget;
use crate::{CommandConfig, CommandError};

const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(500);

/// PTY/process substrate for one command. It owns the child process, transcript,
/// and cancel flag, but no workspace policy: the run that owns this process
/// decides publish-vs-discard.
pub struct CommandProcess {
    id: String,
    command: String,
    started_at: Instant,
    runtime: CommandProcessRuntime,
}

pub struct CommandProcessSpec {
    pub id: String,
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub timeout_seconds: Option<f64>,
}

#[derive(Clone)]
pub struct CommandProcessSpawn {
    pub workspace_entry: WorkspaceEntry,
    pub transcript_path: PathBuf,
    cgroup: Option<CommandCgroup>,
    cgroup_monitor_config: CgroupMonitorConfig,
}

/// The raw, policy-free result of a finished command process. The substrate
/// produces this; the owning workspace run turns it into a
/// rendered operation response by publishing (complete) or discarding (cancel).
/// Keeping the publish/discard decision out of the process is the structural
/// guarantee that a cancelled command never reaches the OCC merge.
#[derive(Debug, Clone)]
pub struct CommandProcessExit {
    pub status: String,
    pub exit_code: i64,
    pub signal: Option<i32>,
    pub stdout: String,
    pub elapsed_s: f64,
    /// Why the substrate killed this process, if it did. `None` is a natural
    /// exit; `Some(_)` means a kill (cancel or timeout) and the owning run
    /// DISCARDS rather than publishes.
    pub kill: Option<KillReason>,
    pub cgroup_final_sample: Option<CgroupMonitorSample>,
    pub cgroup_cleanup: Option<CgroupCleanupState>,
}

/// Per-command process state: the child, its paths, and the kill/exit flags.
pub(crate) struct CommandProcessRuntime {
    process: PtyProcess,
    transcript_path: PathBuf,
    cgroup: Option<CommandCgroup>,
    cgroup_monitor_config: CgroupMonitorConfig,
    /// Why this process was killed, if it has been. Set once by `cancel_process`.
    kill: Mutex<Option<KillReason>>,
    /// Exit-taken guard so two pollers can't both finalize the same child.
    exit_taken: Mutex<bool>,
}

impl CommandProcessRuntime {
    pub(crate) fn new(
        process: PtyProcess,
        transcript_path: PathBuf,
        cgroup: Option<CommandCgroup>,
        cgroup_monitor_config: CgroupMonitorConfig,
    ) -> Self {
        Self {
            process,
            transcript_path,
            cgroup,
            cgroup_monitor_config,
            kill: Mutex::new(None),
            exit_taken: Mutex::new(false),
        }
    }

    fn artifact_dir(&self) -> PathBuf {
        self.transcript_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    /// `/dev/null`-backed runtime so scaffold processes can exist in tests
    /// without a live child.
    fn inactive() -> Self {
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .expect("open /dev/null for inactive command process");
        Self::new(
            PtyProcess::inactive(writer),
            PathBuf::new(),
            None,
            CgroupMonitorConfig::default(),
        )
    }
}

impl CommandProcess {
    #[doc(hidden)]
    #[must_use]
    pub fn inactive_for_test(spec: CommandProcessSpec) -> Self {
        Self::with_runtime(spec, CommandProcessRuntime::inactive())
    }

    #[doc(hidden)]
    #[must_use]
    pub fn inactive_with_process_group_for_test(spec: CommandProcessSpec, pgid: i32) -> Self {
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .expect("open /dev/null for inactive command process");
        Self::with_runtime(
            spec,
            CommandProcessRuntime::new(
                PtyProcess::inactive_with_process_group_for_test(writer, pgid),
                PathBuf::new(),
                None,
                CgroupMonitorConfig::default(),
            ),
        )
    }

    #[doc(hidden)]
    #[must_use]
    pub fn inactive_with_transcript_for_test(
        spec: CommandProcessSpec,
        transcript_path: PathBuf,
    ) -> Self {
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .expect("open /dev/null for inactive command process");
        Self::with_runtime(
            spec,
            CommandProcessRuntime::new(
                PtyProcess::inactive(writer),
                transcript_path,
                None,
                CgroupMonitorConfig::default(),
            ),
        )
    }

    pub fn spawn(
        spec: CommandProcessSpec,
        parts: CommandProcessSpawn,
    ) -> Result<Self, CommandError> {
        let command_request = build_namespace_command_request(&spec, parts.workspace_entry);
        let process = spawn_current_exe_ns_runner(&command_request, parts.transcript_path.clone())?;
        let process = process.allow_start().map_err(|error| {
            CommandError::artifact_write("process_start_ack", &parts.transcript_path, error)
        })?;
        Ok(Self::with_runtime(
            spec,
            CommandProcessRuntime::new(
                process,
                parts.transcript_path,
                parts.cgroup,
                parts.cgroup_monitor_config,
            ),
        ))
    }

    pub(crate) fn with_runtime(spec: CommandProcessSpec, runtime: CommandProcessRuntime) -> Self {
        Self {
            id: spec.id,
            command: spec.command,
            started_at: Instant::now(),
            runtime,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    #[must_use]
    pub fn process_group_id(&self) -> Option<i32> {
        self.runtime.process.process_group_id()
    }

    #[must_use]
    pub fn transcript_path(&self) -> Option<&Path> {
        if self.runtime.transcript_path.as_os_str().is_empty() {
            None
        } else {
            Some(&self.runtime.transcript_path)
        }
    }

    #[must_use]
    pub fn artifact_dir(&self) -> PathBuf {
        self.runtime.artifact_dir()
    }

    pub fn cleanup_artifacts_after_start_failure(&self) -> io::Result<()> {
        if let Some(cgroup) = self.runtime.cgroup.as_ref() {
            let cleanup = cgroup.cleanup();
            if let Some(error) = cleanup.last_cleanup_error {
                return Err(io::Error::other(error));
            }
        }
        let artifact_dir = self.artifact_dir();
        cleanup_artifacts_dir(&artifact_dir)
    }

    #[must_use]
    pub fn cgroup_target(&self) -> Option<CommandCgroupTarget> {
        self.runtime.cgroup.as_ref().map(CommandCgroup::target)
    }

    pub fn write_process_stdin(&self, chars: &str) -> Result<(), CommandError> {
        self.runtime.process.write_command_stdin(chars.as_bytes())?;
        Ok(())
    }

    /// Cancel at a caller's request (Ctrl-C/Ctrl-D, the cancel op, or run
    /// teardown): record the reason and kill the process group.
    pub fn cancel_process(&self) {
        *lock(&self.runtime.kill) = Some(KillReason::Cancelled);
        self.runtime.process.terminate();
    }

    #[must_use]
    pub fn read_output_since(&self, start_offset: u64) -> String {
        read_transcript_since(&self.runtime.transcript_path, start_offset)
    }

    #[must_use]
    pub fn transcript_len(&self) -> u64 {
        transcript_len(&self.runtime.transcript_path)
    }

    /// Take the child exit if it has completed, returning the raw command
    /// result. Returns `None` while the process is still running or the exit has
    /// already been taken. This only takes the process exit; it does not publish
    /// or discard. The owning run decides that from `CommandProcessExit::kill`.
    pub fn take_exit(&self) -> Option<CommandProcessExit> {
        let mut exit_taken = lock(&self.runtime.exit_taken);
        if *exit_taken {
            return None;
        }
        let process_exit = match self.runtime.process.take_exit() {
            PtyProcessExit::Running => return None,
            PtyProcessExit::Exited(exit) => exit,
        };
        *exit_taken = true;
        drop(exit_taken);
        self.runtime.process.terminate();
        self.runtime
            .process
            .wait_for_reader_done(OUTPUT_DRAIN_GRACE);
        let runner = self.runtime.process.take_runner_result(OUTPUT_DRAIN_GRACE);
        let cgroup_final_sample = self
            .runtime
            .cgroup
            .as_ref()
            .and_then(|cgroup| cgroup.final_sample(&self.runtime.cgroup_monitor_config));
        let cgroup_cleanup = self.runtime.cgroup.as_ref().map(CommandCgroup::cleanup);
        let kill = *lock(&self.runtime.kill);
        let completion =
            CommandCompletionStatus::from_process_and_runner(process_exit, runner.as_ref(), kill);
        Some(CommandProcessExit {
            status: completion.status().to_owned(),
            exit_code: completion.exit_code(),
            signal: process_exit.signal(),
            stdout: self.final_stdout(),
            elapsed_s: self.started_at.elapsed().as_secs_f64(),
            kill,
            cgroup_final_sample,
            cgroup_cleanup,
        })
    }

    fn final_stdout(&self) -> String {
        read_full_transcript_stdout(&self.runtime.transcript_path)
    }
}

impl CommandProcessSpawn {
    pub fn prepare(
        command_session_id: &str,
        workspace_entry: WorkspaceEntry,
        config: &CommandConfig,
    ) -> Result<Self, CommandError> {
        let command_dir = config.scratch_root.join(command_session_id);
        fs::create_dir_all(&command_dir).map_err(|error| {
            CommandError::artifact_write("command_artifact_directory", &command_dir, error)
        })?;
        let cgroup = CommandCgroup::prepare(
            command_session_id,
            workspace_entry.cgroup_path.as_deref(),
            &workspace_entry.upperdir,
        )?;
        let mut workspace_entry = workspace_entry;
        if let Some(cgroup) = cgroup.as_ref() {
            workspace_entry.cgroup_path = Some(cgroup.target().cgroup_path);
        }
        Ok(Self {
            workspace_entry,
            transcript_path: command_dir.join("transcript.log"),
            cgroup,
            cgroup_monitor_config: config.cgroup_monitor.clone(),
        })
    }

    #[must_use]
    pub fn cgroup_target(&self) -> Option<CommandCgroupTarget> {
        self.cgroup.as_ref().map(CommandCgroup::target)
    }

    #[must_use]
    pub fn artifact_dir(&self) -> PathBuf {
        self.transcript_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    pub fn cleanup_artifacts_after_start_failure(&self) -> io::Result<()> {
        if let Some(cgroup) = self.cgroup.as_ref() {
            let cleanup = cgroup.cleanup();
            if let Some(error) = cleanup.last_cleanup_error {
                return Err(io::Error::other(error));
            }
        }
        cleanup_artifacts_dir(&self.artifact_dir())
    }
}

pub(crate) fn build_namespace_command_request(
    spec: &CommandProcessSpec,
    entry: WorkspaceEntry,
) -> NamespaceCommandRequest {
    let cwd = spec
        .cwd
        .as_deref()
        .unwrap_or_else(|| Path::new("."))
        .to_string_lossy()
        .into_owned();
    NamespaceCommandRequest {
        request_id: spec.id.clone(),
        args: json!({
            "command": spec.command.clone(),
            "cwd": cwd,
        }),
        workspace_root: entry.workspace_root,
        layer_paths: entry.layer_paths,
        upperdir: Some(entry.upperdir),
        workdir: Some(entry.workdir),
        ns_fds: Some(entry.ns_fds.into()),
        cgroup_path: entry.cgroup_path,
        timeout_seconds: spec.timeout_seconds,
    }
}

impl CommandWaitTarget<CommandProcessExit> for CommandProcess {
    fn take_exit(&self) -> Option<CommandProcessExit> {
        self.take_exit()
    }

    fn transcript_len(&self) -> u64 {
        Self::transcript_len(self)
    }

    fn read_output_since(&self, start_offset: u64) -> String {
        Self::read_output_since(self, start_offset)
    }
}

fn transcript_len(path: &Path) -> u64 {
    if path.as_os_str().is_empty() {
        return 0;
    }
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn cleanup_artifacts_dir(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
