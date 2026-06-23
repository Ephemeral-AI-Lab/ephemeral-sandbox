use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::workspace_crate::WorkspaceSessionId;

const DEFAULT_MAX_PENDING_PROJECTION: usize = 256;
const DEFAULT_MAX_RECENT_PROJECTED: usize = 256;
const DEFAULT_MAX_PARTIAL_ERRORS: usize = 32;
const MAX_ERROR_FIELD_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NamespaceExecutionId(pub String);

#[derive(Debug)]
pub struct NamespaceExecutionStore {
    inner: Mutex<NamespaceExecutionState>,
    next_id: AtomicU64,
    force_mutation_errors: AtomicBool,
    max_pending_projection: usize,
    max_recent_projected: usize,
    max_partial_errors: usize,
}

#[derive(Debug)]
struct NamespaceExecutionState {
    active: HashMap<NamespaceExecutionId, NamespaceExecutionRecord>,
    pending_projection: VecDeque<NamespaceExecutionRecord>,
    recent_projected: VecDeque<NamespaceExecutionRecord>,
    partial_errors: VecDeque<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceExecutionRecord {
    pub namespace_execution_id: NamespaceExecutionId,
    pub workspace_session_id: WorkspaceSessionId,
    pub operation_name: String,
    pub request_id: Option<String>,
    pub lifecycle_state: NamespaceExecutionLifecycle,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
    pub duration_ms: Option<f64>,
    pub terminal_status: Option<NamespaceExecutionTerminalStatus>,
    pub exit_code: Option<i64>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceExecutionLifecycle {
    Starting,
    Running,
    Terminal,
}

impl NamespaceExecutionLifecycle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Terminal => "terminal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceExecutionTerminalStatus {
    Ok,
    Error,
    TimedOut,
    Cancelled,
}

impl NamespaceExecutionTerminalStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeginNamespaceExecution {
    pub workspace_session_id: WorkspaceSessionId,
    pub operation_name: String,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteNamespaceExecution {
    pub terminal_status: NamespaceExecutionTerminalStatus,
    pub exit_code: Option<i64>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeNamespaceExecutionSnapshot {
    pub namespace_execution_id: NamespaceExecutionId,
    pub workspace_session_id: WorkspaceSessionId,
    pub operation_name: String,
    pub lifecycle_state: NamespaceExecutionLifecycle,
    pub started_at_unix_ms: i64,
}

impl NamespaceExecutionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(
            DEFAULT_MAX_PENDING_PROJECTION,
            DEFAULT_MAX_RECENT_PROJECTED,
            DEFAULT_MAX_PARTIAL_ERRORS,
        )
    }

    #[must_use]
    pub fn with_limits(
        max_pending_projection: usize,
        max_recent_projected: usize,
        max_partial_errors: usize,
    ) -> Self {
        Self {
            inner: Mutex::new(NamespaceExecutionState {
                active: HashMap::new(),
                pending_projection: VecDeque::new(),
                recent_projected: VecDeque::new(),
                partial_errors: VecDeque::new(),
            }),
            next_id: AtomicU64::new(1),
            force_mutation_errors: AtomicBool::new(false),
            max_pending_projection,
            max_recent_projected,
            max_partial_errors,
        }
    }

    #[must_use]
    pub fn allocate_namespace_execution_id(&self) -> NamespaceExecutionId {
        let next_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        NamespaceExecutionId(format!("namespace_execution_{next_id}"))
    }

    pub fn begin_namespace_execution(
        &self,
        namespace_execution_id: NamespaceExecutionId,
        begin: BeginNamespaceExecution,
    ) -> Result<(), String> {
        let mut state = self.lock_state()?;
        self.fail_forced_mutation(&mut state, "begin_namespace_execution")?;
        if state.active.contains_key(&namespace_execution_id)
            || state
                .pending_projection
                .iter()
                .any(|record| record.namespace_execution_id == namespace_execution_id)
            || state
                .recent_projected
                .iter()
                .any(|record| record.namespace_execution_id == namespace_execution_id)
        {
            return Err(format!(
                "namespace execution id {} already exists",
                namespace_execution_id.0
            ));
        }
        let record = NamespaceExecutionRecord {
            namespace_execution_id: namespace_execution_id.clone(),
            workspace_session_id: begin.workspace_session_id,
            operation_name: begin.operation_name,
            request_id: begin.request_id,
            lifecycle_state: NamespaceExecutionLifecycle::Starting,
            started_at_unix_ms: unix_ms(),
            finished_at_unix_ms: None,
            duration_ms: None,
            terminal_status: None,
            exit_code: None,
            error_kind: None,
            error_message: None,
        };
        state.active.insert(namespace_execution_id, record);
        Ok(())
    }

    pub fn mark_namespace_execution_running(
        &self,
        namespace_execution_id: &NamespaceExecutionId,
    ) -> Result<(), String> {
        let mut state = self.lock_state()?;
        self.fail_forced_mutation(&mut state, "mark_namespace_execution_running")?;
        let Some(record) = state.active.get_mut(namespace_execution_id) else {
            if find_terminal_record(&state, namespace_execution_id).is_some() {
                return Ok(());
            }
            return Err(format!(
                "namespace execution id {} is not active",
                namespace_execution_id.0
            ));
        };
        record.lifecycle_state = NamespaceExecutionLifecycle::Running;
        Ok(())
    }

    pub fn complete_namespace_execution(
        &self,
        namespace_execution_id: &NamespaceExecutionId,
        completion: CompleteNamespaceExecution,
    ) -> Result<NamespaceExecutionRecord, String> {
        let mut state = self.lock_state()?;
        self.fail_forced_mutation(&mut state, "complete_namespace_execution")?;
        if let Some(record) = find_terminal_record(&state, namespace_execution_id) {
            return Ok(record.clone());
        }
        let Some(mut record) = state.active.remove(namespace_execution_id) else {
            return Err(format!(
                "namespace execution id {} is not active",
                namespace_execution_id.0
            ));
        };

        let finished_at_unix_ms = unix_ms();
        record.lifecycle_state = NamespaceExecutionLifecycle::Terminal;
        record.finished_at_unix_ms = Some(finished_at_unix_ms);
        record.duration_ms = Some(duration_ms(record.started_at_unix_ms, finished_at_unix_ms));
        record.terminal_status = Some(completion.terminal_status);
        record.exit_code = completion.exit_code;
        record.error_kind = completion.error_kind.map(bound_error_field);
        record.error_message = completion.error_message.map(bound_error_field);

        if self.max_pending_projection > 0 {
            while state.pending_projection.len() >= self.max_pending_projection {
                if let Some(dropped) = state.pending_projection.pop_front() {
                    push_partial_error(
                        &mut state,
                        self.max_partial_errors,
                        format!(
                            "dropped namespace execution {} before projection acknowledgement",
                            dropped.namespace_execution_id.0
                        ),
                    );
                    push_recent_projected(&mut state, self.max_recent_projected, dropped);
                } else {
                    break;
                }
            }
            state.pending_projection.push_back(record.clone());
        } else {
            push_partial_error(
                &mut state,
                self.max_partial_errors,
                format!(
                    "dropped namespace execution {} before projection acknowledgement",
                    record.namespace_execution_id.0
                ),
            );
            push_recent_projected(&mut state, self.max_recent_projected, record.clone());
        }
        Ok(record)
    }

    pub fn snapshot_active_namespace_executions(
        &self,
    ) -> Result<Vec<RuntimeNamespaceExecutionSnapshot>, String> {
        let state = self.lock_state()?;
        let mut snapshots = state
            .active
            .values()
            .map(|record| RuntimeNamespaceExecutionSnapshot {
                namespace_execution_id: record.namespace_execution_id.clone(),
                workspace_session_id: record.workspace_session_id.clone(),
                operation_name: record.operation_name.clone(),
                lifecycle_state: record.lifecycle_state,
                started_at_unix_ms: record.started_at_unix_ms,
            })
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| {
            left.namespace_execution_id
                .cmp(&right.namespace_execution_id)
        });
        Ok(snapshots)
    }

    pub fn drain_completed_namespace_executions(
        &self,
        limit: usize,
    ) -> Result<Vec<NamespaceExecutionRecord>, String> {
        let state = self.lock_state()?;
        Ok(state
            .pending_projection
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    pub fn ack_completed_namespace_executions(
        &self,
        namespace_execution_ids: &[NamespaceExecutionId],
    ) -> Result<(), String> {
        let mut state = self.lock_state()?;
        self.fail_forced_mutation(&mut state, "ack_completed_namespace_executions")?;
        let ids = namespace_execution_ids.iter().collect::<HashSet<_>>();
        let mut kept = VecDeque::new();
        let mut acked = Vec::new();
        while let Some(record) = state.pending_projection.pop_front() {
            if ids.contains(&record.namespace_execution_id) {
                acked.push(record);
            } else {
                kept.push_back(record);
            }
        }
        state.pending_projection = kept;
        for record in acked {
            push_recent_projected(&mut state, self.max_recent_projected, record);
        }
        Ok(())
    }

    pub fn drain_partial_errors(&self) -> Result<Vec<String>, String> {
        let mut state = self.lock_state()?;
        Ok(state.partial_errors.drain(..).collect())
    }

    #[doc(hidden)]
    pub fn set_force_mutation_errors_for_test(&self, enabled: bool) {
        self.force_mutation_errors.store(enabled, Ordering::Relaxed);
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, NamespaceExecutionState>, String> {
        self.inner
            .lock()
            .map_err(|_| "namespace execution store lock is poisoned".to_owned())
    }

    fn fail_forced_mutation(
        &self,
        state: &mut NamespaceExecutionState,
        operation: &'static str,
    ) -> Result<(), String> {
        if !self.force_mutation_errors.load(Ordering::Relaxed) {
            return Ok(());
        }
        let error = format!("forced namespace execution store mutation failure: {operation}");
        push_partial_error(state, self.max_partial_errors, error.clone());
        Err(error)
    }
}

impl Default for NamespaceExecutionStore {
    fn default() -> Self {
        Self::new()
    }
}

fn find_terminal_record<'a>(
    state: &'a NamespaceExecutionState,
    namespace_execution_id: &NamespaceExecutionId,
) -> Option<&'a NamespaceExecutionRecord> {
    state
        .pending_projection
        .iter()
        .chain(state.recent_projected.iter())
        .find(|record| &record.namespace_execution_id == namespace_execution_id)
}

fn push_recent_projected(
    state: &mut NamespaceExecutionState,
    max_recent_projected: usize,
    record: NamespaceExecutionRecord,
) {
    if max_recent_projected == 0 {
        return;
    }
    while state.recent_projected.len() >= max_recent_projected {
        let _ = state.recent_projected.pop_front();
    }
    state.recent_projected.push_back(record);
}

fn push_partial_error(
    state: &mut NamespaceExecutionState,
    max_partial_errors: usize,
    error: String,
) {
    if max_partial_errors == 0 {
        return;
    }
    while state.partial_errors.len() >= max_partial_errors {
        let _ = state.partial_errors.pop_front();
    }
    state.partial_errors.push_back(bound_error_field(error));
}

fn bound_error_field(value: String) -> String {
    if value.len() <= MAX_ERROR_FIELD_BYTES {
        return value;
    }
    let mut end = MAX_ERROR_FIELD_BYTES;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_owned()
}

fn duration_ms(started_at_unix_ms: i64, finished_at_unix_ms: i64) -> f64 {
    finished_at_unix_ms
        .saturating_sub(started_at_unix_ms)
        .max(0) as f64
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
