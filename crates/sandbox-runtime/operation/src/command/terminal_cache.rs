use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use sandbox_runtime_namespace_execution::{
    CommandTranscriptWindow, CompletionWaiter, NamespaceExecutionError, NamespaceExecutionId,
};

use super::contract::CommandTerminalResult;
use crate::workspace_crate::WorkspaceSessionId;
use crate::workspace_session::FinalizeOutcome;

/// Ownerless, bounded output projection retained only to preserve terminal
/// read behavior after an implicit workspace destroy evicts its command owner.
pub(crate) struct TerminalDrainRecord {
    pub(crate) result: Result<CommandTerminalResult, NamespaceExecutionError>,
    pub(crate) retained: CommandTranscriptWindow,
    pub(crate) workspace_session_id: WorkspaceSessionId,
    pub(crate) started_at: Instant,
    pub(crate) next_snapshot_offset: u64,
    pub(crate) finalize_outcome: Arc<OnceLock<FinalizeOutcome>>,
    pub(crate) completion: Arc<dyn CompletionWaiter>,
}

impl TerminalDrainRecord {
    #[must_use]
    pub(crate) fn elapsed_seconds(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64()
    }

    #[must_use]
    pub(crate) fn window(&self, offset: u64, limit: usize) -> CommandTranscriptWindow {
        retained_window(&self.retained, offset, limit)
    }
}

pub(crate) struct TerminalDrainCache {
    state: Mutex<TerminalDrainState>,
}

struct TerminalDrainState {
    entries: HashMap<NamespaceExecutionId, TerminalDrainRecord>,
    order: VecDeque<NamespaceExecutionId>,
    max_entries: usize,
}

impl TerminalDrainCache {
    #[must_use]
    pub(crate) fn new(max_entries: usize) -> Self {
        Self {
            state: Mutex::new(TerminalDrainState {
                entries: HashMap::new(),
                order: VecDeque::new(),
                max_entries,
            }),
        }
    }

    pub(crate) fn insert(&self, id: NamespaceExecutionId, record: TerminalDrainRecord) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.max_entries == 0 {
            return;
        }
        if state.entries.insert(id.clone(), record).is_none() {
            state.order.push_back(id);
        }
        while state.order.len() > state.max_entries {
            let Some(oldest) = state.order.pop_front() else {
                break;
            };
            state.entries.remove(&oldest);
        }
    }

    pub(crate) fn with<R>(
        &self,
        id: &NamespaceExecutionId,
        f: impl FnOnce(&TerminalDrainRecord) -> R,
    ) -> Option<R> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .get(id)
            .map(f)
    }

    pub(crate) fn with_mut<R>(
        &self,
        id: &NamespaceExecutionId,
        f: impl FnOnce(&mut TerminalDrainRecord) -> R,
    ) -> Option<R> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .get_mut(id)
            .map(f)
    }
}

fn retained_window(
    retained: &CommandTranscriptWindow,
    offset: u64,
    limit: usize,
) -> CommandTranscriptWindow {
    let output = retained
        .output
        .iter()
        .filter(|row| row.offset >= offset)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let next_offset = output.last().map_or_else(
        || {
            if offset < retained.truncated_before {
                retained.truncated_before
            } else {
                offset
            }
        },
        |row| row.offset.saturating_add(1),
    );
    CommandTranscriptWindow {
        offset,
        next_offset,
        total_lines: retained.total_lines,
        truncated_before: retained.truncated_before,
        output_truncated: offset < retained.truncated_before || next_offset < retained.total_lines,
        output,
    }
}
