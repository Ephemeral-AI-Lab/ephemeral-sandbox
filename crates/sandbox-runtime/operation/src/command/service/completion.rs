use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use sandbox_runtime_command::process::{CommandProcess, CommandProcessExit};

use crate::command::{CommandServiceError, CommandSessionId};
use crate::observability::{
    AsyncTraceSink, CommandFinalizationTraceMetadata, CompletedOperationTrace, OperationTrace,
};
use crate::workspace_session::WorkspaceSessionService;

use super::finalize::complete_terminal_command_with_services;
use super::process_store::CommandProcessStore;

const COMPLETION_POLL: Duration = Duration::from_millis(5);
const QUIET_MS: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCompletionWaitOutcome {
    Completed,
    Running,
}

#[derive(Clone)]
pub(crate) struct CommandCompletionSender {
    tx: mpsc::Sender<CommandCompletion>,
}

impl CommandCompletionSender {
    fn send(&self, completion: CommandCompletion) {
        let _ = self.tx.send(completion);
    }
}

#[derive(Clone)]
pub struct CommandCompletionPromise {
    command_session_id: CommandSessionId,
    origin_request_id: Option<String>,
    sender: CommandCompletionSender,
    state: Arc<CommandCompletionState>,
}

struct CommandCompletionState {
    inner: Mutex<CommandCompletionStateInner>,
}

#[derive(Debug, Default)]
struct CommandCompletionStateInner {
    exited: bool,
}

struct CommandCompletion {
    command_session_id: CommandSessionId,
    origin_request_id: Option<String>,
    process_exit: CommandProcessExit,
}

impl CommandCompletionPromise {
    pub(crate) fn new(
        command_session_id: CommandSessionId,
        sender: CommandCompletionSender,
        origin_request_id: Option<String>,
    ) -> Self {
        Self {
            command_session_id,
            origin_request_id,
            sender,
            state: Arc::new(CommandCompletionState {
                inner: Mutex::new(CommandCompletionStateInner::default()),
            }),
        }
    }

    #[doc(hidden)]
    pub fn start_watcher(&self, process: Arc<CommandProcess>) {
        let promise = self.clone();
        thread::spawn(move || loop {
            if let Some(process_exit) = process.take_exit() {
                promise.resolve(process_exit);
                return;
            }
            thread::sleep(COMPLETION_POLL);
        });
    }

    #[doc(hidden)]
    pub fn resolve(&self, process_exit: CommandProcessExit) {
        let should_send = {
            let mut inner = self.lock_inner();
            if inner.exited {
                false
            } else {
                inner.exited = true;
                true
            }
        };
        if should_send {
            self.sender.send(CommandCompletion {
                command_session_id: self.command_session_id.clone(),
                origin_request_id: self.origin_request_id.clone(),
                process_exit,
            });
        }
    }

    pub(crate) fn is_exited(&self) -> bool {
        self.lock_inner().exited
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, CommandCompletionStateInner> {
        self.state
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) fn spawn_completion_finalizer(
    workspace: Arc<WorkspaceSessionService>,
    process_store: Arc<CommandProcessStore>,
    async_trace_sink: Option<AsyncTraceSink>,
) -> CommandCompletionSender {
    let (tx, rx) = mpsc::channel::<CommandCompletion>();
    thread::spawn(move || {
        for completion in rx {
            finalize_completion(
                workspace.as_ref(),
                process_store.as_ref(),
                completion,
                async_trace_sink.as_ref(),
            );
        }
    });
    CommandCompletionSender { tx }
}

fn finalize_completion(
    workspace: &WorkspaceSessionService,
    process_store: &CommandProcessStore,
    completion: CommandCompletion,
    async_trace_sink: Option<&AsyncTraceSink>,
) {
    let Some(origin_request_id) = completion.origin_request_id.clone() else {
        let outcome = complete_terminal_command_with_services(
            workspace,
            process_store,
            completion.command_session_id,
            completion.process_exit,
            None,
        );
        let _ = outcome.result;
        return;
    };
    let Some(async_trace_sink) = async_trace_sink else {
        let outcome = complete_terminal_command_with_services(
            workspace,
            process_store,
            completion.command_session_id,
            completion.process_exit,
            None,
        );
        let _ = outcome.result;
        return;
    };

    let command_session_id = completion.command_session_id.clone();
    let trace = OperationTrace::new();
    let outcome = complete_terminal_command_with_services(
        workspace,
        process_store,
        completion.command_session_id,
        completion.process_exit,
        Some(&trace),
    );
    let metadata = CommandFinalizationTraceMetadata {
        origin_request_id,
        workspace_session_id: outcome.workspace_session_id.clone(),
        command_session_id,
        finalizer_status: if outcome.result.is_ok() {
            "ok"
        } else {
            "error"
        },
        finalizer_error: outcome.result.as_ref().err().map(ToString::to_string),
    };
    let completed_trace: CompletedOperationTrace = trace.complete();
    async_trace_sink(completed_trace, metadata);
    let _ = outcome.result;
}

pub(crate) fn wait_for_completion_yield(
    process: &CommandProcess,
    completion: &CommandCompletionPromise,
    yield_time_ms: u64,
    start_offset: u64,
) -> CommandCompletionWaitOutcome {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(yield_time_ms);
    let (mut last_off, mut last_change) = (start_offset, started);
    loop {
        if completion.is_exited() {
            return CommandCompletionWaitOutcome::Completed;
        }
        let now = Instant::now();
        let off = process.transcript_len();
        if off != last_off {
            last_off = off;
            last_change = now;
        }
        if off > start_offset && now.duration_since(last_change) >= QUIET_MS {
            return CommandCompletionWaitOutcome::Running;
        }
        if now >= deadline {
            return CommandCompletionWaitOutcome::Running;
        }
        thread::sleep(COMPLETION_POLL);
    }
}

pub(crate) fn wait_for_completed_record(
    process_store: &CommandProcessStore,
    command_session_id: &CommandSessionId,
) -> Result<super::process_store::CompletedCommandRecord, CommandServiceError> {
    loop {
        if let Some(completed) = process_store.completed(command_session_id) {
            return Ok(completed);
        }
        if let Some(active) = process_store.active(command_session_id) {
            if matches!(
                active.finalization,
                super::process_store::FinalizationState::Failed { .. }
            ) {
                drop(active);
                return process_store.completed(command_session_id).ok_or_else(|| {
                    CommandServiceError::CommandNotFound {
                        command_session_id: command_session_id.clone(),
                    }
                });
            }
        } else {
            return Err(CommandServiceError::CommandNotFound {
                command_session_id: command_session_id.clone(),
            });
        }
        thread::sleep(COMPLETION_POLL);
    }
}
