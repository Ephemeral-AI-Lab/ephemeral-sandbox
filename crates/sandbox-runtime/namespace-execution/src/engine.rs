use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use sandbox_runtime_namespace_process::runner::protocol::NamespaceRunnerRequest;
use serde_json::Value;

use crate::error::NamespaceExecutionError;
use crate::execution::{ExecutionHandle, InteractiveExecution};
use crate::launcher::{ForkRunnerLauncher, NsRunnerLauncher, RunnerChild};
use crate::promise::CompletionPromise;
use crate::registry::ExecutionRegistry;
use crate::shell::{NamespaceExecutionTerminalStatus, RunnerOutcome, ShellOperation};
use crate::types::{ExecutionObserver, NamespaceExecutionId, NamespaceTarget};

pub struct NamespaceExecutionEngine<V = ()> {
    registry: Arc<ExecutionRegistry<V>>,
    observer: Arc<dyn ExecutionObserver>,
    launcher: Box<dyn NsRunnerLauncher>,
    next_id: AtomicU64,
    setup_timeout_s: f64,
}

impl<V: Send + 'static> NamespaceExecutionEngine<V> {
    #[must_use]
    pub fn new(
        observer: Arc<dyn ExecutionObserver>,
        max_active: usize,
        setup_timeout_s: f64,
    ) -> Self {
        Self::with_launcher(
            Box::new(ForkRunnerLauncher),
            observer,
            max_active,
            setup_timeout_s,
        )
    }

    pub fn with_launcher(
        launcher: Box<dyn NsRunnerLauncher>,
        observer: Arc<dyn ExecutionObserver>,
        max_active: usize,
        setup_timeout_s: f64,
    ) -> Self {
        Self {
            registry: Arc::new(ExecutionRegistry::new(max_active)),
            observer,
            launcher,
            next_id: AtomicU64::new(1),
            setup_timeout_s,
        }
    }

    #[must_use]
    pub fn allocate_id(&self) -> NamespaceExecutionId {
        let next_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        NamespaceExecutionId(format!("namespace_execution_{next_id}"))
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

    pub fn run_shell_interactive<S: ShellOperation>(
        &self,
        op: S,
        target: NamespaceTarget,
        id: NamespaceExecutionId,
    ) -> Result<InteractiveExecution<S::Output>, NamespaceExecutionError> {
        let request = build_request(&target, &id, shell_args(op.command()), op.timeout_seconds());
        let transcript_path = op.transcript_path().map(Path::to_path_buf);
        let cancelled = Arc::new(AtomicBool::new(false));
        let op = Box::new(op);
        let (child, pty) = self.reserve_spawn(&id, || {
            self.launcher
                .spawn_pty(request, transcript_path, Arc::clone(&cancelled))
        })?;
        self.observer.on_running(&id);
        let promise = Arc::new(CompletionPromise::new());
        self.spawn_watcher(
            id.clone(),
            child,
            Arc::clone(&promise),
            cancelled,
            None,
            move |outcome| op.finalize(outcome),
        );
        Ok(InteractiveExecution::new(
            ExecutionHandle::new(id, promise),
            pty,
        ))
    }

    pub fn run_mount<O: Send + 'static>(
        &self,
        mode_flag: &'static str,
        target: NamespaceTarget,
        id: NamespaceExecutionId,
        args: Value,
        parse: impl FnOnce(RunnerOutcome) -> Result<O, NamespaceExecutionError> + Send + 'static,
    ) -> Result<ExecutionHandle<O>, NamespaceExecutionError> {
        let request = build_request(&target, &id, args, None);
        let child = self.reserve_spawn(&id, || {
            self.launcher
                .spawn_piped(mode_flag, request, self.setup_timeout_s)
        })?;
        self.observer.on_running(&id);
        let promise = Arc::new(CompletionPromise::new());
        self.spawn_watcher(
            id.clone(),
            child,
            Arc::clone(&promise),
            Arc::new(AtomicBool::new(false)),
            Some(mode_flag),
            parse,
        );
        Ok(ExecutionHandle::new(id, promise))
    }

    fn reserve_spawn<R>(
        &self,
        id: &NamespaceExecutionId,
        spawn: impl FnOnce() -> Result<R, NamespaceExecutionError>,
    ) -> Result<R, NamespaceExecutionError> {
        self.registry.try_reserve(id)?;
        match spawn() {
            Ok(spawned) => Ok(spawned),
            Err(error) => {
                self.registry.abort(id);
                Err(error)
            }
        }
    }

    fn spawn_watcher<O: Send + 'static>(
        &self,
        id: NamespaceExecutionId,
        mut child: Box<dyn RunnerChild>,
        promise: Arc<CompletionPromise<O>>,
        cancelled: Arc<AtomicBool>,
        mount_error_mode: Option<&'static str>,
        finalize: impl FnOnce(RunnerOutcome) -> Result<O, NamespaceExecutionError> + Send + 'static,
    ) {
        let registry = Arc::clone(&self.registry);
        let observer = Arc::clone(&self.observer);
        thread::spawn(move || {
            let (result, status, exit_code) = match child.wait_completion() {
                Ok(run_result) => {
                    let outcome = RunnerOutcome::new(run_result)
                        .with_cancelled(cancelled.load(Ordering::Acquire));
                    let status = outcome.status();
                    let exit_code = Some(outcome.exit_code());
                    let result = mount_exit_error(mount_error_mode, &outcome)
                        .map_or_else(|| finalize_outcome(finalize, outcome), Err);
                    let status = if result.is_ok() {
                        status
                    } else {
                        NamespaceExecutionTerminalStatus::Error
                    };
                    (result, status, exit_code)
                }
                Err(error) => (Err(error), NamespaceExecutionTerminalStatus::Error, None),
            };
            registry.complete(&id, status, exit_code);
            promise.resolve(result);
            observer.on_terminal(&id, status, exit_code);
        });
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
) -> NamespaceRunnerRequest {
    NamespaceRunnerRequest {
        request_id: id.0.clone(),
        args,
        workspace_root: target.workspace_root.clone(),
        layer_paths: target.layer_paths.clone(),
        upperdir: target.upperdir.clone(),
        workdir: target.workdir.clone(),
        ns_fds: Some(target.ns_fds),
        timeout_seconds,
    }
}
