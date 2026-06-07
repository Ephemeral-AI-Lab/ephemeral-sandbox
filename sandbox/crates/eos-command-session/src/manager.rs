use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Duration;
use std::time::Instant;

use eos_workspace_api::CommandWorkspacePolicy;

use crate::registry::{CommandSessionCompletion, CommandSessionRegistry};
#[cfg(target_os = "linux")]
use crate::session::RunningCommandSessionParts;
use crate::session::{CommandSession, CommandSessionSpec};
#[cfg(target_os = "linux")]
use crate::wait::{wait_for_yield, WaitOutcome};
#[cfg(target_os = "linux")]
use crate::{process::spawn_current_exe_ns_runner, CommandSessionOutput};
use crate::{
    CancelCommandSession, CollectCompleted, CollectCompletedResponse, CommandResponse,
    CommandSessionConfig, CommandSessionError, DynCommandWorkspacePolicy, StartCommandSession,
    WriteStdin,
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
    ) -> Result<CommandResponse, CommandSessionError>
    where
        P: CommandWorkspacePolicy + 'static,
    {
        self.start_boxed(request, Box::new(policy))
    }

    pub fn start_boxed(
        &self,
        request: StartCommandSession,
        policy: DynCommandWorkspacePolicy,
    ) -> Result<CommandResponse, CommandSessionError> {
        #[cfg(target_os = "linux")]
        {
            self.start_boxed_linux(request, policy)
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.start_boxed_scaffold(request, policy)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn start_boxed_scaffold(
        &self,
        request: StartCommandSession,
        policy: DynCommandWorkspacePolicy,
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
        self.registry.insert(Arc::clone(&session));
        Ok(CommandResponse::running(
            id,
            session.read_model_output(request.max_output_tokens),
        ))
    }

    #[cfg(target_os = "linux")]
    fn start_boxed_linux(
        &self,
        request: StartCommandSession,
        policy: DynCommandWorkspacePolicy,
    ) -> Result<CommandResponse, CommandSessionError> {
        if request.cmd.trim().is_empty() {
            return Err(CommandSessionError::InvalidRequest(
                "cmd must be non-empty".to_owned(),
            ));
        }
        let id = self.registry.next_id();
        let prepared = policy.prepare_command_workspace(request.prepare_request(id.clone()))?;
        let output = Arc::new(CommandSessionOutput::new(&self.config));
        let process = spawn_current_exe_ns_runner(
            &prepared.request_path,
            &prepared.run_request,
            &prepared.output_path,
            prepared.transcript_path.clone(),
            Arc::clone(&output),
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
                output,
                output_path: prepared.output_path,
                final_path: prepared.final_path,
                output_drain_grace_ms: self.config.output_drain_grace_ms,
            },
        ));
        self.registry.insert(Arc::clone(&session));
        match wait_for_yield(
            session.as_ref(),
            &self.config,
            request.yield_time_ms,
            request.max_output_tokens,
        ) {
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
        let Some(session) = self.registry.get(&request.command_session_id) else {
            return self
                .registry
                .take_completed_result(&request.command_session_id)
                .ok_or(CommandSessionError::NotFound(request.command_session_id));
        };
        if !request.chars.is_empty() {
            session.append_output(request.chars);
        }
        if request.terminate {
            return self.finish_session(session, "cancelled", Some(130), true);
        }
        Ok(CommandResponse::running(
            request.command_session_id,
            session.read_model_output(request.max_output_tokens),
        ))
    }

    #[cfg(target_os = "linux")]
    fn write_stdin_linux(
        &self,
        request: WriteStdin,
    ) -> Result<CommandResponse, CommandSessionError> {
        let Some(session) = self.registry.get(&request.command_session_id) else {
            return self
                .registry
                .take_completed_result(&request.command_session_id)
                .ok_or(CommandSessionError::NotFound(request.command_session_id));
        };
        session.write_process_stdin(&request.chars)?;
        if request.terminate {
            session.cancel_process();
        }
        match wait_for_yield(
            session.as_ref(),
            &self.config,
            request.yield_time_ms,
            request.max_output_tokens,
        ) {
            WaitOutcome::Completed(result) => {
                let response = result?;
                Ok(self.finish_completed(session, response, false))
            }
            WaitOutcome::Running(stdout) => {
                Ok(CommandResponse::running(request.command_session_id, stdout))
            }
        }
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
        let _ = request.max_output_tokens;
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
        session.cancel_process();
        match wait_for_yield(
            session.as_ref(),
            &self.config,
            self.config.cancel_wait_ms,
            request.max_output_tokens,
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

    #[must_use]
    pub fn cleanup_caller(&self, caller_id: &str, grace_s: Option<f64>) -> usize {
        #[cfg(target_os = "linux")]
        {
            self.cleanup_caller_linux(caller_id, grace_s)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (caller_id, grace_s);
            0
        }
    }

    #[cfg(target_os = "linux")]
    fn cleanup_caller_linux(&self, caller_id: &str, grace_s: Option<f64>) -> usize {
        let caller_id = caller_id.trim();
        if caller_id.is_empty() {
            return 0;
        }
        let sessions: Vec<Arc<CommandSession>> = self
            .registry
            .live()
            .into_iter()
            .filter(|session| session.caller_id() == caller_id)
            .collect();
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
        let result = session.finalize(status, exit_code, include_session_id)?;
        Ok(self.finish_completed(session, result, true))
    }

    fn finish_completed(
        &self,
        session: Arc<CommandSession>,
        result: CommandResponse,
        publish_completion: bool,
    ) -> CommandResponse {
        let result_for_completion = if publish_completion {
            result.clone().with_stdout(session.read_model_output(None))
        } else {
            result.clone()
        };
        let notification_result = result
            .clone()
            .with_stdout(session.read_notification_output(None));
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
                    max_output_tokens: None,
                },
                ExpiringPolicy,
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
}
