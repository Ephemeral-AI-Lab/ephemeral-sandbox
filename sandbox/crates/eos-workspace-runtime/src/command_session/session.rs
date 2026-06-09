// On non-Linux hosts this module is compiled only for scaffold unit tests; the
// real PTY-backed manager that uses these internals is Linux-only.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use serde_json::Value;

#[cfg(target_os = "linux")]
use crate::command_session::process::{
    CommandCompletionStatus, CommandRunnerResult, CommandSessionProcess, KillReason, ProcessReap,
};
#[cfg(target_os = "linux")]
use crate::command_session::transcript::{
    read_transcript_since, read_transcript_stdout, read_transcript_tail,
};
#[cfg(target_os = "linux")]
use crate::command_session::wait::CommandSessionWaitTarget;
#[cfg(target_os = "linux")]
use crate::command_session::CommandResponse;
#[cfg(target_os = "linux")]
use crate::command_session::CommandSessionError;

/// The raw, policy-free result of reaping a finished command process. The
/// substrate produces this; the owning workspace run turns it into a
/// `CommandResponse` by publishing (complete) or discarding (cancel). Keeping the
/// publish/discard decision out of the session is the structural guarantee that a
/// cancelled command never reaches the OCC merge.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub(crate) struct ReapedCommand {
    pub(crate) status: String,
    pub(crate) exit_code: i64,
    pub(crate) runner_result: Option<Value>,
    pub(crate) stdout: String,
    pub(crate) elapsed_s: f64,
    /// Why the substrate killed this session, if it did. `None` is a natural
    /// exit; `Some(_)` means a kill (cancel or timeout) and the owning run
    /// DISCARDS rather than publishes.
    pub(crate) kill: Option<KillReason>,
}

/// PTY/process substrate for one command session. It owns the child process,
/// the transcript, and the cancel flag — but **no** workspace policy: the run
/// that owns this session decides publish-vs-discard.
pub(crate) struct CommandSession {
    id: String,
    caller_id: String,
    command: String,
    #[cfg(target_os = "linux")]
    process: CommandSessionProcess,
    #[cfg(target_os = "linux")]
    output_path: PathBuf,
    #[cfg(target_os = "linux")]
    final_path: PathBuf,
    #[cfg(target_os = "linux")]
    transcript_path: PathBuf,
    /// Why this session was killed, if it has been. Set once by `cancel_process`
    /// (user cancel) or `time_out_process` (deadline backstop); a user cancel
    /// wins, so a cancelled session is never relabeled as timed-out.
    #[cfg(target_os = "linux")]
    kill: Mutex<Option<KillReason>>,
    #[cfg(target_os = "linux")]
    output_drain_grace_ms: u64,
    /// Reaped-once guard so two pollers can't both finalize the same child.
    #[cfg(target_os = "linux")]
    reaped: Mutex<bool>,
    started_at: Instant,
    timeout: Option<Duration>,
}

pub(crate) struct CommandSessionSpec {
    pub(crate) id: String,
    pub(crate) caller_id: String,
    pub(crate) command: String,
    pub(crate) timeout_seconds: Option<f64>,
}

#[cfg(target_os = "linux")]
pub(crate) struct RunningCommandSessionParts {
    pub(crate) process: CommandSessionProcess,
    pub(crate) output_path: PathBuf,
    pub(crate) final_path: PathBuf,
    pub(crate) transcript_path: PathBuf,
    pub(crate) output_drain_grace_ms: u64,
}

impl CommandSession {
    #[must_use]
    #[cfg(any(not(target_os = "linux"), test))]
    pub(crate) fn new(spec: CommandSessionSpec) -> Self {
        Self::new_scaffold(spec)
    }

    #[cfg(target_os = "linux")]
    #[must_use]
    pub(crate) fn new_running(spec: CommandSessionSpec, parts: RunningCommandSessionParts) -> Self {
        Self::new_with_process(spec, parts)
    }

    #[cfg(any(not(target_os = "linux"), test))]
    fn new_scaffold(spec: CommandSessionSpec) -> Self {
        #[cfg(target_os = "linux")]
        let inactive = inactive_process_parts();
        Self {
            id: spec.id,
            caller_id: spec.caller_id,
            command: spec.command,
            #[cfg(target_os = "linux")]
            process: inactive.process,
            #[cfg(target_os = "linux")]
            output_path: inactive.output_path,
            #[cfg(target_os = "linux")]
            final_path: inactive.final_path,
            #[cfg(target_os = "linux")]
            transcript_path: inactive.transcript_path,
            #[cfg(target_os = "linux")]
            kill: Mutex::new(None),
            #[cfg(target_os = "linux")]
            output_drain_grace_ms: inactive.output_drain_grace_ms,
            #[cfg(target_os = "linux")]
            reaped: Mutex::new(false),
            started_at: Instant::now(),
            timeout: spec.timeout_seconds.and_then(duration_from_secs_f64),
        }
    }

    #[cfg(target_os = "linux")]
    fn new_with_process(spec: CommandSessionSpec, running: RunningCommandSessionParts) -> Self {
        Self {
            id: spec.id,
            caller_id: spec.caller_id,
            command: spec.command,
            process: running.process,
            output_path: running.output_path,
            final_path: running.final_path,
            transcript_path: running.transcript_path,
            kill: Mutex::new(None),
            output_drain_grace_ms: running.output_drain_grace_ms,
            reaped: Mutex::new(false),
            started_at: Instant::now(),
            timeout: spec.timeout_seconds.and_then(duration_from_secs_f64),
        }
    }

    #[must_use]
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub(crate) fn caller_id(&self) -> &str {
        &self.caller_id
    }

    #[must_use]
    pub(crate) fn command(&self) -> &str {
        &self.command
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn write_process_stdin(&self, chars: &str) -> Result<(), CommandSessionError> {
        self.process.write_stdin(chars.as_bytes())?;
        Ok(())
    }

    /// Cancel at a caller's request (Ctrl-C/Ctrl-D, the cancel op, or run
    /// teardown): record the reason and kill the process group. A cancel always
    /// wins over a later timeout mark.
    #[cfg(target_os = "linux")]
    pub(crate) fn cancel_process(&self) {
        *lock(&self.kill) = Some(KillReason::Cancelled);
        self.process.terminate();
    }

    /// Kill a session that exceeded its deadline (the reaper backstop). Records
    /// `TimedOut` only if no kill reason is set yet, so a prior user cancel keeps
    /// its `Cancelled` label; either way the process group is killed.
    #[cfg(target_os = "linux")]
    pub(crate) fn time_out_process(&self) {
        {
            let mut kill = lock(&self.kill);
            if kill.is_none() {
                *kill = Some(KillReason::TimedOut);
            }
        }
        self.process.terminate();
    }

    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn read_recent_output(&self, last_n_lines: usize) -> String {
        read_transcript_tail(&self.transcript_path, last_n_lines)
    }

    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn read_output_since(&self, start_offset: u64) -> String {
        read_transcript_since(&self.transcript_path, start_offset)
    }

    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn transcript_len(&self) -> u64 {
        transcript_len(&self.transcript_path)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) const fn started_at(&self) -> Instant {
        self.started_at
    }

    #[cfg(any(not(target_os = "linux"), test))]
    #[must_use]
    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        self.timeout
            .is_some_and(|timeout| now.duration_since(self.started_at) >= timeout)
    }

    #[cfg(target_os = "linux")]
    #[must_use]
    pub(crate) fn is_past_deadline(&self, now: Instant, max_session_s: u64) -> bool {
        let timeout = self
            .timeout
            .unwrap_or_else(|| Duration::from_secs(max_session_s));
        now.duration_since(self.started_at) >= timeout
    }

    /// Reap the child if it has exited, returning the raw command result. Returns
    /// `None` while the process is still running or has already been reaped. This
    /// only reaps the substrate — it does not publish or discard; the owning run
    /// decides that from `ReapedCommand::kill`.
    #[cfg(target_os = "linux")]
    pub(crate) fn reap(&self) -> Option<ReapedCommand> {
        let mut reaped = lock(&self.reaped);
        if *reaped {
            return None;
        }
        let process_exit = match self.process.try_reap() {
            ProcessReap::Running => return None,
            ProcessReap::Exited(exit) => exit,
        };
        *reaped = true;
        drop(reaped);
        self.process.terminate();
        self.process
            .wait_for_reader_done(Duration::from_millis(self.output_drain_grace_ms));
        let runner = CommandRunnerResult::read_from_path(&self.output_path);
        let kill = *lock(&self.kill);
        let completion =
            CommandCompletionStatus::from_process_and_runner(process_exit, runner.as_ref(), kill);
        Some(ReapedCommand {
            status: completion.status().to_owned(),
            exit_code: completion.exit_code(),
            runner_result: runner.map(|runner| runner.value().clone()),
            stdout: self.final_stdout(),
            elapsed_s: self.started_at.elapsed().as_secs_f64(),
            kill,
        })
    }

    /// Persist the run's final response to `final_path` for crash recovery and
    /// remove the transcript. Best-effort: `final_path` is only a crash-recovery
    /// convenience, so a write failure does not undo the already-decided
    /// publish/discard or fail the operation.
    #[cfg(target_os = "linux")]
    pub(crate) fn persist_final(&self, response: &CommandResponse) {
        let _ = write_final_response(&self.final_path, response);
        self.remove_transcript_file();
    }

    #[cfg(target_os = "linux")]
    fn remove_transcript_file(&self) {
        if self.transcript_path.as_os_str().is_empty() {
            return;
        }
        let _ = std::fs::remove_file(&self.transcript_path);
    }

    #[cfg(target_os = "linux")]
    fn final_stdout(&self) -> String {
        read_transcript_stdout(&self.transcript_path)
    }
}

#[cfg(target_os = "linux")]
fn transcript_len(path: &Path) -> u64 {
    if path.as_os_str().is_empty() {
        return 0;
    }
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

#[cfg(target_os = "linux")]
fn write_final_response(
    path: &Path,
    response: &CommandResponse,
) -> Result<(), CommandSessionError> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&response.to_wire_value()).map_err(|error| {
        CommandSessionError::InvalidRequest(format!("serialize final command response: {error}"))
    })?;
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(target_os = "linux")]
impl CommandSessionWaitTarget<ReapedCommand> for CommandSession {
    fn try_finalize(&self) -> Option<ReapedCommand> {
        self.reap()
    }

    fn transcript_len(&self) -> u64 {
        Self::transcript_len(self)
    }

    fn read_output_since(&self, start_offset: u64) -> String {
        Self::read_output_since(self, start_offset)
    }
}

fn duration_from_secs_f64(seconds: f64) -> Option<Duration> {
    if seconds.is_finite() && seconds > 0.0 {
        Some(Duration::from_secs_f64(seconds))
    } else {
        None
    }
}

#[cfg(all(target_os = "linux", test))]
fn inactive_process_parts() -> RunningCommandSessionParts {
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .expect("open /dev/null for inactive command session process");
    RunningCommandSessionParts {
        process: CommandSessionProcess::inactive(writer),
        output_path: PathBuf::new(),
        final_path: PathBuf::new(),
        transcript_path: PathBuf::new(),
        output_drain_grace_ms: 0,
    }
}

#[cfg(target_os = "linux")]
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "../../tests/command_session/session_unit.rs"]
mod tests;
