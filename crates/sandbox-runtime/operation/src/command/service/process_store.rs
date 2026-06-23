use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::command::{
    CommandFinalizedMetadata, CommandServiceError, CommandSessionId, CommandStatus,
};
use crate::namespace_execution::NamespaceExecutionId;
use crate::workspace_crate::WorkspaceSessionId;
use crate::workspace_remount::{RemountCancellationToken, RemountSwitchState};
use crate::workspace_session::WorkspaceSessionHandler;

use super::completion::CommandCompletionPromise;

const DEFAULT_MAX_ACTIVE_COMMANDS: usize = 256;

pub(crate) struct CommandProcessStore {
    active: Mutex<HashMap<CommandSessionId, ActiveCommandProcess>>,
    completed: CommandCompletionStore,
    next_id: AtomicU64,
    active_count: AtomicUsize,
    max_active: usize,
}

impl CommandProcessStore {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::with_max_active(DEFAULT_MAX_ACTIVE_COMMANDS)
    }

    #[must_use]
    fn with_max_active(max_active: usize) -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            completed: CommandCompletionStore::new(),
            next_id: AtomicU64::new(1),
            active_count: AtomicUsize::new(0),
            max_active,
        }
    }

    #[must_use]
    pub(crate) fn allocate_command_session_id(&self) -> CommandSessionId {
        let next_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        CommandSessionId(format!("cmd_{next_id}"))
    }

    pub(crate) fn try_reserve(&self) -> Result<CommandReservation<'_>, CommandServiceError> {
        loop {
            let active = self.active_count.load(Ordering::Acquire);
            if active >= self.max_active {
                return Err(CommandServiceError::CommandAdmissionLimit {
                    active,
                    max: self.max_active,
                });
            }

            if self
                .active_count
                .compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(CommandReservation {
                    store: self,
                    activated: false,
                });
            }
        }
    }

    pub(crate) fn insert_active(
        &self,
        reservation: CommandReservation<'_>,
        record: ActiveCommandProcess,
    ) -> Result<(), CommandServiceError> {
        reservation.ensure_store(self)?;
        let command_session_id = record.command_session_id.clone();
        let mut active = lock(&self.active);
        if active.contains_key(&command_session_id) {
            return Err(CommandServiceError::DuplicateCommandSessionId { command_session_id });
        }

        active.insert(command_session_id, record);
        reservation.activate();
        Ok(())
    }

    #[must_use]
    pub(crate) fn active(
        &self,
        command_session_id: &CommandSessionId,
    ) -> Option<ActiveCommandRef<'_>> {
        let active = lock(&self.active);
        if !active.contains_key(command_session_id) {
            return None;
        }

        Some(ActiveCommandRef {
            command_session_id: command_session_id.clone(),
            active,
        })
    }

    #[must_use]
    pub(crate) fn active_command_session_ids_for_workspace_session(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Vec<CommandSessionId> {
        let mut command_session_ids = lock(&self.active)
            .iter()
            .filter(|(_, active)| &active.workspace_session_id == workspace_session_id)
            .map(|(command_session_id, _)| command_session_id.clone())
            .collect::<Vec<_>>();
        command_session_ids.sort();
        command_session_ids
    }

    pub(crate) fn complete_active(
        &self,
        record: CompletedCommandRecord,
    ) -> Result<Option<ActiveCommandProcess>, CommandServiceError> {
        let command_session_id = record.command_session_id.clone();
        let mut active = lock(&self.active);
        if !active.contains_key(&command_session_id) {
            return Ok(None);
        }
        let active_record = active
            .get(&command_session_id)
            .expect("active command exists after contains_key");
        if active_record.workspace_session_id != record.workspace_session_id {
            return Err(CommandServiceError::CommandWorkspaceSessionMismatch {
                command_session_id,
                expected: active_record.workspace_session_id.clone(),
                actual: record.workspace_session_id,
            });
        }

        let mut completed = lock(&self.completed.completed);
        if completed.contains_key(&command_session_id) {
            return Err(CommandServiceError::DuplicateCommandSessionId { command_session_id });
        }

        let removed = active
            .remove(&record.command_session_id)
            .expect("active command exists after contains_key");
        completed.insert(record.command_session_id.clone(), record);
        decrement_slot(&self.active_count);
        Ok(Some(removed))
    }

    pub(crate) fn fail_active(
        &self,
        command_session_id: &CommandSessionId,
        error: String,
        result: CommandTerminalResult,
        finalized: Option<CommandFinalizedMetadata>,
    ) -> Result<(), CommandServiceError> {
        let mut active = lock(&self.active);
        let Some(active_record) = active.remove(command_session_id) else {
            return Ok(());
        };
        let mut completed = lock(&self.completed.completed);
        if completed.contains_key(command_session_id) {
            return Err(CommandServiceError::DuplicateCommandSessionId {
                command_session_id: command_session_id.clone(),
            });
        }
        completed.insert(
            command_session_id.clone(),
            CompletedCommandRecord {
                command_session_id: command_session_id.clone(),
                workspace_session_id: active_record.workspace_session_id,
                namespace_execution_id: active_record.namespace_execution_id,
                started_at: active_record.started_at,
                result,
                transcript: RetainedCommandTranscript {
                    transcript_path: active_record.transcript.transcript_path,
                },
                next_snapshot_offset: active_record.next_snapshot_offset,
                finalization: FinalizationState::Failed {
                    error,
                    finalized: finalized.clone().map(Box::new),
                },
                finalized,
            },
        );
        decrement_slot(&self.active_count);
        Ok(())
    }

    #[must_use]
    pub(crate) fn completed(
        &self,
        command_session_id: &CommandSessionId,
    ) -> Option<CompletedCommandRecord> {
        self.completed.get(command_session_id)
    }

    pub(crate) fn update_active<R>(
        &self,
        command_session_id: &CommandSessionId,
        update: impl FnOnce(&mut ActiveCommandProcess) -> R,
    ) -> Option<R> {
        lock(&self.active).get_mut(command_session_id).map(update)
    }
}

impl Default for CommandProcessStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CommandProcessStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandProcessStore")
            .field("active_len", &lock(&self.active).len())
            .field("completed", &self.completed)
            .field("next_id", &self.next_id.load(Ordering::Relaxed))
            .field("active_count", &self.active_count.load(Ordering::Relaxed))
            .field("max_active", &self.max_active)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct CommandReservation<'a> {
    store: &'a CommandProcessStore,
    activated: bool,
}

impl CommandReservation<'_> {
    fn ensure_store(&self, store: &CommandProcessStore) -> Result<(), CommandServiceError> {
        if std::ptr::eq(self.store, store) {
            Ok(())
        } else {
            Err(CommandServiceError::ReservationStoreMismatch)
        }
    }

    fn activate(mut self) {
        self.activated = true;
    }
}

impl Drop for CommandReservation<'_> {
    fn drop(&mut self) {
        if !self.activated {
            decrement_slot(&self.store.active_count);
        }
    }
}

pub(crate) struct ActiveCommandRef<'a> {
    command_session_id: CommandSessionId,
    active: MutexGuard<'a, HashMap<CommandSessionId, ActiveCommandProcess>>,
}

impl Deref for ActiveCommandRef<'_> {
    type Target = ActiveCommandProcess;

    fn deref(&self) -> &Self::Target {
        self.active
            .get(&self.command_session_id)
            .expect("active command disappeared while lock is held")
    }
}

pub(crate) struct ActiveCommandProcess {
    pub(crate) command_session_id: CommandSessionId,
    pub(crate) namespace_execution_id: NamespaceExecutionId,
    pub(crate) workspace_session_id: WorkspaceSessionId,
    pub(crate) workspace_ownership: CommandWorkspaceOwnership,
    pub(crate) workspace_root: PathBuf,
    pub(crate) started_at: Instant,
    pub(crate) process: Arc<::sandbox_runtime_command::CommandProcess>,
    pub(crate) completion: CommandCompletionPromise,
    pub(crate) transcript: CommandTranscriptStore,
    pub(crate) next_snapshot_offset: u64,
    pub(crate) lifecycle_state: CommandLifecycleState,
    pub(crate) cancellation: CancellationState,
    pub(crate) remount_cancellation: Option<RemountCancellationToken>,
    pub(crate) remount_switch_state: Option<RemountSwitchState>,
    pub(crate) finalization: FinalizationState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandWorkspaceOwnership {
    ExistingSession,
    OneShot {
        handler: Box<WorkspaceSessionHandler>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandLifecycleState {
    Running,
    QuiescedForRemount,
    Finalizing,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CancellationState {
    None,
    Requested { requested_at: Instant },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FinalizationState {
    NotStarted,
    InProgress,
    Complete,
    Failed {
        error: String,
        finalized: Option<Box<CommandFinalizedMetadata>>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CommandTranscriptStore {
    pub(crate) transcript_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RetainedCommandTranscript {
    pub(crate) transcript_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommandTerminalResult {
    pub(crate) status: CommandStatus,
    pub(crate) exit_code: Option<i64>,
    pub(crate) stdout: String,
    pub(crate) command_total_time_seconds: f64,
}

#[derive(Debug, Default)]
struct CommandCompletionStore {
    completed: Mutex<HashMap<CommandSessionId, CompletedCommandRecord>>,
}

impl CommandCompletionStore {
    #[must_use]
    fn new() -> Self {
        Self::default()
    }

    #[must_use]
    fn get(&self, command_session_id: &CommandSessionId) -> Option<CompletedCommandRecord> {
        lock(&self.completed).get(command_session_id).cloned()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompletedCommandRecord {
    pub(crate) command_session_id: CommandSessionId,
    pub(crate) workspace_session_id: WorkspaceSessionId,
    pub(crate) namespace_execution_id: NamespaceExecutionId,
    pub(crate) started_at: Instant,
    pub(crate) result: CommandTerminalResult,
    pub(crate) transcript: RetainedCommandTranscript,
    pub(crate) next_snapshot_offset: u64,
    pub(crate) finalization: FinalizationState,
    pub(crate) finalized: Option<CommandFinalizedMetadata>,
}

fn decrement_slot(active_count: &AtomicUsize) {
    let _ = active_count.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
        Some(count.saturating_sub(1))
    });
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
