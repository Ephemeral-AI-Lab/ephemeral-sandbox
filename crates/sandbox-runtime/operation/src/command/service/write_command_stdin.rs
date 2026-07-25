use crate::command::service::r#yield::{command_not_found, finalize_message};
use crate::command::service::CommandOperationService;
use crate::command::{CommandOutput, CommandServiceError, WriteCommandStdinInput};
use sandbox_runtime_namespace_execution::{OutputActivity, OutputActivitySnapshot};
use std::sync::Arc;
use std::time::{Duration, Instant};

impl CommandOperationService {
    pub fn write_command_stdin(
        &self,
        input: WriteCommandStdinInput,
    ) -> Result<CommandOutput, CommandServiceError> {
        let command_session_id = input.command_session_id;
        let yield_time_ms = input.yield_time_ms.unwrap_or(1000);
        let is_kill_input = is_kill_input(&input.stdin);
        let id = command_session_id.clone();

        let target = self.engine().with_value(&id, |command| {
            if !command.exec.is_finished() {
                let output_activity = command.exec.output_activity();
                let observed_output = output_activity.snapshot();
                let write_error = if is_kill_input {
                    command.exec.cancel();
                    None
                } else {
                    command
                        .exec
                        .write_stdin(input.stdin.as_bytes())
                        .err()
                        .map(|error| error.to_string())
                };
                return WriteTarget::Live {
                    output_activity,
                    observed_output,
                    write_error,
                };
            }
            match command.exec.resolved() {
                Some(Err(error)) => WriteTarget::FinalizationFailed(finalize_message(&error)),
                _ => WriteTarget::AlreadyCompleted,
            }
        });
        let (output_activity, observed_output) = match target {
            None => return command_not_found(command_session_id),
            Some(WriteTarget::AlreadyCompleted) => {
                return Err(CommandServiceError::CommandAlreadyCompleted { command_session_id });
            }
            Some(WriteTarget::FinalizationFailed(error)) => {
                return Err(CommandServiceError::CommandFinalizationFailed {
                    command_session_id,
                    error,
                });
            }
            Some(WriteTarget::Live {
                output_activity,
                observed_output,
                write_error,
            }) => {
                if let Some(error) = write_error {
                    return Err(CommandServiceError::CommandIo {
                        command_session_id,
                        error,
                    });
                }
                (output_activity, observed_output)
            }
        };
        let observed_output_bytes = observed_output.output_bytes();

        let wait_time_ms = if is_kill_input { 1000 } else { yield_time_ms };
        if !is_kill_input && wait_time_ms > 0 {
            let timeout = Duration::from_millis(wait_time_ms);
            let deadline = Instant::now() + timeout;
            let mut observed_output = observed_output;
            let echo_byte_bound = max_terminal_echo_bytes(&input.stdin);
            loop {
                if observed_output.is_closed() {
                    let remaining_ms = u64::try_from(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis(),
                    )
                    .unwrap_or(u64::MAX);
                    return self.wait_for_command_yield(command_session_id, remaining_ms, true);
                }
                if observed_output
                    .output_bytes()
                    .saturating_sub(observed_output_bytes)
                    > echo_byte_bound
                {
                    return self.wait_for_command_yield(command_session_id, 0, true);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return self.wait_for_command_yield(command_session_id, 0, true);
                }
                let next_output = output_activity.wait_for_change(observed_output, remaining);
                if next_output == observed_output {
                    return self.wait_for_command_yield(command_session_id, 0, true);
                }
                observed_output = next_output;
            }
        }
        self.wait_for_command_yield(command_session_id, wait_time_ms, true)
    }
}

enum WriteTarget {
    Live {
        output_activity: Arc<OutputActivity>,
        observed_output: OutputActivitySnapshot,
        write_error: Option<String>,
    },
    AlreadyCompleted,
    FinalizationFailed(String),
}

fn is_kill_input(stdin: &str) -> bool {
    stdin.contains('\u{3}') || stdin.contains('\u{4}')
}

fn max_terminal_echo_bytes(stdin: &str) -> u64 {
    u64::try_from(stdin.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(2)
}

#[cfg(test)]
mod tests {
    use super::max_terminal_echo_bytes;

    #[test]
    fn terminal_echo_bound_covers_control_expansion() {
        assert_eq!(max_terminal_echo_bytes("input\n"), 12);
        assert_eq!(max_terminal_echo_bytes("\u{7}"), 2);
    }
}
