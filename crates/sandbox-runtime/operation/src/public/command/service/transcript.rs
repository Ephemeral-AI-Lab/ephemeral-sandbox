use crate::command::{
    CommandLinesOutput, CommandOutputSnapshot, CommandServiceError, CommandSessionId, CommandStatus,
};

use super::process_store::{CommandTranscriptStore, RetainedCommandTranscript};

impl CommandTranscriptStore {
    #[must_use]
    pub(crate) fn window(
        &self,
        offset: u64,
        limit: usize,
    ) -> ::sandbox_runtime_command::CommandTranscriptWindow {
        ::sandbox_runtime_command::transcript_window(self.transcript_path.as_deref(), offset, limit)
    }
}

impl RetainedCommandTranscript {
    pub(crate) fn window(
        &self,
        command_session_id: &CommandSessionId,
        offset: u64,
        limit: usize,
    ) -> Result<::sandbox_runtime_command::CommandTranscriptWindow, CommandServiceError> {
        ::sandbox_runtime_command::required_transcript_window(
            self.transcript_path.as_deref(),
            offset,
            limit,
        )
        .map_err(|error| CommandServiceError::CommandTranscriptUnavailable {
            command_session_id: command_session_id.clone(),
            path: self.transcript_path.clone(),
            error,
        })
    }
}

#[must_use]
pub(crate) fn command_lines_output(
    window: ::sandbox_runtime_command::CommandTranscriptWindow,
    command_session_id: CommandSessionId,
    status: CommandStatus,
    exit_code: Option<i64>,
    wall_time_seconds: f64,
    command_total_time_seconds: f64,
) -> CommandLinesOutput {
    let output = command_output_snapshot(window);
    CommandLinesOutput {
        command_session_id,
        status,
        exit_code,
        wall_time_seconds,
        command_total_time_seconds,
        start_offset: output.start_offset,
        end_offset: output.end_offset,
        total_lines: output.total_lines,
        original_token_count: output.original_token_count,
        output: output.output,
    }
}

#[must_use]
pub(crate) fn command_output_snapshot(
    window: ::sandbox_runtime_command::CommandTranscriptWindow,
) -> CommandOutputSnapshot {
    let output = render_transcript_text(&window.output);
    CommandOutputSnapshot {
        start_offset: window.offset,
        end_offset: window.next_offset,
        total_lines: window.total_lines,
        original_token_count: estimate_token_count(output.len()),
        output,
    }
}

#[must_use]
pub(crate) fn estimate_token_count(chars: usize) -> u64 {
    if chars == 0 {
        0
    } else {
        u64::try_from(chars.div_ceil(4)).unwrap_or(u64::MAX)
    }
}

fn render_transcript_text(rows: &[::sandbox_runtime_command::CommandTranscriptRow]) -> String {
    rows.iter()
        .map(|row| row.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
