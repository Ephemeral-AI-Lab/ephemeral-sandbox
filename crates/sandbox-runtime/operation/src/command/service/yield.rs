use std::time::{Duration, Instant};

use sandbox_runtime_namespace_execution::{
    CompletionWaiter, NamespaceExecutionError, NamespaceExecutionId,
};

use super::core::CommandOperationService;
use super::render::{command_output, command_status};
use crate::command::{CommandOutput, CommandServiceError, CommandStatus};

impl CommandOperationService {
    pub(crate) fn wait_for_command_yield(
        &self,
        command_session_id: NamespaceExecutionId,
        yield_time_ms: u64,
        include_terminal_command_session_id: bool,
    ) -> Result<CommandOutput, CommandServiceError> {
        let id = command_session_id.clone();
        let deadline = Instant::now() + Duration::from_millis(yield_time_ms);
        loop {
            let Some((finished, waiter)) = self.command_wait_state(&id) else {
                return command_not_found(command_session_id);
            };
            if finished {
                return self.completed_command_output(
                    command_session_id,
                    include_terminal_command_session_id,
                );
            }
            let now = Instant::now();
            if now >= deadline {
                return match self.command_finished(&id) {
                    Some(true) => self.completed_command_output(
                        command_session_id,
                        include_terminal_command_session_id,
                    ),
                    Some(false) => self.running_command_output(command_session_id),
                    None => command_not_found(command_session_id),
                };
            }
            waiter.wait_timeout(deadline.saturating_duration_since(now));
        }
    }

    fn command_wait_state(
        &self,
        command_session_id: &NamespaceExecutionId,
    ) -> Option<(bool, std::sync::Arc<dyn CompletionWaiter>)> {
        self.engine()
            .with_value(command_session_id, |command| {
                (command.exec.is_finished(), command.exec.completion())
            })
            .or_else(|| {
                self.terminal_drains().with(command_session_id, |record| {
                    (
                        record.completion.wait_timeout(Duration::ZERO),
                        std::sync::Arc::clone(&record.completion),
                    )
                })
            })
    }

    fn command_finished(&self, command_session_id: &NamespaceExecutionId) -> Option<bool> {
        self.engine()
            .with_value(command_session_id, |command| command.exec.is_finished())
            .or_else(|| {
                self.terminal_drains().with(command_session_id, |record| {
                    record.completion.wait_timeout(Duration::ZERO)
                })
            })
    }

    fn running_command_output(
        &self,
        command_session_id: NamespaceExecutionId,
    ) -> Result<CommandOutput, CommandServiceError> {
        let id = command_session_id.clone();
        let output = self.engine().with_value(&id, |command| {
            let start = command.take_snapshot_offset();
            let window = command.transcript_window(start, usize::MAX);
            command.advance_snapshot_offset(window.next_offset);
            let elapsed = command.elapsed_seconds();
            let mut output =
                command_output(window, None, CommandStatus::Running, None, elapsed, elapsed);
            output.workspace_session_id = Some(command.workspace_session_id.clone());
            output
        });
        match output {
            Some(mut output) => {
                output.command_session_id = Some(command_session_id);
                Ok(output)
            }
            None => {
                let output = self
                    .terminal_drains()
                    .with_mut(&command_session_id, |record| {
                        let start = record.next_snapshot_offset;
                        let window = record.window(start, usize::MAX);
                        record.next_snapshot_offset = window.next_offset;
                        let elapsed = record.elapsed_seconds();
                        let mut output = command_output(
                            window,
                            None,
                            CommandStatus::Running,
                            None,
                            elapsed,
                            elapsed,
                        );
                        output.workspace_session_id = Some(record.workspace_session_id.clone());
                        output
                    });
                match output {
                    Some(mut output) => {
                        output.command_session_id = Some(command_session_id);
                        Ok(output)
                    }
                    None => command_not_found(command_session_id),
                }
            }
        }
    }

    pub(crate) fn completed_command_output(
        &self,
        command_session_id: NamespaceExecutionId,
        include_terminal_command_session_id: bool,
    ) -> Result<CommandOutput, CommandServiceError> {
        let id = command_session_id.clone();
        let read = self.engine().with_value(&id, |command| {
            let result = command.exec.resolved();
            let start = command.take_snapshot_offset();
            let window = command.transcript_window(start, usize::MAX);
            let elapsed = command.elapsed_seconds();
            let workspace_session_id = command.workspace_session_id.clone();
            let outcome = command.finalize_outcome.get();
            let publish_rejected = outcome.and_then(|outcome| outcome.publish_reject_class);
            let finalization_failure_class =
                outcome.and_then(|outcome| outcome.finalization_failure_class);
            let finalization_attempts = outcome.and_then(|outcome| outcome.finalization_attempts);
            (
                result,
                window,
                elapsed,
                workspace_session_id,
                publish_rejected,
                finalization_failure_class,
                finalization_attempts,
            )
        });
        let read = read.or_else(|| {
            self.terminal_drains().with(&command_session_id, |record| {
                let result = Some(record.result.clone());
                let start = record.next_snapshot_offset;
                let window = record.window(start, usize::MAX);
                let elapsed = record.elapsed_seconds();
                let workspace_session_id = record.workspace_session_id.clone();
                let outcome = record.finalize_outcome.get();
                let publish_rejected = outcome.and_then(|outcome| outcome.publish_reject_class);
                let finalization_failure_class =
                    outcome.and_then(|outcome| outcome.finalization_failure_class);
                let finalization_attempts =
                    outcome.and_then(|outcome| outcome.finalization_attempts);
                (
                    result,
                    window,
                    elapsed,
                    workspace_session_id,
                    publish_rejected,
                    finalization_failure_class,
                    finalization_attempts,
                )
            })
        });
        let Some((
            result,
            window,
            elapsed,
            workspace_session_id,
            publish_rejected,
            finalization_failure_class,
            finalization_attempts,
        )) = read
        else {
            return command_not_found(command_session_id);
        };
        let result = match result {
            Some(Ok(result)) => result,
            Some(Err(error)) => {
                return Err(finalization_failed(command_session_id, &error));
            }
            None => return command_not_found(command_session_id),
        };
        let has_more_output = window.output_truncated;
        let returned_command_session_id =
            (include_terminal_command_session_id || has_more_output).then_some(command_session_id);
        let mut output = command_output(
            window,
            returned_command_session_id,
            command_status(result.status),
            Some(result.exit_code),
            elapsed,
            result.command_total_time_seconds,
        );
        output.workspace_session_id = Some(workspace_session_id);
        output.publish_rejected = publish_rejected;
        output.finalization_failed = finalization_failure_class;
        output.finalization_attempts = finalization_attempts;
        Ok(output)
    }
}

pub(crate) fn finalization_failed(
    command_session_id: NamespaceExecutionId,
    error: &NamespaceExecutionError,
) -> CommandServiceError {
    CommandServiceError::CommandFinalizationFailed {
        command_session_id,
        error: finalize_message(error),
    }
}

pub(crate) fn command_not_found<T>(
    command_session_id: NamespaceExecutionId,
) -> Result<T, CommandServiceError> {
    Err(CommandServiceError::CommandNotFound { command_session_id })
}

pub(crate) fn finalize_message(error: &NamespaceExecutionError) -> String {
    match error {
        NamespaceExecutionError::Finalize(message) => message.clone(),
        other => other.to_string(),
    }
}
