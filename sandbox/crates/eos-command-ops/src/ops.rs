use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eos_command_session::process::{spawn_current_exe_ns_runner, KillReason};
use eos_command_session::session::{
    CommandSession, CommandSessionSpec, ReapedCommand, RunningCommandSessionParts,
};
use eos_command_session::yield_wait_loop::{wait_for_yield, WaitOutcome};
use eos_command_session::{
    CancelCommandSession, CollectCompleted, CollectCompletedResponse, CommandResponse,
    CommandSessionCompletion, CommandSessionConfig, CommandSessionError, ReadCommandProgress,
    StartCommandSession, WriteStdin,
};
use eos_ephemeral_workspace::EphemeralWorkspace;
use eos_layerstack::service;

use crate::outcome::FinalizeCommandRequest;
use crate::prepare::{prepare_ephemeral, prepare_isolated, PrepareInputs, PreparedCommand};
use crate::registry::{ActiveCommand, CommandRegistry, EphemeralRun, IsolatedRun};
use crate::settle::{discarded_response, settle_ephemeral, settle_isolated};
use crate::CommandBinding;

pub enum ExecTarget {
    Ephemeral {
        root: PathBuf,
        workspace_root: PathBuf,
        scratch_root: PathBuf,
    },
    Isolated {
        binding: Box<CommandBinding>,
    },
}

pub struct CommandOps {
    config: CommandSessionConfig,
    registry: Arc<CommandRegistry>,
}

impl CommandOps {
    #[must_use]
    pub fn new(config: CommandSessionConfig) -> Self {
        Self {
            config,
            registry: Arc::new(CommandRegistry::new()),
        }
    }

    pub fn exec_command(
        &self,
        request: StartCommandSession,
        target: ExecTarget,
    ) -> Result<CommandResponse, CommandSessionError> {
        if request.cmd.trim().is_empty() {
            return Err(CommandSessionError::InvalidRequest(
                "cmd must be non-empty".to_owned(),
            ));
        }
        let id = self.registry.next_id();
        let yield_time_ms = request.yield_time_ms;
        let spec = CommandSessionSpec {
            id: id.clone(),
            caller_id: request.caller_id.clone(),
            command: request.cmd.clone(),
            timeout_seconds: request.timeout_seconds,
        };
        match target {
            ExecTarget::Ephemeral {
                root,
                workspace_root,
                scratch_root,
            } => self.start_ephemeral(
                spec,
                &request,
                &id,
                root,
                workspace_root,
                scratch_root,
                yield_time_ms,
            ),
            ExecTarget::Isolated { binding } => {
                self.start_isolated(spec, &request, &id, binding, yield_time_ms)
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "start inputs are one-shot plumbing from the typed target"
    )]
    fn start_ephemeral(
        &self,
        spec: CommandSessionSpec,
        request: &StartCommandSession,
        command_id: &str,
        root: PathBuf,
        workspace_root: PathBuf,
        scratch_root: PathBuf,
        yield_time_ms: u64,
    ) -> Result<CommandResponse, CommandSessionError> {
        let request_id = format!(
            "command_session:{}:{}",
            request.caller_id, request.invocation_id
        );
        let snapshot = service::acquire_snapshot(&root, &request_id)
            .map_err(|error| CommandSessionError::Workspace(error.to_string()))?;
        let writable_root = eos_overlay::overlay_writable_root()
            .map_err(|error| CommandSessionError::Workspace(error.to_string()));
        let result = writable_root.and_then(|writable_root| {
            let workspace = EphemeralWorkspace::create(
                &writable_root.join("runtime"),
                "sandbox-overlay",
                &request.invocation_id,
                workspace_root,
                snapshot.layer_paths.clone(),
            )
            .map_err(|error| CommandSessionError::Workspace(error.to_string()))?;
            let prepared = prepare_ephemeral(
                PrepareInputs {
                    caller_id: &request.caller_id,
                    command_id,
                    invocation_id: &request.invocation_id,
                    cmd: &request.cmd,
                    timeout_seconds: request.timeout_seconds,
                    session_dir: scratch_root.join(command_id),
                    workspace_label: "ephemeral",
                },
                workspace.mount_plan(),
                &workspace.dirs().run_dir,
            )?;
            let session = self.spawn_session(spec, prepared)?;
            Ok((workspace, session))
        });
        let (workspace, session) = match result {
            Ok(parts) => parts,
            Err(error) => {
                let _ = service::release_lease(&root, &snapshot.lease_id);
                return Err(error);
            }
        };
        Ok(
            self.register_and_wait(session, yield_time_ms, move |session| {
                ActiveCommand::Ephemeral(EphemeralRun {
                    session,
                    root,
                    snapshot,
                    workspace,
                })
            }),
        )
    }

    fn start_isolated(
        &self,
        spec: CommandSessionSpec,
        request: &StartCommandSession,
        command_id: &str,
        binding: Box<CommandBinding>,
        yield_time_ms: u64,
    ) -> Result<CommandResponse, CommandSessionError> {
        let prepared = prepare_isolated(
            PrepareInputs {
                caller_id: &request.caller_id,
                command_id,
                invocation_id: &request.invocation_id,
                cmd: &request.cmd,
                timeout_seconds: request.timeout_seconds,
                session_dir: binding.scratch_dir.join("sessions").join(command_id),
                workspace_label: "isolated",
            },
            &binding,
        )?;
        let session = self.spawn_session(spec, prepared)?;
        let binding = *binding;
        Ok(
            self.register_and_wait(session, yield_time_ms, move |session| {
                ActiveCommand::Isolated(IsolatedRun { session, binding })
            }),
        )
    }

    fn spawn_session(
        &self,
        spec: CommandSessionSpec,
        prepared: PreparedCommand,
    ) -> Result<CommandSession, CommandSessionError> {
        let process = spawn_current_exe_ns_runner(
            &prepared.request_path,
            &prepared.run_request,
            &prepared.output_path,
            prepared.transcript_path.clone(),
            &self.config.transcript_timestamp_timezone,
        )?;
        Ok(CommandSession::new_running(
            spec,
            RunningCommandSessionParts {
                process,
                output_path: prepared.output_path,
                final_path: prepared.final_path,
                transcript_path: prepared.transcript_path,
                output_drain_grace_ms: self.config.output_drain_grace_ms,
            },
        ))
    }

    fn register_and_wait(
        &self,
        session: CommandSession,
        yield_time_ms: u64,
        make_run: impl FnOnce(CommandSession) -> ActiveCommand,
    ) -> CommandResponse {
        let id = session.id().to_owned();
        let run = Arc::new(make_run(session));
        self.registry.insert(Arc::clone(&run));
        self.wait_on_run(run, yield_time_ms, 0, |stdout| {
            CommandResponse::running(id, stdout)
        })
    }

    fn wait_on_run(
        &self,
        run: Arc<ActiveCommand>,
        wait_ms: u64,
        start_offset: u64,
        on_running: impl FnOnce(String) -> CommandResponse,
    ) -> CommandResponse {
        match wait_for_yield(run.session(), &self.config, wait_ms, start_offset) {
            WaitOutcome::Completed(reaped) => self.finish_reaped(run, reaped, false),
            WaitOutcome::Running(stdout) => on_running(stdout),
        }
    }

    pub fn write_stdin(&self, request: WriteStdin) -> Result<CommandResponse, CommandSessionError> {
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
        let Some(run) = self.registry.get(&request.command_session_id) else {
            return Err(CommandSessionError::NotFound(request.command_session_id));
        };
        if request.chars.is_empty() {
            return Err(CommandSessionError::InvalidRequest(
                "chars must be non-empty".to_owned(),
            ));
        }
        let command_session_id = request.command_session_id.clone();
        let start_offset = run.session().transcript_len();
        run.session().write_process_stdin(&request.chars)?;
        Ok(
            self.wait_on_run(run, request.yield_time_ms, start_offset, |stdout| {
                CommandResponse::running(command_session_id, stdout)
            }),
        )
    }

    pub fn read_command_progress(
        &self,
        request: ReadCommandProgress,
    ) -> Result<CommandResponse, CommandSessionError> {
        if request.last_n_lines == 0 {
            return Err(CommandSessionError::InvalidRequest(
                "last_n_lines must be >= 1".to_owned(),
            ));
        }
        let Some(run) = self.registry.get(&request.command_session_id) else {
            return self
                .registry
                .completed_result(&request.command_session_id)
                .map(|result| result.with_last_lines(request.last_n_lines))
                .ok_or(CommandSessionError::NotFound(request.command_session_id));
        };
        if let Some(reaped) = run.session().reap() {
            return Ok(self
                .finish_reaped(run, reaped, false)
                .with_last_lines(request.last_n_lines));
        }
        Ok(CommandResponse::running(
            request.command_session_id,
            run.session().read_recent_output(request.last_n_lines),
        ))
    }

    pub fn cancel(
        &self,
        request: CancelCommandSession,
    ) -> Result<CommandResponse, CommandSessionError> {
        let Some(run) = self.registry.get(&request.command_session_id) else {
            return self
                .registry
                .take_completed_result(&request.command_session_id)
                .ok_or(CommandSessionError::NotFound(request.command_session_id));
        };
        let start_offset = run.session().transcript_len();
        run.session().cancel_process();
        Ok(
            self.wait_on_run(run, self.config.cancel_wait_ms, start_offset, |stdout| {
                CommandResponse::cancelled(stdout)
            }),
        )
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
        let caller_id = caller_id.trim();
        if caller_id.is_empty() {
            return 0;
        }
        self.cancel_and_drain(self.registry.caller_sessions(caller_id), grace_s)
    }

    #[must_use]
    pub fn cancel_all(&self, grace_s: Option<f64>) -> usize {
        self.cancel_and_drain(self.registry.live(), grace_s)
    }

    fn cancel_and_drain(&self, runs: Vec<Arc<ActiveCommand>>, grace_s: Option<f64>) -> usize {
        if runs.is_empty() {
            return 0;
        }
        for run in &runs {
            run.session().cancel_process();
        }
        let cancel_wait_s = self.config.cancel_wait_ms as f64 / 1000.0;
        let wait_s = grace_s.unwrap_or(cancel_wait_s).max(cancel_wait_s);
        let deadline = Instant::now() + Duration::from_secs_f64(wait_s);
        let mut pending = runs.clone();
        loop {
            pending.retain(|run| match run.session().reap() {
                Some(reaped) => {
                    let _ = self.finish_reaped(Arc::clone(run), reaped, false);
                    false
                }
                None => true,
            });
            if pending.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for run in pending {
            if let Some(reaped) = run.session().reap() {
                let _ = self.finish_reaped(run, reaped, false);
            } else {
                self.force_discard(&run);
            }
        }
        runs.len()
    }

    fn force_discard(&self, run: &Arc<ActiveCommand>) {
        if self.registry.remove(run.session().id()).is_none() {
            return;
        }
        if let ActiveCommand::Ephemeral(ephemeral) = &**run {
            let _ = service::release_lease(&ephemeral.root, &ephemeral.snapshot.lease_id);
        }
    }

    pub fn sweep_expired(&self, now: Instant) {
        for run in self.registry.live() {
            if run
                .session()
                .is_past_deadline(now, self.config.max_session_s)
            {
                run.session().time_out_process();
            }
            if let Some(reaped) = run.session().reap() {
                let publish_completion = reaped.kill != Some(KillReason::Cancelled);
                let _ = self.finish_reaped(run, reaped, publish_completion);
            }
        }
    }

    fn finish_reaped(
        &self,
        run: Arc<ActiveCommand>,
        reaped: ReapedCommand,
        publish_completion: bool,
    ) -> CommandResponse {
        let request = FinalizeCommandRequest {
            runner_result: reaped.runner_result,
            command_elapsed_s: reaped.elapsed_s,
            status: reaped.status,
            exit_code: Some(reaped.exit_code),
            stdout: reaped.stdout,
            stderr: String::new(),
            command_session_id: Some(run.session().id().to_owned()),
        };
        let cancelled = reaped.kill.is_some();
        let outcome = match &*run {
            ActiveCommand::Ephemeral(ephemeral) => {
                let outcome = if cancelled {
                    Ok(discarded_response("ephemeral", request))
                } else {
                    settle_ephemeral(
                        &ephemeral.root,
                        &ephemeral.snapshot,
                        &ephemeral.workspace,
                        request,
                    )
                };
                let _ = service::release_lease(&ephemeral.root, &ephemeral.snapshot.lease_id);
                outcome
            }
            ActiveCommand::Isolated(isolated) => {
                if cancelled {
                    Ok(discarded_response("isolated", request))
                } else {
                    settle_isolated(&isolated.binding, request)
                }
            }
        };
        let response = match outcome {
            Ok(response) => response,
            Err(error) => CommandResponse::error(error.to_string()),
        };
        run.session().persist_final(&response);
        let command_session_id = run.session().id().to_owned();
        let caller_id = run.session().caller_id().to_owned();
        let command = run.session().command().to_owned();
        self.registry.remove(&command_session_id);
        if publish_completion {
            self.registry.push_completed(CommandSessionCompletion {
                command_session_id,
                caller_id,
                command,
                result: response.clone(),
            });
        }
        response
    }
}

fn is_teardown_control(chars: &str) -> bool {
    matches!(chars, "\u{3}" | "\u{4}")
}

fn contains_teardown_control(chars: &str) -> bool {
    chars.contains('\u{3}') || chars.contains('\u{4}')
}
