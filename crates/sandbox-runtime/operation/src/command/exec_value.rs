use std::cell::{Cell, RefCell};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use sandbox_runtime_namespace_execution::{
    required_transcript_window, transcript_window, CommandTranscriptWindow, InteractiveExecution,
    NamespaceExecutionError,
};

use super::contract::CommandTerminalResult;
use super::terminal_cache::TerminalDrainRecord;
use crate::workspace_crate::WorkspaceSessionId;
use crate::workspace_crate::{ExecutionScratchLease, ExecutionScratchRoute};
use crate::workspace_session::FinalizeOutcome;

/// The per-execution value the engine registry holds for a command. The engine
/// forwards (`is_finished`/`output_len`/`completion`/`write_stdin`/`cancel`/
/// `resolved`) are reached through `value.exec`; this type only adds what the
/// command layer owns beyond `InteractiveExecution`: the transcript window, the
/// elapsed-time clocks, the streaming snapshot offset, and the finalize-outcome
/// slot set at attach (§2.5). Dropping the value — retention eviction or engine
/// teardown — removes the command's scratch directory.
pub struct CommandExecValue {
    pub(crate) exec: InteractiveExecution<CommandTerminalResult>,
    scratch: RefCell<ScratchTranscript>,
    pub(crate) workspace_session_id: WorkspaceSessionId,
    started_at: Instant,
    pub(crate) operation_name: &'static str,
    pub(crate) command: String,
    next_snapshot_offset: Cell<u64>,
    pub(crate) finalize_outcome: Arc<OnceLock<FinalizeOutcome>>,
    max_transcript_window_bytes: u64,
    scratch_route: ExecutionScratchRoute,
}

struct ScratchTranscript {
    lease: Option<ExecutionScratchLease>,
    retained: Option<CommandTranscriptWindow>,
}

impl CommandExecValue {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        exec: InteractiveExecution<CommandTerminalResult>,
        scratch: ExecutionScratchLease,
        workspace_session_id: WorkspaceSessionId,
        started_at: Instant,
        operation_name: &'static str,
        command: String,
        finalize_outcome: Arc<OnceLock<FinalizeOutcome>>,
        max_transcript_window_bytes: u64,
    ) -> Self {
        let scratch_route = scratch.route();
        Self {
            exec,
            scratch: RefCell::new(ScratchTranscript {
                lease: Some(scratch),
                retained: None,
            }),
            workspace_session_id,
            started_at,
            operation_name,
            command,
            next_snapshot_offset: Cell::new(0),
            finalize_outcome,
            max_transcript_window_bytes,
            scratch_route,
        }
    }

    #[must_use]
    pub(crate) const fn scratch_route(&self) -> ExecutionScratchRoute {
        self.scratch_route
    }

    #[must_use]
    pub fn elapsed_seconds(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64()
    }

    #[must_use]
    pub fn take_snapshot_offset(&self) -> u64 {
        self.next_snapshot_offset.get()
    }

    pub fn advance_snapshot_offset(&self, next: u64) {
        self.next_snapshot_offset.set(next);
    }

    #[must_use]
    pub fn transcript_window(&self, start: u64, limit: usize) -> CommandTranscriptWindow {
        let scratch = self.scratch.borrow();
        scratch.lease.as_ref().map_or_else(
            || {
                scratch.retained.as_ref().map_or_else(
                    || transcript_window(None, start, limit, 0),
                    |retained| retained_window(retained, start, limit),
                )
            },
            |lease| {
                transcript_window(
                    Some(lease.transcript_path()),
                    start,
                    limit,
                    self.max_transcript_window_bytes,
                )
            },
        )
    }

    pub fn required_transcript_window(
        &self,
        start: u64,
        limit: usize,
    ) -> Result<CommandTranscriptWindow, String> {
        let scratch = self.scratch.borrow();
        match (&scratch.lease, &scratch.retained) {
            (Some(lease), _) => required_transcript_window(
                Some(lease.transcript_path()),
                start,
                limit,
                self.max_transcript_window_bytes,
            ),
            (None, Some(retained)) => Ok(retained_window(retained, start, limit)),
            (None, None) => Err("retained transcript is missing".to_owned()),
        }
    }

    #[must_use]
    pub fn transcript_bytes(&self) -> u64 {
        self.scratch.borrow().lease.as_ref().map_or(0, |lease| {
            std::fs::metadata(lease.transcript_path()).map_or(0, |metadata| metadata.len())
        })
    }

    /// Snapshot the bounded terminal transcript, then release only the
    /// execution leaf. The registry value remains available for output drains
    /// but no longer owns filesystem scratch.
    pub(crate) fn release_scratch(&self) -> Result<bool, String> {
        let mut scratch = self.scratch.borrow_mut();
        let Some(lease) = scratch.lease.as_mut() else {
            return Ok(false);
        };
        let retained = required_transcript_window(
            Some(lease.transcript_path()),
            0,
            usize::MAX,
            self.max_transcript_window_bytes,
        )?;
        lease
            .release_in_place()
            .map_err(|error| error.to_string())?;
        scratch.lease.take();
        scratch.retained = Some(retained);
        Ok(true)
    }

    #[must_use]
    pub(crate) fn terminal_drain_record(
        &self,
        result: Result<CommandTerminalResult, NamespaceExecutionError>,
    ) -> TerminalDrainRecord {
        TerminalDrainRecord {
            result,
            retained: self.transcript_window(0, usize::MAX),
            workspace_session_id: self.workspace_session_id.clone(),
            started_at: self.started_at,
            next_snapshot_offset: self.take_snapshot_offset(),
            finalize_outcome: Arc::clone(&self.finalize_outcome),
            completion: self.exec.completion(),
        }
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
