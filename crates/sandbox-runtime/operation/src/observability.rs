use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::command::CommandSessionId;
use crate::namespace_execution::{NamespaceExecutionRecord, RuntimeNamespaceExecutionSnapshot};
use crate::workspace_crate::{WorkspaceProfile, WorkspaceSessionId};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuntimeObservabilitySnapshot {
    pub workspaces: Vec<RuntimeWorkspaceSnapshot>,
    pub active_namespace_executions: Vec<RuntimeNamespaceExecutionSnapshot>,
    pub completed_namespace_executions: Vec<NamespaceExecutionRecord>,
    pub partial_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeWorkspaceSnapshot {
    pub workspace_id: WorkspaceSessionId,
    pub remount_state: String,
    pub profile: WorkspaceProfile,
    pub workspace_root: PathBuf,
    pub upperdir: Option<PathBuf>,
    pub workdir: Option<PathBuf>,
    pub namespace_fd_count: Option<usize>,
    pub base_manifest_version: Option<i64>,
    pub base_root_hash: Option<String>,
    pub layer_count: Option<usize>,
}

pub type AsyncTraceSink =
    Arc<dyn Fn(CompletedOperationTrace, CommandFinalizationTraceMetadata) + Send + Sync + 'static>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFinalizationTraceMetadata {
    pub origin_request_id: String,
    pub workspace_session_id: Option<WorkspaceSessionId>,
    pub command_session_id: CommandSessionId,
    pub finalizer_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SpanKey(&'static str);

impl SpanKey {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

pub mod span_keys {
    use super::SpanKey;

    pub const COMMAND_EXEC_WORKSPACE_RESOLVE: SpanKey = SpanKey("command.exec.workspace.resolve");
    pub const COMMAND_EXEC_WORKSPACE_RESOLVE_EXISTING_SESSION: SpanKey =
        SpanKey("command.exec.workspace.resolve_existing_session");
    pub const COMMAND_EXEC_WORKSPACE_CREATE_ONE_SHOT_SESSION: SpanKey =
        SpanKey("command.exec.workspace.create_one_shot_session");
    pub const COMMAND_EXEC_PROCESS_START: SpanKey = SpanKey("command.exec.process.start");
    pub const LAYERSTACK_SQUASH_OPEN_STACK: SpanKey = SpanKey("layerstack.squash.open_stack");
    pub const LAYERSTACK_SQUASH_COMPACT_STACK: SpanKey = SpanKey("layerstack.squash.compact_stack");
}

pub struct OperationTrace {
    state: RefCell<TraceState>,
    enabled_span_keys: HashSet<SpanKey>,
}

struct TraceState {
    started_at: Instant,
    started_at_unix_ms: i64,
    active_stack: Vec<i64>,
    completed: Vec<CompletedOperationSpan>,
    next_call_index: i64,
}

pub struct SpanGuard<'a> {
    trace: &'a OperationTrace,
    parent_call_index: Option<i64>,
    method_name: &'static str,
    call_index: i64,
    started_at: Instant,
    started_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletedOperationTrace {
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub duration_ms: f64,
    pub spans: Vec<CompletedOperationSpan>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletedOperationSpan {
    pub parent_call_index: Option<i64>,
    pub method_name: &'static str,
    pub call_index: i64,
    pub status: &'static str,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub duration_ms: f64,
}

impl OperationTrace {
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_enabled_span_keys([])
    }

    #[must_use]
    pub fn new_with_enabled_span_keys(keys: impl IntoIterator<Item = SpanKey>) -> Self {
        Self {
            state: RefCell::new(TraceState {
                started_at: Instant::now(),
                started_at_unix_ms: unix_ms(),
                active_stack: Vec::new(),
                completed: Vec::new(),
                next_call_index: 0,
            }),
            enabled_span_keys: keys.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn enter(&self, method_name: &'static str) -> SpanGuard<'_> {
        let started_at = Instant::now();
        let started_at_unix_ms = unix_ms();
        let mut state = self.state.borrow_mut();
        let call_index = state.next_call_index;
        state.next_call_index += 1;
        let parent_call_index = state.active_stack.last().copied();
        state.active_stack.push(call_index);
        SpanGuard {
            trace: self,
            parent_call_index,
            method_name,
            call_index,
            started_at,
            started_at_unix_ms,
        }
    }

    pub fn measure<T>(&self, method_name: &'static str, call: impl FnOnce() -> T) -> T {
        let _span = self.enter(method_name);
        call()
    }

    pub fn measure_if<T>(&self, span_key: SpanKey, call: impl FnOnce() -> T) -> T {
        if self.enabled_span_keys.contains(&span_key) {
            self.measure(span_key.as_str(), call)
        } else {
            call()
        }
    }

    #[must_use]
    pub fn complete(&self) -> CompletedOperationTrace {
        let state = self.state.borrow();
        let duration_ms = elapsed_ms(state.started_at);
        let mut spans = state.completed.clone();
        spans.sort_by_key(|span| span.call_index);
        CompletedOperationTrace {
            started_at_unix_ms: state.started_at_unix_ms,
            finished_at_unix_ms: finish_unix_ms(state.started_at_unix_ms, duration_ms),
            duration_ms,
            spans,
        }
    }
}

impl Default for OperationTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SpanGuard<'_> {
    fn drop(&mut self) {
        let duration_ms = elapsed_ms(self.started_at);
        let mut state = self.trace.state.borrow_mut();
        let _ = state.active_stack.pop();
        state.completed.push(CompletedOperationSpan {
            parent_call_index: self.parent_call_index,
            method_name: self.method_name,
            call_index: self.call_index,
            status: if thread::panicking() { "panic" } else { "ok" },
            started_at_unix_ms: self.started_at_unix_ms,
            finished_at_unix_ms: finish_unix_ms(self.started_at_unix_ms, duration_ms),
            duration_ms,
        });
    }
}

pub(crate) fn measure_optional<T>(
    trace: Option<&OperationTrace>,
    method_name: &'static str,
    call: impl FnOnce() -> T,
) -> T {
    match trace {
        Some(trace) => trace.measure(method_name, call),
        None => call(),
    }
}

pub(crate) fn measure_optional_if<T>(
    trace: Option<&OperationTrace>,
    span_key: SpanKey,
    call: impl FnOnce() -> T,
) -> T {
    match trace {
        Some(trace) => trace.measure_if(span_key, call),
        None => call(),
    }
}

fn elapsed_ms(started_at: Instant) -> f64 {
    started_at.elapsed().as_secs_f64() * 1000.0
}

fn finish_unix_ms(started_at_unix_ms: i64, duration_ms: f64) -> i64 {
    started_at_unix_ms.saturating_add(duration_ms.round() as i64)
}

fn unix_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}
