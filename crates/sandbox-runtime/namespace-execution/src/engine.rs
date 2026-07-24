use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use sandbox_observability_telemetry::{SpanStatus, TerminalHook, TraceContext};
use sandbox_runtime_namespace_process::runner::protocol::{NamespaceRunnerRequest, RunResult};
use serde_json::Value;

use crate::caps::ExecutionCaps;
use crate::error::NamespaceExecutionError;
use crate::execution::{ExecutionHandle, InteractiveExecution};
use crate::launcher::{
    ForkRunnerLauncher, NsRunnerLauncher, RunnerChild, RunnerPlacement, MOUNT_OVERLAY_MODE_FLAG,
};
use crate::promise::CompletionPromise;
use crate::registry::{ExecutionRegistry, RegistryValueMetrics};
use crate::shell::{NamespaceExecutionTerminalStatus, RunnerOutcome, ShellOperation};
use crate::supervisor::CompletionSupervisor;
use crate::types::{NamespaceExecutionId, NamespaceTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackgroundWorkerSnapshot {
    pub completion_supervisor_threads: usize,
    pub pty_reactor_threads: usize,
    pub active_completions: usize,
    pub active_pty_readers: usize,
}

pub struct NamespaceExecutionEngine<V = ()> {
    registry: Arc<ExecutionRegistry<V>>,
    terminal_hook: Arc<dyn TerminalHook<NamespaceExecutionId>>,
    launcher: Box<dyn NsRunnerLauncher>,
    supervisor: CompletionSupervisor,
    next_id: AtomicU64,
    teardown_join_total: AtomicU64,
    teardown_deadline_total: AtomicU64,
    scratch_bytes_high_water: AtomicU64,
    setup_timeout_s: f64,
}

impl<V: Send + 'static> NamespaceExecutionEngine<V> {
    #[must_use]
    pub fn new(
        terminal_hook: Arc<dyn TerminalHook<NamespaceExecutionId>>,
        caps: ExecutionCaps,
    ) -> Self {
        let engine =
            Self::with_launcher(Box::new(ForkRunnerLauncher::new(caps)), terminal_hook, caps);
        crate::pty::initialize_output_reactor();
        engine
    }

    pub fn with_launcher(
        launcher: Box<dyn NsRunnerLauncher>,
        terminal_hook: Arc<dyn TerminalHook<NamespaceExecutionId>>,
        caps: ExecutionCaps,
    ) -> Self {
        Self {
            registry: Arc::new(ExecutionRegistry::new(
                caps.max_active,
                caps.max_terminal_entries,
            )),
            terminal_hook,
            launcher,
            supervisor: CompletionSupervisor::new(),
            next_id: AtomicU64::new(1),
            teardown_join_total: AtomicU64::new(0),
            teardown_deadline_total: AtomicU64::new(0),
            scratch_bytes_high_water: AtomicU64::new(0),
            setup_timeout_s: caps.setup_timeout_s,
        }
    }

    #[must_use]
    pub fn allocate_id(&self) -> NamespaceExecutionId {
        let next_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        NamespaceExecutionId(format!("namespace_execution_{next_id}"))
    }

    /// Override the registry's terminal-entry retention cap (initialized
    /// from [`ExecutionCaps::max_terminal_entries`]).
    pub fn set_terminal_retention(&self, max_terminal: usize) {
        self.registry.set_terminal_retention(max_terminal);
    }

    #[must_use]
    pub fn is_live(&self, id: &NamespaceExecutionId) -> bool {
        self.registry.is_live(id)
    }

    #[must_use]
    pub fn is_completed(&self, id: &NamespaceExecutionId) -> bool {
        self.registry.is_completed(id)
    }

    pub fn attach(&self, id: &NamespaceExecutionId, value: V) {
        self.registry.attach(id, value);
    }

    pub fn with_value<R>(&self, id: &NamespaceExecutionId, f: impl FnOnce(&V) -> R) -> Option<R> {
        self.registry.with_value(id, f)
    }

    pub fn live_values<R>(&self, f: impl Fn(&V) -> Option<R>) -> Vec<R> {
        self.registry.live_values(f)
    }

    pub fn retained_values<R>(&self, f: impl Fn(&V) -> Option<R>) -> Vec<R> {
        self.registry.retained_values(f)
    }

    pub fn value_metrics(&self, value_units: impl FnMut(&V) -> u64) -> RegistryValueMetrics {
        self.registry.value_metrics(value_units)
    }

    pub fn visit_terminal_values<E>(
        &self,
        visitor: impl FnMut(&V) -> Result<bool, E>,
    ) -> Result<usize, E> {
        self.registry.visit_terminal_values(visitor)
    }

    pub fn remove_terminal_values(&self, predicate: impl FnMut(&V) -> bool) -> usize {
        self.registry.remove_terminal_values(predicate)
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.registry.active_count()
    }

    pub fn record_teardown(&self, join_count: usize, deadline_count: usize) {
        self.teardown_join_total
            .fetch_add(join_count as u64, Ordering::Relaxed);
        self.teardown_deadline_total
            .fetch_add(deadline_count as u64, Ordering::Relaxed);
    }

    #[must_use]
    pub fn teardown_totals(&self) -> (u64, u64) {
        (
            self.teardown_join_total.load(Ordering::Relaxed),
            self.teardown_deadline_total.load(Ordering::Relaxed),
        )
    }

    #[must_use]
    pub fn observe_scratch_bytes_high_water(&self, live_bytes: u64) -> u64 {
        self.scratch_bytes_high_water
            .fetch_max(live_bytes, Ordering::Relaxed);
        self.scratch_bytes_high_water.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn background_worker_snapshot(&self) -> BackgroundWorkerSnapshot {
        let pty = crate::pty::output_reactor_snapshot();
        BackgroundWorkerSnapshot {
            completion_supervisor_threads: self.supervisor.worker_threads(),
            pty_reactor_threads: pty.worker_threads,
            active_completions: self.supervisor.active(),
            active_pty_readers: pty.active_readers,
        }
    }

    pub fn run_shell_interactive<S: ShellOperation>(
        &self,
        op: S,
        target: NamespaceTarget,
        id: NamespaceExecutionId,
        on_complete: impl FnOnce(&Result<S::Output, NamespaceExecutionError>) + Send + 'static,
        cgroup_procs_path: Option<PathBuf>,
        trace_handoff: Option<(TraceContext, PathBuf)>,
    ) -> Result<InteractiveExecution<S::Output>, NamespaceExecutionError> {
        self.run_shell_interactive_attached(
            op,
            target,
            id,
            |_| {},
            on_complete,
            cgroup_procs_path,
            trace_handoff,
        )
    }

    /// Launch a shell operation and synchronously attach its owning registry
    /// value before the completion watcher can publish a terminal edge.
    #[allow(clippy::too_many_arguments)]
    pub fn run_shell_interactive_attached<S: ShellOperation>(
        &self,
        op: S,
        target: NamespaceTarget,
        id: NamespaceExecutionId,
        on_ready: impl FnOnce(InteractiveExecution<S::Output>),
        on_complete: impl FnOnce(&Result<S::Output, NamespaceExecutionError>) + Send + 'static,
        cgroup_procs_path: Option<PathBuf>,
        trace_handoff: Option<(TraceContext, PathBuf)>,
    ) -> Result<InteractiveExecution<S::Output>, NamespaceExecutionError> {
        let request = build_request(
            &target,
            &id,
            shell_args(op.command()),
            op.timeout_seconds(),
            trace_handoff,
        );
        let transcript_path = op.transcript_path().map(Path::to_path_buf);
        let cancelled = Arc::new(AtomicBool::new(false));
        let op = Box::new(op);
        let (child, pty) = self.reserve_spawn(&id, || {
            self.launcher.spawn_pty(
                request,
                transcript_path,
                Arc::clone(&cancelled),
                RunnerPlacement { cgroup_procs_path },
            )
        })?;
        let promise = Arc::new(CompletionPromise::new());
        let terminal_release = pty.terminal_release();
        let execution =
            InteractiveExecution::new(ExecutionHandle::new(id.clone(), Arc::clone(&promise)), pty);
        on_ready(execution.clone());
        self.spawn_watcher(
            id.clone(),
            child,
            Arc::clone(&promise),
            cancelled,
            terminal_release,
            move |outcome| op.finalize(outcome),
            on_complete,
        )?;
        Ok(execution)
    }

    pub fn mount_overlay(
        &self,
        target: NamespaceTarget,
        id: NamespaceExecutionId,
    ) -> Result<ExecutionHandle<()>, NamespaceExecutionError> {
        let request = build_request(&target, &id, serde_json::json!({}), None, None);
        let child = self.reserve_spawn(&id, || {
            self.launcher.spawn_overlay_mount(
                request,
                RunnerPlacement::none(),
                self.setup_timeout_s,
            )
        })?;
        let promise = Arc::new(CompletionPromise::new());
        self.spawn_watcher(
            id.clone(),
            child,
            Arc::clone(&promise),
            Arc::new(AtomicBool::new(false)),
            || {},
            |outcome| mount_exit_error(Some(MOUNT_OVERLAY_MODE_FLAG), &outcome).map_or(Ok(()), Err),
            |_| {},
        )?;
        Ok(ExecutionHandle::new(id, promise))
    }

    /// Launch the staged-switch remount runner in the session namespaces.
    /// Peer of [`Self::mount_overlay`], but the output is the runner's raw
    /// [`RunResult`] payload — the two-boolean report drives the caller's
    /// policy, so the exit code is never treated as a mount failure.
    pub fn remount_overlay(
        &self,
        target: NamespaceTarget,
        id: NamespaceExecutionId,
    ) -> Result<ExecutionHandle<RunResult>, NamespaceExecutionError> {
        let request = build_request(&target, &id, serde_json::json!({}), None, None);
        let child = self.reserve_spawn(&id, || {
            self.launcher.spawn_remount_overlay(
                request,
                RunnerPlacement::none(),
                self.setup_timeout_s,
            )
        })?;
        let promise = Arc::new(CompletionPromise::new());
        self.spawn_watcher(
            id.clone(),
            child,
            Arc::clone(&promise),
            Arc::new(AtomicBool::new(false)),
            || {},
            |outcome| Ok(outcome.into_result()),
            |_| {},
        )?;
        Ok(ExecutionHandle::new(id, promise))
    }

    /// Launch a file operation in the session namespaces. Peer of
    /// [`Self::mount_overlay`]: the same request/result runner launch, but the
    /// output is the runner's raw [`RunResult`] payload (the encoded file-op
    /// result or error), so the exit code is not treated as a mount failure.
    pub fn run_file_op(
        &self,
        target: NamespaceTarget,
        id: NamespaceExecutionId,
        args: Value,
        cgroup_procs_path: Option<PathBuf>,
    ) -> Result<ExecutionHandle<RunResult>, NamespaceExecutionError> {
        let request = build_request(&target, &id, args, None, None);
        let child = self.reserve_spawn(&id, || {
            self.launcher.spawn_file_op(
                request,
                RunnerPlacement { cgroup_procs_path },
                self.setup_timeout_s,
            )
        })?;
        let promise = Arc::new(CompletionPromise::new());
        self.spawn_watcher(
            id.clone(),
            child,
            Arc::clone(&promise),
            Arc::new(AtomicBool::new(false)),
            || {},
            |outcome| Ok(outcome.into_result()),
            |_| {},
        )?;
        Ok(ExecutionHandle::new(id, promise))
    }

    fn reserve_spawn<R>(
        &self,
        id: &NamespaceExecutionId,
        spawn: impl FnOnce() -> Result<R, NamespaceExecutionError>,
    ) -> Result<R, NamespaceExecutionError> {
        self.supervisor.ensure_accepting()?;
        self.registry.try_reserve(id)?;
        match spawn() {
            Ok(spawned) => Ok(spawned),
            Err(error) => {
                self.registry.abort(id);
                Err(error)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_watcher<O: Send + 'static>(
        &self,
        id: NamespaceExecutionId,
        child: Box<dyn RunnerChild>,
        promise: Arc<CompletionPromise<O>>,
        cancelled: Arc<AtomicBool>,
        terminal_release: impl FnOnce() + Send + 'static,
        finalize: impl FnOnce(RunnerOutcome) -> Result<O, NamespaceExecutionError> + Send + 'static,
        on_complete: impl FnOnce(&Result<O, NamespaceExecutionError>) + Send + 'static,
    ) -> Result<(), NamespaceExecutionError> {
        let registry = Arc::clone(&self.registry);
        let terminal_hook = Arc::clone(&self.terminal_hook);
        let abort_id = id.clone();
        let submitted = self.supervisor.submit(child, move |wait_result| {
            let (result, status, exit_code) = match wait_result {
                Ok(run_result) => {
                    let outcome = RunnerOutcome::new(run_result)
                        .with_cancelled(cancelled.load(Ordering::Acquire));
                    let exec_status = outcome.status();
                    let exit_code = Some(outcome.exit_code());
                    terminal_hook.on_terminal(&id, exec_status.to_span_status(), exit_code);
                    let result = finalize_outcome(finalize, outcome);
                    let live_status = if result.is_ok() {
                        exec_status
                    } else {
                        NamespaceExecutionTerminalStatus::Error
                    };
                    (result, live_status, exit_code)
                }
                Err(error) => {
                    terminal_hook.on_terminal(&id, SpanStatus::Error, None);
                    (Err(error), NamespaceExecutionTerminalStatus::Error, None)
                }
            };
            terminal_release();
            registry.complete(&id, status, exit_code);
            let result = notify_completion(on_complete, result);
            promise.resolve(result);
        });
        if let Err(error) = submitted {
            self.registry.abort(&abort_id);
            return Err(error);
        }
        Ok(())
    }

    pub fn shutdown_and_join(&self) -> Result<(), NamespaceExecutionError> {
        self.supervisor.shutdown_and_join()
    }
}

fn notify_completion<O>(
    on_complete: impl FnOnce(&Result<O, NamespaceExecutionError>),
    result: Result<O, NamespaceExecutionError>,
) -> Result<O, NamespaceExecutionError> {
    match catch_unwind(AssertUnwindSafe(|| on_complete(&result))) {
        Ok(()) => result,
        Err(payload) => Err(NamespaceExecutionError::Finalize(format!(
            "completion callback panicked: {}",
            panic_payload_message(payload.as_ref())
        ))),
    }
}

fn finalize_outcome<O>(
    finalize: impl FnOnce(RunnerOutcome) -> Result<O, NamespaceExecutionError>,
    outcome: RunnerOutcome,
) -> Result<O, NamespaceExecutionError> {
    match catch_unwind(AssertUnwindSafe(|| finalize(outcome))) {
        Ok(result) => result,
        Err(payload) => Err(NamespaceExecutionError::Finalize(format!(
            "finalize panicked: {}",
            panic_payload_message(payload.as_ref())
        ))),
    }
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn mount_exit_error(
    mode_flag: Option<&str>,
    outcome: &RunnerOutcome,
) -> Option<NamespaceExecutionError> {
    let mode_flag = mode_flag?;
    (outcome.exit_code() != 0).then(|| {
        NamespaceExecutionError::Finalize(format!(
            "namespace runner {} failed with exit code {}: {}",
            mode_flag,
            outcome.exit_code(),
            mount_failure_detail(outcome.payload())
        ))
    })
}

fn mount_failure_detail(payload: &Value) -> String {
    payload
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| payload.to_string())
}

fn shell_args(command: &str) -> Value {
    serde_json::json!({ "command": command, "cwd": "." })
}

fn build_request(
    target: &NamespaceTarget,
    id: &NamespaceExecutionId,
    args: Value,
    timeout_seconds: Option<f64>,
    trace_handoff: Option<(TraceContext, PathBuf)>,
) -> NamespaceRunnerRequest {
    let (trace, parent, observability_log_path) = match trace_handoff {
        Some((trace, path)) => (
            Some(trace.trace.to_string()),
            trace.parent.as_ref().map(|parent| parent.to_string()),
            Some(path),
        ),
        _ => (None, None, None),
    };
    NamespaceRunnerRequest {
        request_id: id.0.clone(),
        args,
        workspace_root: target.workspace_root.clone(),
        layer_paths: target.layer_paths.clone(),
        upperdir: target.upperdir.clone(),
        workdir: target.workdir.clone(),
        ns_fds: Some(target.ns_fds),
        timeout_seconds,
        trace,
        parent,
        observability_log_path,
    }
}
