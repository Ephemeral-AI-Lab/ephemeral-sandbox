//! Test-local fakes for the launcher Bridge seam. Downstream integration suites
//! can build an engine over a scripted fake runner without reaching the real fork
//! path. Two drive modes share one type:
//!
//! - **manual** (engine suites): each `spawn_pty`/`spawn_overlay_mount` parks a
//!   pending child; the test later `complete_latest`/`fail_latest_wait`.
//! - **scripted** (command/operation suites): push a [`FakeRunnerScript`] per
//!   spawn upfront; `spawn_pty` applies it (writes transcript output, completes or
//!   parks, sets a pgid) so the real condvar yield loop observes it.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use sandbox_runtime_namespace_process::runner::protocol::{NamespaceRunnerRequest, RunResult};

use sandbox_runtime_namespace_execution::{
    open_pty_pair, NamespaceExecutionError, NsRunnerLauncher, PtyMaster, RunnerChild,
    RunnerPlacement,
};

/// A controllable completion cell the test drives. `complete`/`fail`/`cancel`
/// unblock a `FakeRunnerChild` blocked in `wait_completion` — a real concurrent
/// unblock through a genuine `Condvar`.
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

    fn try_result(&self) -> Result<Option<RunResult>, NamespaceExecutionError> {
        let slot = self.slot.lock().expect("fake completion mutex poisoned");
        match slot.clone() {
            Some(FakeCompletionOutcome::Completed(result)) => Ok(Some(result)),
            Some(FakeCompletionOutcome::Failed(detail)) => {
                Err(NamespaceExecutionError::Spawn(detail))
            }
            None => Ok(None),
        }
    }
}

struct FakeRunnerChild {
    completion: Arc<FakeCompletion>,
    _slave: Option<File>,
}

impl RunnerChild for FakeRunnerChild {
    fn wait_completion(&mut self) -> Result<RunResult, NamespaceExecutionError> {
        let result = self.completion.wait();
        self._slave.take();
        result
    }

    fn try_wait_completion(&mut self) -> Result<Option<RunResult>, NamespaceExecutionError> {
        let result = self.completion.try_result();
        if !matches!(&result, Ok(None)) {
            self._slave.take();
        }
        result
    }

    fn terminate(&mut self) {
        self.completion.cancel();
    }
}

/// One scripted `spawn_pty` outcome, applied at spawn time so the caller's yield
/// loop observes it without the test having to race the watcher.
#[derive(Default)]
pub struct FakeRunnerScript {
    output: Vec<u8>,
    completion: Option<RunResult>,
    pgid: Option<i32>,
    spawn_error: Option<NamespaceExecutionError>,
    ignore_cancel: bool,
}

impl FakeRunnerScript {
    /// Write `output` to the transcript and park the child (stays running).
    #[must_use]
    pub fn running(output: impl Into<Vec<u8>>) -> Self {
        Self {
            output: output.into(),
            ..Self::default()
        }
    }

    /// Complete the child immediately with `result` (no transcript output).
    #[must_use]
    pub fn completes(result: RunResult) -> Self {
        Self {
            completion: Some(result),
            ..Self::default()
        }
    }

    /// Write `output` to the transcript, then complete with `result`.
    #[must_use]
    pub fn completes_with_output(output: impl Into<Vec<u8>>, result: RunResult) -> Self {
        Self {
            output: output.into(),
            completion: Some(result),
            ..Self::default()
        }
    }

    /// Park the child with no output (the yield loop times out to Running).
    #[must_use]
    pub fn pending() -> Self {
        Self::default()
    }

    /// Park the child and record cancellation without completing it. This
    /// injects a stuck runner so teardown timeout/retry behavior can be proven.
    #[must_use]
    pub fn pending_ignoring_cancel() -> Self {
        Self {
            ignore_cancel: true,
            ..Self::default()
        }
    }

    /// Fail the spawn itself.
    #[must_use]
    pub fn spawn_error(error: NamespaceExecutionError) -> Self {
        Self {
            spawn_error: Some(error),
            ..Self::default()
        }
    }

    /// Give the spawned `PtyMaster` a process group id.
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
    cancel_request_ids: Vec<String>,
    transcript_paths: Vec<Option<PathBuf>>,
    cgroup_procs_paths: Vec<Option<PathBuf>>,
    completions: Vec<Arc<FakeCompletion>>,
    scripts: VecDeque<FakeRunnerScript>,
    overlay_mount_setup_timeouts: Vec<f64>,
    file_op_setup_timeouts: Vec<f64>,
    cancel_after_completion_entered: Option<Sender<()>>,
    cancel_after_completion_release: Option<Receiver<()>>,
}

/// A fake `NsRunnerLauncher`: records each request, hands back a `FakeRunnerChild`
/// bound to a fresh `FakeCompletion`, and for `spawn_pty` builds a real-`openpt`
/// `PtyMaster` whose cancel trips the `cancelled` flag and that completion.
/// Cloneable — the engine holds one clone, the test another, sharing state.
#[derive(Clone, Default)]
pub struct FakeLauncher {
    state: Arc<Mutex<FakeLauncherState>>,
}

impl FakeLauncher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a scripted outcome for the next `spawn_pty` (FIFO).
    pub fn push_script(&self, script: FakeRunnerScript) {
        self.lock().scripts.push_back(script);
    }

    /// Park the next cancellation after its completion has been published.
    /// This exposes the interval where the command ledger can drain while the
    /// holder-exit transaction is still joining its teardown owner.
    pub fn park_next_cancel_after_completion(&self) -> (Receiver<()>, Sender<()>) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut state = self.lock();
        state.cancel_after_completion_entered = Some(entered_tx);
        state.cancel_after_completion_release = Some(release_rx);
        (entered_rx, release_tx)
    }

    #[must_use]
    pub fn recorded_request_ids(&self) -> Vec<String> {
        self.lock().request_ids.clone()
    }

    #[must_use]
    pub fn recorded_cancel_request_ids(&self) -> Vec<String> {
        self.lock().cancel_request_ids.clone()
    }

    #[must_use]
    pub fn recorded_requests(&self) -> Vec<NamespaceRunnerRequest> {
        self.lock().requests.clone()
    }

    /// The transcript-file path threaded into each `spawn_pty` (in spawn order).
    #[must_use]
    pub fn recorded_transcript_paths(&self) -> Vec<Option<PathBuf>> {
        self.lock().transcript_paths.clone()
    }

    /// The workspace `cgroup.procs` path threaded into each `spawn_pty`.
    #[must_use]
    pub fn recorded_cgroup_procs_paths(&self) -> Vec<Option<PathBuf>> {
        self.lock().cgroup_procs_paths.clone()
    }

    #[must_use]
    pub fn overlay_mount_setup_timeouts(&self) -> Vec<f64> {
        self.lock().overlay_mount_setup_timeouts.clone()
    }

    #[must_use]
    pub fn file_op_setup_timeouts(&self) -> Vec<f64> {
        self.lock().file_op_setup_timeouts.clone()
    }

    /// Complete the most recently spawned execution.
    pub fn complete_latest(&self, result: RunResult) {
        if let Some(completion) = self.latest_completion() {
            completion.complete(result);
        }
    }

    /// Complete the spawned execution recorded for `request_id` (parked
    /// children only; completing an already-completed child is a no-op).
    pub fn complete_request(&self, request_id: &str, result: RunResult) {
        let completion = {
            let state = self.lock();
            state
                .request_ids
                .iter()
                .position(|recorded| recorded == request_id)
                .and_then(|index| state.completions.get(index).map(Arc::clone))
        };
        if let Some(completion) = completion {
            completion.complete(result);
        }
    }

    /// Fail the most recently spawned execution's `wait_completion`.
    pub fn fail_latest_wait(&self, detail: impl Into<String>) {
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
        cgroup_procs_path: Option<PathBuf>,
    ) -> (Arc<FakeCompletion>, FakeRunnerScript) {
        let completion = Arc::new(FakeCompletion::new());
        let mut state = self.lock();
        state.requests.push(request.clone());
        state.request_ids.push(request.request_id.clone());
        state.transcript_paths.push(transcript_path);
        state.cgroup_procs_paths.push(cgroup_procs_path);
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
        placement: RunnerPlacement,
    ) -> Result<(Box<dyn RunnerChild>, PtyMaster), NamespaceExecutionError> {
        let (completion, script) = self.record(
            &request,
            transcript_path.clone(),
            placement.cgroup_procs_path,
        );
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
            let state = Arc::clone(&self.state);
            let request_id = request.request_id.clone();
            let ignore_cancel = script.ignore_cancel;
            move || {
                cancelled.store(true, Ordering::Release);
                let (entered, release) = {
                    let mut state = state.lock().expect("fake launcher mutex poisoned");
                    state.cancel_request_ids.push(request_id.clone());
                    (
                        state.cancel_after_completion_entered.take(),
                        state.cancel_after_completion_release.take(),
                    )
                };
                if !ignore_cancel {
                    completion.cancel();
                }
                if let Some(entered) = entered {
                    let _ = entered.send(());
                }
                if let Some(release) = release {
                    let _ = release.recv_timeout(Duration::from_secs(2));
                }
            }
        };
        let pty = PtyMaster::spawn(
            master,
            script.pgid,
            transcript_path,
            Box::new(cancel),
            Duration::from_secs(2),
        )
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
        placement: RunnerPlacement,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError> {
        let (completion, _script) = self.record(&request, None, placement.cgroup_procs_path);
        self.lock()
            .overlay_mount_setup_timeouts
            .push(setup_timeout_s);
        Ok(Box::new(FakeRunnerChild {
            completion,
            _slave: None,
        }))
    }

    fn spawn_file_op(
        &self,
        request: NamespaceRunnerRequest,
        placement: RunnerPlacement,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError> {
        let (completion, script) = self.record(&request, None, placement.cgroup_procs_path);
        self.lock().file_op_setup_timeouts.push(setup_timeout_s);
        if let Some(error) = script.spawn_error {
            return Err(error);
        }
        if let Some(result) = script.completion {
            completion.complete(result);
        }
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
