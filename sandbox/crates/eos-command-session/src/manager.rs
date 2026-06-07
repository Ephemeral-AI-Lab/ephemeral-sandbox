use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Duration;
use std::time::Instant;

use eos_workspace_api::CommandWorkspacePolicy;

#[cfg(target_os = "linux")]
use crate::process::spawn_current_exe_ns_runner;
use crate::registry::{CommandSessionCompletion, CommandSessionRegistry, WorkspaceRunKind};
#[cfg(target_os = "linux")]
use crate::session::RunningCommandSessionParts;
use crate::session::{CommandSession, CommandSessionSpec};
#[cfg(target_os = "linux")]
use crate::wait::{wait_for_yield, WaitOutcome};
use crate::{
    CancelCommandSession, CollectCompleted, CollectCompletedResponse, CommandResponse,
    CommandSessionConfig, CommandSessionError, DynCommandWorkspacePolicy, ReadCommandProgress,
    StartCommandSession, WriteStdin,
};

pub struct CommandSessionManager {
    config: CommandSessionConfig,
    registry: Arc<CommandSessionRegistry>,
}

impl CommandSessionManager {
    #[must_use]
    pub fn new(config: CommandSessionConfig) -> Self {
        Self {
            config,
            registry: Arc::new(CommandSessionRegistry::new()),
        }
    }

    pub fn start<P>(
        &self,
        request: StartCommandSession,
        policy: P,
        kind: WorkspaceRunKind,
    ) -> Result<CommandResponse, CommandSessionError>
    where
        P: CommandWorkspacePolicy + 'static,
    {
        self.start_boxed(request, Box::new(policy), kind)
    }

    pub fn start_boxed(
        &self,
        request: StartCommandSession,
        policy: DynCommandWorkspacePolicy,
        kind: WorkspaceRunKind,
    ) -> Result<CommandResponse, CommandSessionError> {
        #[cfg(target_os = "linux")]
        {
            self.start_boxed_linux(request, policy, kind)
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.start_boxed_scaffold(request, policy, kind)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn start_boxed_scaffold(
        &self,
        request: StartCommandSession,
        policy: DynCommandWorkspacePolicy,
        kind: WorkspaceRunKind,
    ) -> Result<CommandResponse, CommandSessionError> {
        if request.cmd.trim().is_empty() {
            return Err(CommandSessionError::InvalidRequest(
                "cmd must be non-empty".to_owned(),
            ));
        }
        let id = self.registry.next_id();
        let _prepared = policy.prepare_command_workspace(request.prepare_request(id.clone()))?;
        let caller_id = request.caller_id;
        let command = request.cmd;
        policy.command_session_started(&id, &caller_id);
        let session = Arc::new(CommandSession::new(
            CommandSessionSpec {
                id: id.clone(),
                caller_id,
                command,
                timeout_seconds: request.timeout_seconds,
            },
            policy,
            &self.config,
        ));
        self.registry.insert(session, kind);
        Ok(CommandResponse::running(id, String::new()))
    }

    #[cfg(target_os = "linux")]
    fn start_boxed_linux(
        &self,
        request: StartCommandSession,
        policy: DynCommandWorkspacePolicy,
        kind: WorkspaceRunKind,
    ) -> Result<CommandResponse, CommandSessionError> {
        if request.cmd.trim().is_empty() {
            return Err(CommandSessionError::InvalidRequest(
                "cmd must be non-empty".to_owned(),
            ));
        }
        let id = self.registry.next_id();
        let prepared = policy.prepare_command_workspace(request.prepare_request(id.clone()))?;
        let process = spawn_current_exe_ns_runner(
            &prepared.request_path,
            &prepared.run_request,
            &prepared.output_path,
            prepared.transcript_path.clone(),
            &self.config.transcript_timestamp_timezone,
        )?;
        let caller_id = request.caller_id;
        let command = request.cmd;
        policy.command_session_started(&id, &caller_id);
        let session = Arc::new(CommandSession::new_running(
            CommandSessionSpec {
                id: id.clone(),
                caller_id,
                command,
                timeout_seconds: request.timeout_seconds,
            },
            policy,
            RunningCommandSessionParts {
                process,
                output_path: prepared.output_path,
                final_path: prepared.final_path,
                transcript_path: prepared.transcript_path,
                output_drain_grace_ms: self.config.output_drain_grace_ms,
            },
        ));
        self.registry.insert(Arc::clone(&session), kind);
        match wait_for_yield(session.as_ref(), &self.config, request.yield_time_ms, 0) {
            WaitOutcome::Completed(result) => {
                let response = result?;
                Ok(self.finish_completed(session, response, false))
            }
            WaitOutcome::Running(stdout) => Ok(CommandResponse::running(id, stdout)),
        }
    }

    pub fn write_stdin(&self, request: WriteStdin) -> Result<CommandResponse, CommandSessionError> {
        #[cfg(target_os = "linux")]
        {
            self.write_stdin_linux(request)
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.write_stdin_scaffold(request)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn write_stdin_scaffold(
        &self,
        request: WriteStdin,
    ) -> Result<CommandResponse, CommandSessionError> {
        if is_teardown_control(&request.chars) {
            return self.cancel(CancelCommandSession {
                command_session_id: request.command_session_id,
            });
        }
        if contains_teardown_control(&request.chars) {
            return Err(CommandSessionError::InvalidRequest(
                "Ctrl-C/Ctrl-D must be sent alone to cancel command session".to_owned(),
            ));
        }
        let Some(session) = self.registry.get(&request.command_session_id) else {
            return Err(CommandSessionError::NotFound(request.command_session_id));
        };
        if request.chars.is_empty() {
            return Err(CommandSessionError::InvalidRequest(
                "chars must be non-empty".to_owned(),
            ));
        }
        let _ = (session, request.yield_time_ms);
        Ok(CommandResponse::running(
            request.command_session_id,
            String::new(),
        ))
    }

    #[cfg(target_os = "linux")]
    fn write_stdin_linux(
        &self,
        request: WriteStdin,
    ) -> Result<CommandResponse, CommandSessionError> {
        if is_teardown_control(&request.chars) {
            return self.cancel(CancelCommandSession {
                command_session_id: request.command_session_id,
            });
        }
        if contains_teardown_control(&request.chars) {
            return Err(CommandSessionError::InvalidRequest(
                "Ctrl-C/Ctrl-D must be sent alone to cancel command session".to_owned(),
            ));
        }
        let Some(session) = self.registry.get(&request.command_session_id) else {
            return Err(CommandSessionError::NotFound(request.command_session_id));
        };
        if request.chars.is_empty() {
            return Err(CommandSessionError::InvalidRequest(
                "chars must be non-empty".to_owned(),
            ));
        }
        let command_session_id = request.command_session_id.clone();
        let start_offset = session.transcript_len();
        session.write_process_stdin(&request.chars)?;
        match wait_for_yield(
            session.as_ref(),
            &self.config,
            request.yield_time_ms,
            start_offset,
        ) {
            WaitOutcome::Completed(result) => {
                let response = result?;
                Ok(self.finish_completed(session, response, false))
            }
            WaitOutcome::Running(stdout) => {
                Ok(CommandResponse::running(command_session_id, stdout))
            }
        }
    }

    pub fn read_progress(
        &self,
        request: ReadCommandProgress,
    ) -> Result<CommandResponse, CommandSessionError> {
        if request.last_n_lines == 0 {
            return Err(CommandSessionError::InvalidRequest(
                "last_n_lines must be >= 1".to_owned(),
            ));
        }
        let Some(session) = self.registry.get(&request.command_session_id) else {
            return self
                .registry
                .completed_result(&request.command_session_id)
                .map(|result| result.with_last_lines(request.last_n_lines))
                .ok_or(CommandSessionError::NotFound(request.command_session_id));
        };
        #[cfg(target_os = "linux")]
        if let Some(result) = session.try_finalize_process() {
            let response = result?;
            return Ok(self
                .finish_completed(session, response, false)
                .with_last_lines(request.last_n_lines));
        }
        Ok(CommandResponse::running(
            request.command_session_id,
            session.read_recent_output(request.last_n_lines),
        ))
    }

    pub fn cancel(
        &self,
        request: CancelCommandSession,
    ) -> Result<CommandResponse, CommandSessionError> {
        #[cfg(target_os = "linux")]
        {
            self.cancel_linux(request)
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.cancel_scaffold(request)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn cancel_scaffold(
        &self,
        request: CancelCommandSession,
    ) -> Result<CommandResponse, CommandSessionError> {
        let Some(session) = self.registry.get(&request.command_session_id) else {
            return self
                .registry
                .take_completed_result(&request.command_session_id)
                .ok_or(CommandSessionError::NotFound(request.command_session_id));
        };
        self.finish_session(session, "cancelled", Some(130), true)
    }

    #[cfg(target_os = "linux")]
    fn cancel_linux(
        &self,
        request: CancelCommandSession,
    ) -> Result<CommandResponse, CommandSessionError> {
        let Some(session) = self.registry.get(&request.command_session_id) else {
            return self
                .registry
                .take_completed_result(&request.command_session_id)
                .ok_or(CommandSessionError::NotFound(request.command_session_id));
        };
        let start_offset = session.transcript_len();
        session.cancel_process();
        match wait_for_yield(
            session.as_ref(),
            &self.config,
            self.config.cancel_wait_ms,
            start_offset,
        ) {
            WaitOutcome::Completed(result) => {
                let response = result?;
                Ok(self.finish_completed(session, response, false))
            }
            WaitOutcome::Running(stdout) => Ok(CommandResponse::cancelled(stdout)),
        }
    }

    #[must_use]
    pub fn count_by_caller(&self, caller_id: Option<&str>) -> usize {
        self.registry.count_by_caller(caller_id)
    }

    #[must_use]
    pub fn collect_completed(&self, request: &CollectCompleted) -> CollectCompletedResponse {
        self.registry.collect_completed(request)
    }

    pub fn push_completed(&self, completion: CommandSessionCompletion) {
        self.registry.push_completed(completion);
    }

    /// Cancel and discard every command session owned by `caller_id` (the
    /// per-caller workspace-run teardown). Cancelled sessions discard their
    /// overlay and push no completion (the caller initiated the cancel).
    #[must_use]
    pub fn cleanup_caller(&self, caller_id: &str, grace_s: Option<f64>) -> usize {
        #[cfg(target_os = "linux")]
        {
            let caller_id = caller_id.trim();
            if caller_id.is_empty() {
                return 0;
            }
            self.cancel_and_drain(self.registry.caller_sessions(caller_id), grace_s)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (caller_id, grace_s);
            0
        }
    }

    /// Cancel and discard every live command session in the sandbox (the
    /// whole-sandbox sweep backstop). Like `cleanup_caller` but across all
    /// callers; cancelled sessions discard and push no completion.
    #[must_use]
    pub fn cancel_all(&self, grace_s: Option<f64>) -> usize {
        #[cfg(target_os = "linux")]
        {
            self.cancel_and_drain(self.registry.live(), grace_s)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = grace_s;
            0
        }
    }

    /// Cancel every session, then reap+discard within `grace`, finalizing any
    /// stragglers. Returns the number of sessions that were live at entry.
    #[cfg(target_os = "linux")]
    fn cancel_and_drain(&self, sessions: Vec<Arc<CommandSession>>, grace_s: Option<f64>) -> usize {
        if sessions.is_empty() {
            return 0;
        }
        for session in &sessions {
            session.cancel_process();
        }

        let cancel_wait_s = self.config.cancel_wait_ms as f64 / 1000.0;
        let wait_s = grace_s.unwrap_or(cancel_wait_s).max(cancel_wait_s);
        let deadline = Instant::now() + Duration::from_secs_f64(wait_s);
        let mut pending = sessions.clone();
        loop {
            pending.retain(|session| match session.try_finalize_process() {
                Some(Ok(response)) => {
                    let _ = self.finish_completed(Arc::clone(session), response, false);
                    false
                }
                Some(Err(error)) => {
                    let response = CommandResponse::error(error.to_string());
                    let _ = self.finish_completed(Arc::clone(session), response, false);
                    false
                }
                None => true,
            });
            if pending.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for session in pending {
            if let Some(result) = session.try_finalize_process() {
                let response =
                    result.unwrap_or_else(|error| CommandResponse::error(error.to_string()));
                let _ = self.finish_completed(session, response, false);
            }
        }
        sessions.len()
    }

    #[must_use]
    pub fn sweep_expired(&self, now: Instant) -> SweepReport {
        #[cfg(target_os = "linux")]
        {
            self.sweep_linux(now)
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.sweep_scaffold(now)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn sweep_scaffold(&self, now: Instant) -> SweepReport {
        let mut expired = 0;
        for session in self.registry.live() {
            if session.is_expired(now) && self.registry.remove(session.id()).is_some() {
                expired += 1;
            }
        }
        SweepReport {
            expired,
            live: self.registry.live().len(),
        }
    }

    #[cfg(target_os = "linux")]
    fn sweep_linux(&self, now: Instant) -> SweepReport {
        let mut expired = 0;
        for session in self.registry.live() {
            if session.is_past_deadline(now, self.config.max_session_s) {
                expired += 1;
                session.cancel_process();
            }
            if let Some(result) = session.try_finalize_process() {
                let response =
                    result.unwrap_or_else(|error| CommandResponse::error(error.to_string()));
                let publish_completion = !session.is_cancelled();
                let _ = self.finish_completed(session, response, publish_completion);
            }
        }
        SweepReport {
            expired,
            live: self.registry.live().len(),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn finish_session(
        &self,
        session: Arc<CommandSession>,
        status: &str,
        exit_code: Option<i64>,
        include_session_id: bool,
    ) -> Result<CommandResponse, CommandSessionError> {
        let result = session.settle_cancelled(status, exit_code, include_session_id)?;
        Ok(self.finish_completed(session, result, true))
    }

    fn finish_completed(
        &self,
        session: Arc<CommandSession>,
        result: CommandResponse,
        publish_completion: bool,
    ) -> CommandResponse {
        let result_for_completion = result.clone();
        let notification_result = result.clone();
        let command_session_id = session.id().to_owned();
        let caller_id = session.caller_id().to_owned();
        let command = session.command().to_owned();
        session.command_session_finished(&result.status);
        self.registry.remove(&command_session_id);
        if publish_completion {
            self.registry.push_completed(CommandSessionCompletion {
                command_session_id,
                caller_id,
                command,
                result: result_for_completion,
                notification_result,
            });
        }
        result
    }
}

fn is_teardown_control(chars: &str) -> bool {
    matches!(chars, "\u{3}" | "\u{4}")
}

fn contains_teardown_control(chars: &str) -> bool {
    chars.contains('\u{3}') || chars.contains('\u{4}')
}

impl Default for CommandSessionManager {
    fn default() -> Self {
        Self::new(CommandSessionConfig::default())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub expired: usize,
    pub live: usize,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use eos_workspace_api::{
        FinalizeCommandRequest, PrepareCommandRequest, PreparedCommandWorkspace, WorkspaceApiError,
        WorkspaceCommandOutcome, WorkspaceMode,
    };
    use serde_json::{json, Value};

    use super::*;

    struct ExpiringPolicy;

    impl CommandWorkspacePolicy for ExpiringPolicy {
        fn prepare_command_workspace(
            &self,
            request: PrepareCommandRequest,
        ) -> Result<PreparedCommandWorkspace, WorkspaceApiError> {
            let session_dir = PathBuf::from(format!("/sessions/{}", request.command_session_id));
            Ok(PreparedCommandWorkspace {
                run_request: json!({ "cmd": request.cmd }),
                request_path: session_dir.join("runner-request.json"),
                output_path: session_dir.join("runner-result.json"),
                final_path: session_dir.join("final.json"),
                session_dir: session_dir.clone(),
                transcript_path: session_dir.join("transcript.log"),
            })
        }

        fn finalize_command_workspace(
            &self,
            request: FinalizeCommandRequest,
        ) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
            Ok(WorkspaceCommandOutcome {
                mode: WorkspaceMode::default(),
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
                metadata: Value::Null,
            })
        }

        fn discard_command_workspace(
            &self,
            request: FinalizeCommandRequest,
        ) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
            Ok(WorkspaceCommandOutcome::discarded(
                WorkspaceMode::default(),
                request,
            ))
        }
    }

    #[test]
    fn manager_registers_counts_and_sweeps_sessions() {
        let manager = CommandSessionManager::default();
        let started = manager
            .start(
                StartCommandSession {
                    invocation_id: "inv".to_owned(),
                    caller_id: "caller".to_owned(),
                    cmd: "sleep 1".to_owned(),
                    timeout_seconds: Some(0.001),
                    yield_time_ms: 1000,
                },
                ExpiringPolicy,
                WorkspaceRunKind::Ephemeral,
            )
            .unwrap_or_else(|error| panic!("start session: {error}"));
        let id = started
            .command_session_id
            .unwrap_or_else(|| panic!("running session id"));
        assert!(id.starts_with("cmd_"));

        assert_eq!(manager.count_by_caller(Some("caller")), 1);

        let report = manager.sweep_expired(Instant::now() + Duration::from_millis(2));

        assert_eq!(report.expired, 1);
        assert_eq!(report.live, 0);
    }

    fn ephemeral_request(caller_id: &str) -> StartCommandSession {
        StartCommandSession {
            invocation_id: "inv".to_owned(),
            caller_id: caller_id.to_owned(),
            cmd: "sleep 1".to_owned(),
            timeout_seconds: None,
            yield_time_ms: 1000,
        }
    }

    #[test]
    fn caller_may_hold_multiple_sessions_per_kind() {
        // A caller holds many ephemeral command sessions (each its own ephemeral
        // workspace); an isolated caller holds many sessions in its one workspace.
        for kind in [WorkspaceRunKind::Ephemeral, WorkspaceRunKind::Isolated] {
            let manager = CommandSessionManager::default();
            for _ in 0..3 {
                manager
                    .start(ephemeral_request("caller"), ExpiringPolicy, kind)
                    .unwrap_or_else(|error| panic!("start ({kind:?}): {error}"));
            }
            assert_eq!(manager.count_by_caller(Some("caller")), 3);
            assert_eq!(manager.count_by_caller(Some("other")), 0);
        }
    }

    #[test]
    fn collected_completion_preserves_finalized_stdout() {
        let manager = CommandSessionManager::default();
        let command_session_id = "cmd_full".to_owned();
        let session = Arc::new(CommandSession::new(
            CommandSessionSpec {
                id: command_session_id.clone(),
                caller_id: "caller".to_owned(),
                command: "printf full".to_owned(),
                timeout_seconds: None,
            },
            Box::new(ExpiringPolicy),
            &CommandSessionConfig::default(),
        ));
        let result = CommandResponse {
            status: "ok".to_owned(),
            exit_code: Some(0),
            stdout: "full transcript stdout".to_owned(),
            stderr: String::new(),
            command_session_id: Some(command_session_id.clone()),
            workspace_mode: Some(WorkspaceMode::default()),
            metadata: Value::Null,
        };

        let returned = manager.finish_completed(session, result, true);

        assert_eq!(returned.stdout, "full transcript stdout");
        let completions = manager.collect_completed(&CollectCompleted {
            command_session_ids: Some(vec![command_session_id]),
            caller_id: Some("caller".to_owned()),
        });
        assert_eq!(completions.completions.len(), 1);
        assert_eq!(
            completions.completions[0].result.stdout,
            "full transcript stdout"
        );
        assert_eq!(
            completions.completions[0].notification_result.stdout,
            "full transcript stdout"
        );
    }
}
