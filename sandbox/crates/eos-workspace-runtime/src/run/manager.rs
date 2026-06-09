//! Linux workspace-run lifecycle: start/settle command sessions over the
//! caller-keyed registry.
//!
//! The manager owns no policy. It acquires the snapshot lease (ephemeral),
//! builds the runner request via the workspace crates' lifecycle free functions,
//! spawns the PTY substrate, and on settlement either **publishes** (ephemeral
//! complete), **records for audit** (isolated complete), or **discards** (cancel
//! — never reaching the OCC merge). Each run owns its overlay/namespace state
//! directly, so the publish-vs-discard branch is structural, not a flag check.
//!
//! The OCC publish, per-finalize resource telemetry, and isolated-audit sink are
//! daemon-resident; they are injected via [`WorkspaceRunHostPorts`] so this crate
//! keeps no `eos-occ` edge and no daemon-global state.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::command_session::process::{spawn_current_exe_ns_runner, KillReason};
use crate::command_session::session::{
    CommandSession, CommandSessionSpec, ReapedCommand, RunningCommandSessionParts,
};
use crate::command_session::wait::{wait_for_yield, WaitOutcome};
use crate::command_session::{
    CancelCommandSession, CollectCompleted, CollectCompletedResponse, CommandResponse,
    CommandSessionCompletion, CommandSessionConfig, CommandSessionError, ReadCommandProgress,
    StartCommandSession, WriteStdin,
};
use crate::ephemeral::{
    discard_ephemeral_command, prepare_ephemeral_command, EphemeralCommandPrepareContext,
    SnapshotLease, PreparedEphemeralCommand,
};
use crate::isolated::{
    finalize_isolated_command, prepare_isolated_command, take_isolated_audit,
    IsolatedCommandFinalizeContext, IsolatedCommandPrepareContext,
};
use eos_layerstack::LayerStack;
use eos_overlay::overlay_writable_root;
use eos_workspace_contract::{
    FinalizeCommandRequest, WorkspaceApiError, WorkspaceCommandOutcome, WorkspaceMode,
};

use super::isolated_command_handle::IsolatedCommandHandle;
use super::ports::WorkspaceRunHostPorts;
use super::registry::{EphemeralRun, IsolatedRun, WorkspaceRun, WorkspaceRunRegistry};

/// Which workspace a starting command session belongs to. The daemon picks the
/// kind from the caller's current mode and supplies the inputs the manager needs
/// to lay out the run: the LayerStack roots (ephemeral) or the caller's isolated
/// namespace handle (isolated).
pub enum StartTarget {
    Ephemeral {
        root: PathBuf,
        workspace_root: PathBuf,
        scratch_root: PathBuf,
    },
    Isolated {
        // Boxed: `IsolatedCommandHandle` is far larger than the ephemeral variant,
        // and this enum is only a short-lived dispatch value.
        handle: Box<IsolatedCommandHandle>,
    },
}

pub struct WorkspaceRunManager {
    config: CommandSessionConfig,
    registry: Arc<WorkspaceRunRegistry>,
    ports: Arc<dyn WorkspaceRunHostPorts>,
}

impl WorkspaceRunManager {
    #[must_use]
    pub fn new(config: CommandSessionConfig, ports: Arc<dyn WorkspaceRunHostPorts>) -> Self {
        Self {
            config,
            registry: Arc::new(WorkspaceRunRegistry::new()),
            ports,
        }
    }

    pub fn start(
        &self,
        request: StartCommandSession,
        target: StartTarget,
    ) -> Result<CommandResponse, CommandSessionError> {
        if request.cmd.trim().is_empty() {
            return Err(CommandSessionError::InvalidRequest(
                "cmd must be non-empty".to_owned(),
            ));
        }
        let id = self.registry.next_id();
        let prepare_request = request.prepare_request(id.clone());
        let yield_time_ms = request.yield_time_ms;
        let spec = CommandSessionSpec {
            id,
            caller_id: request.caller_id,
            command: request.cmd,
            timeout_seconds: request.timeout_seconds,
        };
        match target {
            StartTarget::Ephemeral {
                root,
                workspace_root,
                scratch_root,
            } => self.start_ephemeral(
                spec,
                prepare_request,
                root,
                workspace_root,
                scratch_root,
                yield_time_ms,
            ),
            StartTarget::Isolated { handle } => {
                self.start_isolated(spec, prepare_request, handle, yield_time_ms)
            }
        }
    }

    fn start_ephemeral(
        &self,
        spec: CommandSessionSpec,
        prepare_request: eos_workspace_contract::PrepareCommandRequest,
        root: PathBuf,
        workspace_root: PathBuf,
        scratch_root: PathBuf,
        yield_time_ms: u64,
    ) -> Result<CommandResponse, CommandSessionError> {
        let request_id = format!(
            "command_session:{}:{}",
            prepare_request.caller_id, prepare_request.invocation_id
        );
        let lease = LayerStack::open(root.clone())
            .and_then(|stack| stack.acquire_snapshot(&request_id))
            .map_err(layerstack_error)?;
        let lease_id = lease.lease_id.clone();
        let snapshot = SnapshotLease {
            lease_id: lease.lease_id,
            manifest_version: lease.manifest_version,
            manifest_root_hash: lease.root_hash,
            layer_paths: lease.layer_paths.into_iter().map(PathBuf::from).collect(),
        };
        let session_dir = scratch_root.join(&spec.id);
        let context = EphemeralCommandPrepareContext {
            layer_stack_root: root.clone(),
            workspace_root,
            writable_root: overlay_writable_root().map_err(workspace_api_error)?,
            final_path: session_dir.join("final.json"),
            session_dir,
        };
        let prepared = match prepare_ephemeral_command(context, snapshot, prepare_request) {
            Ok(prepared) => prepared,
            Err(error) => {
                release_lease(&root, &lease_id);
                return Err(error.into());
            }
        };
        let PreparedEphemeralCommand {
            prepared,
            workspace,
        } = prepared;
        let session = match self.spawn_session(spec, prepared) {
            Ok(session) => session,
            Err(error) => {
                discard_ephemeral_command(&workspace.dirs);
                release_lease(&root, &lease_id);
                return Err(error);
            }
        };
        Ok(
            self.register_and_wait(session, yield_time_ms, move |session| {
                WorkspaceRun::Ephemeral(EphemeralRun { session, workspace })
            }),
        )
    }

    fn start_isolated(
        &self,
        spec: CommandSessionSpec,
        prepare_request: eos_workspace_contract::PrepareCommandRequest,
        handle: Box<IsolatedCommandHandle>,
        yield_time_ms: u64,
    ) -> Result<CommandResponse, CommandSessionError> {
        let context = IsolatedCommandPrepareContext {
            workspace_handle_id: handle.workspace_handle_id.clone(),
            workspace_root: handle.workspace_root.clone(),
            scratch_dir: handle.scratch_dir.clone(),
            layer_paths: handle.layer_paths.clone(),
            upperdir: handle.upperdir.clone(),
            workdir: handle.workdir.clone(),
            ns_fds: handle.ns_fds.clone(),
            cgroup_path: handle.cgroup_path.clone(),
        };
        let prepared = prepare_isolated_command(context, prepare_request)?;
        let session = self.spawn_session(spec, prepared)?;
        let handle = *handle;
        Ok(
            self.register_and_wait(session, yield_time_ms, move |session| {
                WorkspaceRun::Isolated(IsolatedRun { session, handle })
            }),
        )
    }

    fn spawn_session(
        &self,
        spec: CommandSessionSpec,
        prepared: eos_workspace_contract::PreparedCommandWorkspace,
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
        make_run: impl FnOnce(CommandSession) -> WorkspaceRun,
    ) -> CommandResponse {
        let id = session.id().to_owned();
        let run = Arc::new(make_run(session));
        self.registry.insert(Arc::clone(&run));
        match wait_for_yield(run.session(), &self.config, yield_time_ms, 0) {
            WaitOutcome::Completed(reaped) => self.finish_reaped(run, reaped, false),
            WaitOutcome::Running(stdout) => CommandResponse::running(id, stdout),
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
        match wait_for_yield(
            run.session(),
            &self.config,
            request.yield_time_ms,
            start_offset,
        ) {
            WaitOutcome::Completed(reaped) => Ok(self.finish_reaped(run, reaped, false)),
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
        match wait_for_yield(
            run.session(),
            &self.config,
            self.config.cancel_wait_ms,
            start_offset,
        ) {
            WaitOutcome::Completed(reaped) => Ok(self.finish_reaped(run, reaped, false)),
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
        let caller_id = caller_id.trim();
        if caller_id.is_empty() {
            return 0;
        }
        self.cancel_and_drain(self.registry.caller_sessions(caller_id), grace_s)
    }

    /// Cancel and discard every live command session in the sandbox (the
    /// whole-sandbox sweep backstop). Cancelled sessions discard and push no
    /// completion.
    #[must_use]
    pub fn cancel_all(&self, grace_s: Option<f64>) -> usize {
        self.cancel_and_drain(self.registry.live(), grace_s)
    }

    /// Cancel every run, then reap+discard within `grace`, finalizing any
    /// stragglers. Returns the number of runs that were live at entry.
    fn cancel_and_drain(&self, runs: Vec<Arc<WorkspaceRun>>, grace_s: Option<f64>) -> usize {
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
                // The kill could not reap the child within grace (e.g. an
                // uninterruptible D-state task). Teardown must NOT depend on a
                // successful reap (spec §3.3): force the run out of the registry
                // and release its overlay lease + dirs, or a stuck process would
                // leak the snapshot lease the whole-sandbox assert-no-leases gate
                // checks. Never publishes — this is the discard branch.
                self.force_discard(&run);
            }
        }
        runs.len()
    }

    /// Reap-independent teardown for a run the cancel grace could not reap.
    /// Releases an ephemeral run's overlay lease + dirs (best-effort: a still-live
    /// process may leave its dirs for the orphan reaper, but the lease is freed);
    /// an isolated session only needs its registry entry dropped, since the
    /// namespace + lease are torn down by `exit_isolated`.
    fn force_discard(&self, run: &Arc<WorkspaceRun>) {
        if self.registry.remove(run.session().id()).is_none() {
            // A concurrent reap already finalized + discarded it.
            return;
        }
        if let WorkspaceRun::Ephemeral(ephemeral) = &**run {
            discard_ephemeral_command(&ephemeral.workspace.dirs);
            release_lease(
                &ephemeral.workspace.layer_stack_root.0,
                &ephemeral.workspace.snapshot.lease_id,
            );
        }
    }

    /// Periodic reaper: enforce the per-session timeout backstop and finalize any
    /// session whose child has exited without a live poller, parking the
    /// completion for the heartbeat.
    ///
    /// A past-deadline session is killed as a **timeout** (not a user cancel), so
    /// its completion is still parked — a fire-and-forget session that hits its
    /// timeout must reach the heartbeat, or its agent-core background session is
    /// stuck Running forever. Only a caller-initiated cancel parks nothing.
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

    /// Turn a reaped command into its final response: the run publishes (normal
    /// completion) or discards (cancel) via the workspace lifecycle helpers, then
    /// persists the final response. Routing cancel to the discard branch is the
    /// structural guarantee that a cancelled command never reaches the OCC merge.
    fn finish_reaped(
        &self,
        run: Arc<WorkspaceRun>,
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
        // A kill of EITHER reason (cancel or timeout) DISCARDS — only a natural
        // exit (`kill == None`) takes the publish branch in `settle_*`.
        self.finalize_run(run, request, reaped.kill.is_some(), publish_completion)
    }

    fn finalize_run(
        &self,
        run: Arc<WorkspaceRun>,
        request: FinalizeCommandRequest,
        cancelled: bool,
        publish_completion: bool,
    ) -> CommandResponse {
        let ports = &*self.ports;
        let outcome = match &*run {
            WorkspaceRun::Ephemeral(ephemeral) => {
                settle_ephemeral(ports, ephemeral, request, cancelled)
            }
            WorkspaceRun::Isolated(isolated) => {
                settle_isolated(ports, isolated, request, cancelled)
            }
        };
        let response = match outcome {
            Ok(outcome) => CommandResponse::from_workspace_outcome(outcome),
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

/// Complete (publish) or discard (cancel) an ephemeral overlay run. Both paths
/// remove the run dirs and release the snapshot lease; only the complete path
/// captures + publishes the upperdir, so a cancelled command never OCC-merges.
fn settle_ephemeral(
    ports: &dyn WorkspaceRunHostPorts,
    run: &EphemeralRun,
    request: FinalizeCommandRequest,
    cancelled: bool,
) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
    let root = run.workspace.layer_stack_root.0.clone();
    let lease_id = run.workspace.snapshot.lease_id.clone();
    // Compute the outcome, then ALWAYS remove the run dirs + release the lease —
    // on completion (after publish), on cancel (discard, never published), and
    // even if shaping the completion errors. Cleanup must not depend on success.
    let outcome = if cancelled {
        Ok(WorkspaceCommandOutcome::discarded(
            WorkspaceMode::Ephemeral,
            request,
        ))
    } else {
        ports.base_timings(&root).and_then(|base_timings| {
            ports.finalize_ephemeral(&root, run.workspace.clone(), base_timings, request)
        })
    };
    discard_ephemeral_command(&run.workspace.dirs);
    release_lease(&root, &lease_id);
    outcome
}

/// Complete (capture for audit) or discard (cancel) one isolated command
/// session. Isolated writes are never published; the upperdir is torn down with
/// the namespace on exit, so discard is a no-op beyond the cancelled outcome.
fn settle_isolated(
    ports: &dyn WorkspaceRunHostPorts,
    run: &IsolatedRun,
    request: FinalizeCommandRequest,
    cancelled: bool,
) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
    if cancelled {
        return Ok(WorkspaceCommandOutcome::discarded(
            WorkspaceMode::Isolated,
            request,
        ));
    }
    let base_timings = ports.base_timings(&run.handle.layer_stack_root)?;
    let context = IsolatedCommandFinalizeContext {
        caller_id: run.handle.caller_id.clone(),
        workspace_handle_id: run.handle.workspace_handle_id.clone(),
        manifest_version: run.handle.manifest_version,
        manifest_root_hash: run.handle.manifest_root_hash.clone(),
        upperdir: run.handle.upperdir.clone(),
        base_timings,
    };
    let mut outcome = finalize_isolated_command(context, request)?;
    let audit = take_isolated_audit(&mut outcome);
    ports.record_tool_call(&run.handle.caller_id, audit);
    Ok(outcome)
}

fn release_lease(root: &Path, lease_id: &str) {
    let _ =
        LayerStack::open(root.to_path_buf()).and_then(|mut stack| stack.release_lease(lease_id));
}

fn layerstack_error(error: impl std::fmt::Display) -> CommandSessionError {
    CommandSessionError::Workspace(WorkspaceApiError::new(
        "snapshot_acquire_failed",
        error.to_string(),
    ))
}

fn workspace_api_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("daemon_command_workspace_error", error.to_string())
}

fn is_teardown_control(chars: &str) -> bool {
    matches!(chars, "\u{3}" | "\u{4}")
}

fn contains_teardown_control(chars: &str) -> bool {
    chars.contains('\u{3}') || chars.contains('\u{4}')
}
