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
use crate::workspace_crate::WorkspaceSessionId;
use crate::workspace_remount::{RemountCancellationToken, RemountSwitchState};

pub const DEFAULT_MAX_ACTIVE_COMMANDS: usize = 256;

pub struct CommandProcessStore {
    active: Mutex<HashMap<CommandSessionId, ActiveCommandProcess>>,
    completed: CommandCompletionStore,
    next_id: AtomicU64,
    active_count: AtomicUsize,
    max_active: usize,
}

impl CommandProcessStore {
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_active(DEFAULT_MAX_ACTIVE_COMMANDS)
    }

    #[must_use]
    pub fn with_max_active(max_active: usize) -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            completed: CommandCompletionStore::new(),
            next_id: AtomicU64::new(1),
            active_count: AtomicUsize::new(0),
            max_active,
        }
    }

    #[must_use]
    pub fn allocate_command_session_id(&self) -> CommandSessionId {
        let next_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        CommandSessionId(format!("cmd_{next_id}"))
    }

    pub fn try_reserve(&self) -> Result<CommandReservation<'_>, CommandServiceError> {
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

    pub fn insert_active(
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
    pub fn active(&self, command_session_id: &CommandSessionId) -> Option<ActiveCommandRef<'_>> {
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
    pub(crate) fn active_process(
        &self,
        command_session_id: &CommandSessionId,
    ) -> Option<Arc<::sandbox_runtime_command::CommandProcess>> {
        lock(&self.active)
            .get(command_session_id)
            .map(|active| Arc::clone(&active.process))
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

    pub fn complete_active(
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

    #[must_use]
    pub fn completed(
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

pub struct ActiveCommandRef<'a> {
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

pub struct ActiveCommandProcess {
    pub command_session_id: CommandSessionId,
    pub workspace_session_id: WorkspaceSessionId,
    pub workspace_root: PathBuf,
    pub process: Arc<::sandbox_runtime_command::CommandProcess>,
    pub transcript: CommandTranscriptStore,
    pub lifecycle_state: CommandLifecycleState,
    pub cancellation: CancellationState,
    pub remount_cancellation: Option<RemountCancellationToken>,
    pub remount_switch_state: Option<RemountSwitchState>,
    pub finalization: FinalizationState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLifecycleState {
    Running,
    QuiescedForRemount,
    Finalizing,
    Cancelled,
    FinalizationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancellationState {
    None,
    Requested { requested_at: Instant },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalizationState {
    NotStarted,
    InProgress,
    Complete,
    Failed {
        error: String,
        finalized: Option<CommandFinalizedMetadata>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandTranscriptStore {
    pub transcript_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetainedCommandTranscript {
    pub transcript_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTerminalResult {
    pub status: CommandStatus,
    pub exit_code: Option<i64>,
    pub stdout: String,
}

#[derive(Debug, Default)]
struct CommandCompletionStore {
    completed: Mutex<HashMap<CommandSessionId, CompletedCommandRecord>>,
}

impl CommandCompletionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn get(&self, command_session_id: &CommandSessionId) -> Option<CompletedCommandRecord> {
        lock(&self.completed).get(command_session_id).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedCommandRecord {
    pub command_session_id: CommandSessionId,
    pub workspace_session_id: WorkspaceSessionId,
    pub result: CommandTerminalResult,
    pub transcript: RetainedCommandTranscript,
    pub finalization: FinalizationState,
    pub finalized: Option<CommandFinalizedMetadata>,
    pub completed_at: Instant,
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
