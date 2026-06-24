use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use sandbox_runtime_namespace_process::runner::protocol::NamespaceRunnerRequest;
use serde_json::Value;

use crate::error::NamespaceExecutionError;
use crate::execution::{ExecutionHandle, InteractiveExecution};
use crate::id::NamespaceExecutionId;
use crate::launcher::{ForkRunnerLauncher, NsRunnerLauncher, RunnerChild};
use crate::observer::ExecutionObserver;
use crate::promise::CompletionPromise;
use crate::registry::ExecutionRegistry;
use crate::shell::{RunnerOutcome, ShellOperation};
use crate::status::NamespaceExecutionTerminalStatus;
use crate::target::NamespaceTarget;

/// Strategy + Template-Method core: holds the registry, observer, and boxed
/// launcher (the Bridge seam, §2.1). Both entry points share one dispatch spine;
/// the engine knows nothing of shell-vs-mount beyond which launcher method and
/// finalizer it is handed.
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
        Self {
            registry: Arc::new(ExecutionRegistry::new(max_active)),
            observer,
            launcher: Box::new(ForkRunnerLauncher),
            next_id: AtomicU64::new(1),
            setup_timeout_s,
        }
    }

    #[cfg(feature = "test-support")]
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

    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn registry_is_completed(&self, id: &NamespaceExecutionId) -> bool {
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

    /// PTY-backed shell execution. The runner runs in `Run` mode (no mode flag).
    pub fn run_shell_interactive<S: ShellOperation>(
        &self,
        op: S,
        target: NamespaceTarget,
        id: NamespaceExecutionId,
    ) -> Result<InteractiveExecution<S::Output>, NamespaceExecutionError> {
        self.registry.try_reserve(&id)?;
        let request = build_request(&target, &id, shell_args(op.command()), op.timeout_seconds());
        let op = Box::new(op);
        let (child, pty) = match self.launcher.spawn_pty(request) {
            Ok(spawned) => spawned,
            Err(error) => {
                self.registry.abort(&id);
                return Err(error);
            }
        };
        self.observer.on_running(&id);
        let promise = Arc::new(CompletionPromise::new());
        self.spawn_watcher(
            id.clone(),
            child,
            Arc::clone(&promise),
            MountExitHandling::AllowNonZero,
            move |outcome| op.finalize(outcome),
        );
        Ok(InteractiveExecution::new(
            ExecutionHandle::new(id, promise),
            pty,
        ))
    }

    /// Pipe-backed mount/remount execution. `mode_flag` selects the runner mode
    /// (`--mount-overlay` / `--remount-overlay`); `parse` projects the outcome.
    pub fn run_mount<O: Send + 'static>(
        &self,
        mode_flag: &'static str,
        target: NamespaceTarget,
        id: NamespaceExecutionId,
        args: Value,
        parse: impl FnOnce(RunnerOutcome) -> Result<O, NamespaceExecutionError> + Send + 'static,
    ) -> Result<ExecutionHandle<O>, NamespaceExecutionError> {
        self.registry.try_reserve(&id)?;
        let request = build_request(&target, &id, args, None);
        let child = match self
            .launcher
            .spawn_piped(mode_flag, request, self.setup_timeout_s)
        {
            Ok(child) => child,
            Err(error) => {
                self.registry.abort(&id);
                return Err(error);
            }
        };
        self.observer.on_running(&id);
        let promise = Arc::new(CompletionPromise::new());
        self.spawn_watcher(
            id.clone(),
            child,
            Arc::clone(&promise),
            MountExitHandling::ShortCircuitNonZero { mode_flag },
            parse,
        );
        Ok(ExecutionHandle::new(id, promise))
    }

    /// The watcher thread: one blocking `wait_completion`, then finalize inline,
    /// `complete` BEFORE `resolve` (so promise-resolved ⟹ the completed entry
    /// exists), then `on_terminal`. No poll loops.
    fn spawn_watcher<O: Send + 'static>(
        &self,
        id: NamespaceExecutionId,
        mut child: Box<dyn RunnerChild>,
        promise: Arc<CompletionPromise<O>>,
        mount_exit_handling: MountExitHandling,
        finalize: impl FnOnce(RunnerOutcome) -> Result<O, NamespaceExecutionError> + Send + 'static,
    ) {
        let registry = Arc::clone(&self.registry);
        let observer = Arc::clone(&self.observer);
        thread::spawn(move || {
            let (result, status, exit_code) = match child.wait_completion() {
                Ok(run_result) => {
                    let outcome = RunnerOutcome::new(run_result);
                    let status = outcome.status();
                    let exit_code = Some(outcome.exit_code());
                    if let Some(error) = mount_exit_handling.short_circuit_error(&outcome) {
                        (
                            Err(error),
                            NamespaceExecutionTerminalStatus::Error,
                            exit_code,
                        )
                    } else {
                        match finalize_outcome(finalize, outcome) {
                            Ok(output) => (Ok(output), status, exit_code),
                            Err(error) => (
                                Err(error),
                                NamespaceExecutionTerminalStatus::Error,
                                exit_code,
                            ),
                        }
                    }
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
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[derive(Clone, Copy)]
enum MountExitHandling {
    AllowNonZero,
    ShortCircuitNonZero { mode_flag: &'static str },
}

impl MountExitHandling {
    fn short_circuit_error(&self, outcome: &RunnerOutcome) -> Option<NamespaceExecutionError> {
        match self {
            Self::AllowNonZero => None,
            Self::ShortCircuitNonZero { mode_flag } if outcome.exit_code() != 0 => {
                Some(NamespaceExecutionError::Finalize(format!(
                    "namespace runner {mode_flag} failed with exit code {}: {}",
                    outcome.exit_code(),
                    mount_failure_detail(outcome.payload())
                )))
            }
            Self::ShortCircuitNonZero { .. } => None,
        }
    }
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
