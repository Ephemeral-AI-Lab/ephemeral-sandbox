#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use eos_workspace_api::FinalizeCommandRequest;
use serde_json::Value;

#[cfg(target_os = "linux")]
use crate::process::{
    CommandCompletionStatus, CommandRunnerResult, CommandSessionProcess, ProcessReap,
};
#[cfg(target_os = "linux")]
use crate::transcript::{read_transcript_since, read_transcript_stdout, read_transcript_tail};
#[cfg(target_os = "linux")]
use crate::wait::CommandSessionWaitTarget;
#[cfg(any(not(target_os = "linux"), test))]
use crate::CommandSessionConfig;
use crate::{CommandResponse, CommandSessionError, DynCommandWorkspacePolicy};

pub(crate) struct CommandSession {
    id: String,
    caller_id: String,
    command: String,
    policy: Mutex<Option<DynCommandWorkspacePolicy>>,
    #[cfg(target_os = "linux")]
    process: CommandSessionProcess,
    #[cfg(target_os = "linux")]
    output_path: PathBuf,
    #[cfg(target_os = "linux")]
    final_path: PathBuf,
    #[cfg(target_os = "linux")]
    transcript_path: PathBuf,
    #[cfg(target_os = "linux")]
    cancelled: Mutex<bool>,
    #[cfg(target_os = "linux")]
    output_drain_grace_ms: u64,
    finalized: Mutex<Option<CommandResponse>>,
    started_at: Instant,
    timeout: Option<Duration>,
}

pub(crate) struct CommandSessionSpec {
    pub id: String,
    pub caller_id: String,
    pub command: String,
    pub timeout_seconds: Option<f64>,
}

#[cfg(target_os = "linux")]
pub(crate) struct RunningCommandSessionParts {
    pub process: CommandSessionProcess,
    pub output_path: PathBuf,
    pub final_path: PathBuf,
    pub transcript_path: PathBuf,
    pub output_drain_grace_ms: u64,
}

impl CommandSession {
    #[must_use]
    #[cfg(any(not(target_os = "linux"), test))]
    pub(crate) fn new(
        spec: CommandSessionSpec,
        policy: DynCommandWorkspacePolicy,
        config: &CommandSessionConfig,
    ) -> Self {
        Self::new_scaffold(spec, policy, config.output_drain_grace_ms)
    }

    #[cfg(target_os = "linux")]
    #[must_use]
    pub(crate) fn new_running(
        spec: CommandSessionSpec,
        policy: DynCommandWorkspacePolicy,
        parts: RunningCommandSessionParts,
    ) -> Self {
        Self::new_with_process(spec, policy, parts)
    }

    #[cfg(any(not(target_os = "linux"), test))]
    fn new_scaffold(
        spec: CommandSessionSpec,
        policy: DynCommandWorkspacePolicy,
        _output_drain_grace_ms: u64,
    ) -> Self {
        #[cfg(target_os = "linux")]
        let inactive = inactive_process_parts(_output_drain_grace_ms);
        Self {
            id: spec.id,
            caller_id: spec.caller_id,
            command: spec.command,
            policy: Mutex::new(Some(policy)),
            #[cfg(target_os = "linux")]
            process: inactive.process,
            #[cfg(target_os = "linux")]
            output_path: inactive.output_path,
            #[cfg(target_os = "linux")]
            final_path: inactive.final_path,
            #[cfg(target_os = "linux")]
            transcript_path: inactive.transcript_path,
            #[cfg(target_os = "linux")]
            cancelled: Mutex::new(false),
            #[cfg(target_os = "linux")]
            output_drain_grace_ms: inactive.output_drain_grace_ms,
            finalized: Mutex::new(None),
            started_at: Instant::now(),
            timeout: spec.timeout_seconds.and_then(duration_from_secs_f64),
        }
    }

    #[cfg(target_os = "linux")]
    fn new_with_process(
        spec: CommandSessionSpec,
        policy: DynCommandWorkspacePolicy,
        running: RunningCommandSessionParts,
    ) -> Self {
        Self {
            id: spec.id,
            caller_id: spec.caller_id,
            command: spec.command,
            policy: Mutex::new(Some(policy)),
            process: running.process,
            output_path: running.output_path,
            final_path: running.final_path,
            transcript_path: running.transcript_path,
            cancelled: Mutex::new(false),
            output_drain_grace_ms: running.output_drain_grace_ms,
            finalized: Mutex::new(None),
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
    #[must_use]
    pub(crate) fn is_cancelled(&self) -> bool {
        *lock(&self.cancelled)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn write_process_stdin(&self, chars: &str) -> Result<(), CommandSessionError> {
        self.process.write_stdin(chars.as_bytes())?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn cancel_process(&self) {
        *lock(&self.cancelled) = true;
        self.process.terminate();
    }

    #[must_use]
    pub(crate) fn read_recent_output(&self, last_n_lines: usize) -> String {
        #[cfg(target_os = "linux")]
        {
            read_transcript_tail(&self.transcript_path, last_n_lines)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = last_n_lines;
            String::new()
        }
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

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn finalize(
        &self,
        status: &str,
        exit_code: Option<i64>,
        include_session_id: bool,
    ) -> Result<CommandResponse, CommandSessionError> {
        self.finalize_with_output(status, exit_code, None, String::new(), include_session_id)
    }

    fn finalize_with_output(
        &self,
        status: &str,
        exit_code: Option<i64>,
        runner_result: Option<Value>,
        stdout: String,
        include_session_id: bool,
    ) -> Result<CommandResponse, CommandSessionError> {
        let mut finalized = lock(&self.finalized);
        if let Some(response) = finalized.as_ref() {
            return Ok(response.clone());
        }
        let policy = lock(&self.policy);
        let policy = policy.as_ref().ok_or_else(|| {
            CommandSessionError::Unsupported("command session has no workspace policy".to_owned())
        })?;
        let outcome = policy.finalize_command_workspace(FinalizeCommandRequest {
            runner_result,
            command_elapsed_s: self.started_at.elapsed().as_secs_f64(),
            status: status.to_owned(),
            exit_code,
            stdout,
            stderr: String::new(),
            command_session_id: include_session_id.then(|| self.id.clone()),
        })?;
        let response = CommandResponse::from_workspace_outcome(outcome);
        *finalized = Some(response.clone());
        Ok(response)
    }

    pub(crate) fn command_session_finished(&self, status: &str) {
        let policy = lock(&self.policy);
        if let Some(policy) = policy.as_ref() {
            policy.command_session_finished(&self.id, &self.caller_id, status);
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn try_finalize_process(
        &self,
    ) -> Option<Result<CommandResponse, CommandSessionError>> {
        let process_exit = match self.process.try_reap() {
            ProcessReap::Running => return None,
            ProcessReap::Exited(exit) => exit,
        };
        self.process.terminate();
        self.process
            .wait_for_reader_done(Duration::from_millis(self.output_drain_grace_ms));
        let runner = CommandRunnerResult::read_from_path(&self.output_path);
        let cancelled = *lock(&self.cancelled);
        let completion = CommandCompletionStatus::from_process_and_runner(
            process_exit,
            runner.as_ref(),
            cancelled,
        );
        let response = self.finalize_with_output(
            completion.status(),
            Some(completion.exit_code()),
            runner.map(|runner| runner.value().clone()),
            self.final_stdout(),
            true,
        );
        if let Ok(response) = response.as_ref() {
            if let Err(error) = write_final_response(&self.final_path, response) {
                self.remove_transcript_file();
                return Some(Err(error));
            }
        }
        self.remove_transcript_file();
        Some(response)
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
impl CommandSessionWaitTarget<Result<CommandResponse, CommandSessionError>> for CommandSession {
    fn try_finalize(&self) -> Option<Result<CommandResponse, CommandSessionError>> {
        self.try_finalize_process()
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
fn inactive_process_parts(output_drain_grace_ms: u64) -> RunningCommandSessionParts {
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
        output_drain_grace_ms,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopPolicy;

    impl eos_workspace_api::CommandWorkspacePolicy for NoopPolicy {
        fn prepare_command_workspace(
            &self,
            _request: eos_workspace_api::PrepareCommandRequest,
        ) -> Result<eos_workspace_api::PreparedCommandWorkspace, eos_workspace_api::WorkspaceApiError>
        {
            unreachable!("session test does not prepare")
        }

        fn finalize_command_workspace(
            &self,
            _request: eos_workspace_api::FinalizeCommandRequest,
        ) -> Result<eos_workspace_api::WorkspaceCommandOutcome, eos_workspace_api::WorkspaceApiError>
        {
            unreachable!("session test does not finalize")
        }
    }

    #[test]
    fn session_exposes_identity_and_expiry() {
        let config = CommandSessionConfig::default();
        let session = CommandSession::new(
            CommandSessionSpec {
                id: "cmd_1".to_owned(),
                caller_id: "caller".to_owned(),
                command: "echo ok".to_owned(),
                timeout_seconds: Some(0.001),
            },
            Box::new(NoopPolicy),
            &config,
        );

        assert_eq!(session.id(), "cmd_1");
        assert_eq!(session.caller_id(), "caller");
        assert_eq!(session.command(), "echo ok");
        assert!(session.is_expired(session.started_at() + Duration::from_millis(2)));
    }

    #[cfg(target_os = "linux")]
    struct FinalizingPolicy;

    #[cfg(target_os = "linux")]
    impl eos_workspace_api::CommandWorkspacePolicy for FinalizingPolicy {
        fn prepare_command_workspace(
            &self,
            _request: eos_workspace_api::PrepareCommandRequest,
        ) -> Result<eos_workspace_api::PreparedCommandWorkspace, eos_workspace_api::WorkspaceApiError>
        {
            unreachable!("finalization test does not prepare")
        }

        fn finalize_command_workspace(
            &self,
            request: eos_workspace_api::FinalizeCommandRequest,
        ) -> Result<eos_workspace_api::WorkspaceCommandOutcome, eos_workspace_api::WorkspaceApiError>
        {
            Ok(eos_workspace_api::WorkspaceCommandOutcome {
                mode: eos_workspace_api::WorkspaceMode::default(),
                success: request.command_succeeded(),
                status: request.status,
                exit_code: request.exit_code,
                stdout: request.stdout,
                stderr: request.stderr,
                command_session_id: request.command_session_id,
                changed_paths: Vec::new(),
                changed_path_kinds: Default::default(),
                mutation_source: "test".to_owned(),
                conflict: None,
                conflict_reason: None,
                timings: Default::default(),
                metadata: serde_json::Value::Null,
            })
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_finalization_removes_transcript_file() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "eos-command-session-transcript-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&root)?;
        let transcript_path = root.join("transcript.log");
        let final_path = root.join("final.json");
        std::fs::write(&transcript_path, b"captured transcript output")?;

        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")?;
        let session = CommandSession::new_running(
            CommandSessionSpec {
                id: "cmd_1".to_owned(),
                caller_id: "caller".to_owned(),
                command: "echo ok".to_owned(),
                timeout_seconds: None,
            },
            Box::new(FinalizingPolicy),
            RunningCommandSessionParts {
                process: crate::process::CommandSessionProcess::inactive(writer),
                output_path: root.join("runner-result.json"),
                final_path: final_path.clone(),
                transcript_path: transcript_path.clone(),
                output_drain_grace_ms: 0,
            },
        );

        let result = session
            .try_finalize_process()
            .expect("inactive process finalizes")?;

        assert_eq!(result.command_session_id.as_deref(), Some("cmd_1"));
        assert_eq!(result.stdout, "captured transcript output");
        assert!(final_path.exists());
        let final_response: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&final_path)?)?;
        assert_eq!(
            final_response
                .get("output")
                .and_then(|output| output.get("stdout"))
                .and_then(serde_json::Value::as_str),
            Some("captured transcript output")
        );
        assert!(!transcript_path.exists());

        let _ = std::fs::remove_dir_all(root);
        Ok(())
    }
}
