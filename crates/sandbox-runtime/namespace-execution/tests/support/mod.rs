//! Shared test fixtures for the engine suite: fakes for the `pub(crate)`
//! launcher seam plus small constructors.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use sandbox_observability::{SpanStatus, TerminalHook};

use crate::launcher::{NsRunnerLauncher, RunnerChild};
use crate::pty::{open_pty_pair, PtyMaster};
use crate::{
    NamespaceExecutionError, NamespaceExecutionId, NamespaceTarget, RunnerOutcome, ShellOperation,
};
use sandbox_runtime_namespace_process::runner::protocol::{
    Fd, NamespaceRunnerRequest, NsFds, RunResult,
};

/// A controllable completion cell the test drives. `complete`/`cancel` unblock a
/// `FakeRunnerChild` blocked in `wait_completion` — a real concurrent unblock.
struct FakeCompletion {
    slot: Mutex<Option<FakeCompletionOutcome>>,
    ready: Condvar,
}

#[derive(Clone)]
enum FakeCompletionOutcome {
    Completed(RunResult),
    Failed(String),
}

impl FakeCompletion {
    fn new() -> Self {
        Self {
            slot: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    fn complete(&self, result: RunResult) {
        self.finish(FakeCompletionOutcome::Completed(result));
    }

    fn fail(&self, detail: impl Into<String>) {
        self.finish(FakeCompletionOutcome::Failed(detail.into()));
    }

    fn finish(&self, outcome: FakeCompletionOutcome) {
        let mut slot = self.slot.lock().expect("fake completion mutex poisoned");
        if slot.is_none() {
            *slot = Some(outcome);
            self.ready.notify_all();
        }
    }

    fn cancel(&self) {
        self.complete(RunResult {
            exit_code: 130,
            payload: serde_json::json!({ "status": "cancelled" }),
        });
    }

    fn wait(&self) -> Result<RunResult, NamespaceExecutionError> {
        let mut slot = self.slot.lock().expect("fake completion mutex poisoned");
        while slot.is_none() {
            slot = self
                .ready
                .wait(slot)
                .expect("fake completion mutex poisoned");
        }
        match slot
            .clone()
            .expect("wait loop exits only once the slot is set")
        {
            FakeCompletionOutcome::Completed(result) => Ok(result),
            FakeCompletionOutcome::Failed(detail) => Err(NamespaceExecutionError::Spawn(detail)),
        }
    }
}

struct FakeRunnerChild {
    completion: Arc<FakeCompletion>,
    _slave: Option<File>,
}

impl RunnerChild for FakeRunnerChild {
    fn wait_completion(&mut self) -> Result<RunResult, NamespaceExecutionError> {
        self.completion.wait()
    }
}

/// One scripted `spawn_pty` outcome, applied at spawn time so callers can
/// observe running, completed, failed-spawn, and process-group states.
#[derive(Default)]
pub struct FakeRunnerScript {
    output: Vec<u8>,
    completion: Option<RunResult>,
    pgid: Option<i32>,
    spawn_error: Option<NamespaceExecutionError>,
}

impl FakeRunnerScript {
    #[must_use]
    pub fn running(output: impl Into<Vec<u8>>) -> Self {
        Self {
            output: output.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn completes(result: RunResult) -> Self {
        Self {
            completion: Some(result),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn completes_with_output(output: impl Into<Vec<u8>>, result: RunResult) -> Self {
        Self {
            output: output.into(),
            completion: Some(result),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn pending() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn spawn_error(error: NamespaceExecutionError) -> Self {
        Self {
            spawn_error: Some(error),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_pgid(mut self, pgid: i32) -> Self {
        self.pgid = Some(pgid);
        self
    }
}

#[derive(Default)]
struct FakeLauncherState {
    requests: Vec<NamespaceRunnerRequest>,
    request_ids: Vec<String>,
    transcript_paths: Vec<Option<PathBuf>>,
    completions: Vec<Arc<FakeCompletion>>,
    scripts: VecDeque<FakeRunnerScript>,
    overlay_mount_setup_timeouts: Vec<f64>,
}

/// A fake `NsRunnerLauncher`: records each request, hands back a `FakeRunnerChild`
/// bound to a fresh `FakeCompletion`, and for `spawn_pty` builds a real-`openpt`
/// `PtyMaster` whose cancel trips that completion. Cloneable — the engine holds
/// one clone, the test another, sharing the recorded state.
#[derive(Clone, Default)]
pub struct FakeLauncher {
    state: Arc<Mutex<FakeLauncherState>>,
}

impl FakeLauncher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_script(&self, script: FakeRunnerScript) {
        self.lock().scripts.push_back(script);
    }

    #[must_use]
    pub fn recorded_request_ids(&self) -> Vec<String> {
        self.lock().request_ids.clone()
    }

    #[must_use]
    pub fn recorded_requests(&self) -> Vec<NamespaceRunnerRequest> {
        self.lock().requests.clone()
    }

    #[must_use]
    pub fn recorded_transcript_paths(&self) -> Vec<Option<PathBuf>> {
        self.lock().transcript_paths.clone()
    }

    #[must_use]
    pub fn overlay_mount_setup_timeouts(&self) -> Vec<f64> {
        self.lock().overlay_mount_setup_timeouts.clone()
    }

    /// Complete the most recently spawned execution.
    pub fn complete_latest(&self, result: RunResult) {
        if let Some(completion) = self.latest_completion() {
            completion.complete(result);
        }
    }

    /// Fail the most recently spawned execution's `wait_completion`.
    pub fn fail_latest_wait(&self, detail: impl Into<String>) {
        let detail = detail.into();
        if let Some(completion) = self.latest_completion() {
            completion.fail(detail);
        }
    }

    fn latest_completion(&self) -> Option<Arc<FakeCompletion>> {
        self.lock().completions.last().map(Arc::clone)
    }

    fn record(
        &self,
        request: &NamespaceRunnerRequest,
        transcript_path: Option<PathBuf>,
    ) -> (Arc<FakeCompletion>, FakeRunnerScript) {
        let completion = Arc::new(FakeCompletion::new());
        let mut state = self.lock();
        state.requests.push(request.clone());
        state.request_ids.push(request.request_id.clone());
        state.transcript_paths.push(transcript_path);
        state.completions.push(Arc::clone(&completion));
        let script = state.scripts.pop_front().unwrap_or_default();
        (completion, script)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeLauncherState> {
        self.state.lock().expect("fake launcher mutex poisoned")
    }
}

impl NsRunnerLauncher for FakeLauncher {
    fn spawn_pty(
        &self,
        request: NamespaceRunnerRequest,
        transcript_path: Option<PathBuf>,
        cancelled: Arc<AtomicBool>,
        _cgroup_procs_path: Option<PathBuf>,
    ) -> Result<(Box<dyn RunnerChild>, PtyMaster), NamespaceExecutionError> {
        let (completion, script) = self.record(&request, transcript_path.clone());
        if let Some(error) = script.spawn_error {
            return Err(error);
        }
        if !script.output.is_empty() {
            if let Some(path) = transcript_path.as_deref() {
                append_transcript(path, &script.output);
            }
        }
        let (master, slave) =
            open_pty_pair().map_err(|error| NamespaceExecutionError::Spawn(error.to_string()))?;
        let cancel = {
            let completion = Arc::clone(&completion);
            move || {
                cancelled.store(true, Ordering::Release);
                completion.cancel();
            }
        };
        let pty = PtyMaster::spawn(master, script.pgid, transcript_path, Box::new(cancel))
            .map_err(|error| NamespaceExecutionError::Spawn(error.to_string()))?;
        if let Some(result) = script.completion {
            completion.complete(result);
        }
        Ok((
            Box::new(FakeRunnerChild {
                completion,
                _slave: Some(slave),
            }),
            pty,
        ))
    }

    fn spawn_overlay_mount(
        &self,
        request: NamespaceRunnerRequest,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError> {
        let (completion, _script) = self.record(&request, None);
        let mut state = self.lock();
        state.overlay_mount_setup_timeouts.push(setup_timeout_s);
        Ok(Box::new(FakeRunnerChild {
            completion,
            _slave: None,
        }))
    }
}

fn append_transcript(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(bytes);
    }
}

/// A recorded terminal edge: which execution finished, with what span status and
/// exit code. The `TerminalHook` terminal edge is the only one the engine emits;
/// live "running" state stays in the engine's own `ExecutionRegistry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRecord {
    pub id: NamespaceExecutionId,
    pub status: SpanStatus,
    pub exit_code: Option<i64>,
}

/// Records `on_terminal` calls; `await_terminal` blocks until the terminal edge
/// lands (the watcher fires it right after `wait_completion`, before finalize).
pub struct FakeObserver {
    events: Mutex<Vec<TerminalRecord>>,
    terminal: Condvar,
}

impl FakeObserver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            terminal: Condvar::new(),
        }
    }

    #[must_use]
    pub fn events(&self) -> Vec<TerminalRecord> {
        self.events
            .lock()
            .expect("fake observer mutex poisoned")
            .clone()
    }

    pub fn await_terminal(&self) -> (SpanStatus, Option<i64>) {
        let mut events = self.events.lock().expect("fake observer mutex poisoned");
        loop {
            if let Some(terminal) = events.last() {
                return (terminal.status, terminal.exit_code);
            }
            events = self
                .terminal
                .wait(events)
                .expect("fake observer mutex poisoned");
        }
    }
}

impl Default for FakeObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalHook<NamespaceExecutionId> for FakeObserver {
    fn on_terminal(&self, id: &NamespaceExecutionId, status: SpanStatus, exit_code: Option<i64>) {
        self.events
            .lock()
            .expect("fake observer mutex poisoned")
            .push(TerminalRecord {
                id: id.clone(),
                status,
                exit_code,
            });
        self.terminal.notify_all();
    }
}

/// A shell op whose `finalize` succeeds, yielding the runner exit code.
#[derive(Default)]
pub struct OkShellOp;

impl ShellOperation for OkShellOp {
    type Output = i64;

    fn operation_name(&self) -> &'static str {
        "ok_shell_op"
    }

    fn command(&self) -> &str {
        "true"
    }

    fn timeout_seconds(&self) -> Option<f64> {
        None
    }

    fn finalize(self: Box<Self>, outcome: RunnerOutcome) -> Result<i64, NamespaceExecutionError> {
        Ok(outcome.exit_code())
    }
}

/// A shell op with non-default request fields.
#[derive(Default)]
pub struct TimedShellOp;

impl ShellOperation for TimedShellOp {
    type Output = i64;

    fn operation_name(&self) -> &'static str {
        "timed_shell_op"
    }

    fn command(&self) -> &str {
        "printf ready"
    }

    fn timeout_seconds(&self) -> Option<f64> {
        Some(2.5)
    }

    fn finalize(self: Box<Self>, outcome: RunnerOutcome) -> Result<i64, NamespaceExecutionError> {
        Ok(outcome.exit_code())
    }
}

/// A shell op whose `finalize` rejects the outcome.
#[derive(Default)]
pub struct ErrShellOp;

impl ShellOperation for ErrShellOp {
    type Output = i64;

    fn operation_name(&self) -> &'static str {
        "err_shell_op"
    }

    fn command(&self) -> &str {
        "false"
    }

    fn timeout_seconds(&self) -> Option<f64> {
        None
    }

    fn finalize(self: Box<Self>, _outcome: RunnerOutcome) -> Result<i64, NamespaceExecutionError> {
        Err(NamespaceExecutionError::Finalize("err shell op".to_owned()))
    }
}

/// A shell op whose `finalize` panics.
#[derive(Default)]
pub struct PanicShellOp;

impl ShellOperation for PanicShellOp {
    type Output = i64;

    fn operation_name(&self) -> &'static str {
        "panic_shell_op"
    }

    fn command(&self) -> &str {
        "panic"
    }

    fn timeout_seconds(&self) -> Option<f64> {
        None
    }

    fn finalize(self: Box<Self>, _outcome: RunnerOutcome) -> Result<i64, NamespaceExecutionError> {
        panic!("panic shell op")
    }
}

#[must_use]
pub fn run_result(exit_code: i32, status: &str) -> RunResult {
    RunResult {
        exit_code,
        payload: serde_json::json!({ "status": status }),
    }
}

#[must_use]
pub fn sample_target() -> NamespaceTarget {
    NamespaceTarget {
        workspace_root: PathBuf::from("/workspace"),
        layer_paths: vec![PathBuf::from("/layers/base"), PathBuf::from("/layers/top")],
        upperdir: Some(PathBuf::from("/overlay/upper")),
        workdir: Some(PathBuf::from("/overlay/work")),
        ns_fds: NsFds {
            user: Some(Fd(11)),
            mnt: Some(Fd(12)),
            pid: Some(Fd(13)),
            net: Some(Fd(14)),
        },
    }
}
