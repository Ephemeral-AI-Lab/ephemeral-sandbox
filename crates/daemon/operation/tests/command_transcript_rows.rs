pub mod support;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use command::process::{
    CommandProcess, CommandProcessExit, CommandProcessSpawn, CommandProcessSpec,
};
use command::yield_wait_loop::WaitOutcome;
use daemon_operation::command::{
    CommandLaunchDriver, CommandServiceError, CommandSessionId, CommandStatus, CommandStream,
    CommandTranscriptRow, ExecCommandInput, PollCommandInput, ReadCommandLinesInput,
};
use workspace::{WorkspaceEntry, WorkspaceProfile};

use support::{
    build_services_with_launch_driver, create_request, success_exit, workspace_handle,
    FakeWorkspaceService, TestServices,
};

#[derive(Debug)]
struct TranscriptLaunchDriver {
    transcript: String,
    outcomes: Mutex<VecDeque<WaitOutcome<CommandProcessExit>>>,
}

#[derive(Debug)]
struct MissingTranscriptLaunchDriver {
    outcomes: Mutex<VecDeque<WaitOutcome<CommandProcessExit>>>,
}

impl TranscriptLaunchDriver {
    fn running(transcript: &str) -> Self {
        Self {
            transcript: transcript.to_owned(),
            outcomes: Mutex::new(VecDeque::from([WaitOutcome::Running(String::new())])),
        }
    }

    fn completed(transcript: &str, stdout: &str) -> Self {
        Self {
            transcript: transcript.to_owned(),
            outcomes: Mutex::new(VecDeque::from([WaitOutcome::Completed(success_exit(
                stdout,
            ))])),
        }
    }
}

impl MissingTranscriptLaunchDriver {
    fn running() -> Self {
        Self {
            outcomes: Mutex::new(VecDeque::from([WaitOutcome::Running(String::new())])),
        }
    }

    fn completed(stdout: &str) -> Self {
        Self {
            outcomes: Mutex::new(VecDeque::from([WaitOutcome::Completed(success_exit(
                stdout,
            ))])),
        }
    }
}

impl CommandLaunchDriver for TranscriptLaunchDriver {
    fn spawn(
        &self,
        spec: CommandProcessSpec,
        workspace_entry: WorkspaceEntry,
        config: &command::CommandConfig,
    ) -> Result<CommandProcess, CommandServiceError> {
        let parts =
            CommandProcessSpawn::prepare(&spec.id, workspace_entry, config).map_err(|error| {
                CommandServiceError::CommandIo {
                    command_session_id: CommandSessionId(spec.id.clone()),
                    error: error.to_string(),
                }
            })?;
        if let Some(parent) = parts.transcript_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| CommandServiceError::CommandIo {
                command_session_id: CommandSessionId(spec.id.clone()),
                error: error.to_string(),
            })?;
        }
        std::fs::write(&parts.transcript_path, &self.transcript).map_err(|error| {
            CommandServiceError::CommandIo {
                command_session_id: CommandSessionId(spec.id.clone()),
                error: error.to_string(),
            }
        })?;
        Ok(CommandProcess::inactive_with_artifacts_for_test(
            spec,
            parts.output_path,
            parts.final_path,
            parts.transcript_path,
        ))
    }

    fn wait_for_initial_yield(
        &self,
        _process: &CommandProcess,
        _yield_time_ms: u64,
        _start_offset: u64,
    ) -> WaitOutcome<CommandProcessExit> {
        self.outcomes
            .lock()
            .expect("test operation succeeds")
            .pop_front()
            .unwrap_or_else(|| WaitOutcome::Running(String::new()))
    }
}

impl CommandLaunchDriver for MissingTranscriptLaunchDriver {
    fn spawn(
        &self,
        spec: CommandProcessSpec,
        workspace_entry: WorkspaceEntry,
        config: &command::CommandConfig,
    ) -> Result<CommandProcess, CommandServiceError> {
        let parts =
            CommandProcessSpawn::prepare(&spec.id, workspace_entry, config).map_err(|error| {
                CommandServiceError::CommandIo {
                    command_session_id: CommandSessionId(spec.id.clone()),
                    error: error.to_string(),
                }
            })?;
        Ok(CommandProcess::inactive_with_artifacts_for_test(
            spec,
            parts.output_path,
            parts.final_path,
            parts.transcript_path,
        ))
    }

    fn wait_for_initial_yield(
        &self,
        _process: &CommandProcess,
        _yield_time_ms: u64,
        _start_offset: u64,
    ) -> WaitOutcome<CommandProcessExit> {
        self.outcomes
            .lock()
            .expect("test operation succeeds")
            .pop_front()
            .unwrap_or_else(|| WaitOutcome::Running(String::new()))
    }
}

fn session_with_driver(
    driver: impl CommandLaunchDriver + 'static,
) -> (TestServices, CommandSessionId) {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::SharedNetwork,
    )));
    let env = build_services_with_launch_driver(Arc::clone(&fake), Arc::new(driver));
    let handler = env
        .workspace
        .create_workspace_session(create_request(workspace_root.clone()))
        .expect("session create succeeds");

    let output = env
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: handler.workspace_session_id.clone(),
            cmd: "printf rows".to_owned(),
            timeout_seconds: None,
            yield_time_ms: Some(0),
        })
        .expect("command exec succeeds");

    (
        env,
        output
            .command_session_id
            .expect("command session id is returned by exec"),
    )
}

#[test]
fn command_transcript_rows_preserve_offsets_streams_and_window_metadata() {
    let transcript = concat!(
        "{\"offset\":0,\"stream\":\"stdout\",\"text\":\"first\"}\n",
        "{\"offset\":1,\"stream\":\"stderr\",\"text\":\"warning\"}\n",
        "{\"offset\":2,\"stream\":\"stdout\",\"text\":\"third\"}\n",
    );
    let (env, command_session_id) =
        session_with_driver(TranscriptLaunchDriver::running(transcript));

    let output = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id: command_session_id.clone(),
            start_offset: 1,
            limit: 1,
        })
        .expect("owner can read active command rows");

    assert_eq!(output.command_session_id, command_session_id);
    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.exit_code, None);
    assert_eq!(output.start_offset, 1);
    assert_eq!(output.end_offset, 2);
    assert_eq!(output.total_lines, 3);
    assert_eq!(output.truncated_before, 0);
    assert!(output.output_truncated);
    assert_eq!(
        output.output,
        vec![CommandTranscriptRow {
            offset: 1,
            stream: CommandStream::Stderr,
            text: "warning".to_owned(),
        }]
    );
}

#[test]
fn command_transcript_rows_parse_raw_pty_transcript_as_stdout_rows() {
    let transcript = concat!(
        "[2026-06-18T01:02:03.004Z] first\n",
        "[2026-06-18T09:02:03.004+08:00] second\n",
        "third\n",
    );
    let (env, command_session_id) =
        session_with_driver(TranscriptLaunchDriver::running(transcript));

    let output = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: 0,
            limit: 10,
        })
        .expect("owner can read raw transcript rows");

    assert_eq!(output.end_offset, 3);
    assert_eq!(output.total_lines, 3);
    assert!(!output.output_truncated);
    assert_eq!(
        output.output,
        vec![
            CommandTranscriptRow {
                offset: 0,
                stream: CommandStream::Stdout,
                text: "first".to_owned(),
            },
            CommandTranscriptRow {
                offset: 1,
                stream: CommandStream::Stdout,
                text: "second".to_owned(),
            },
            CommandTranscriptRow {
                offset: 2,
                stream: CommandStream::Stdout,
                text: "third".to_owned(),
            },
        ]
    );
}

#[test]
fn command_transcript_rows_keep_empty_window_end_offset_at_request() {
    let (env, command_session_id) =
        session_with_driver(TranscriptLaunchDriver::running("one\ntwo\nthree\n"));

    let output = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: 10,
            limit: 5,
        })
        .expect("owner can request beyond retained rows");

    assert_eq!(output.start_offset, 10);
    assert_eq!(output.end_offset, 10);
    assert_eq!(output.total_lines, 3);
    assert_eq!(output.truncated_before, 0);
    assert!(output.output.is_empty());
    assert!(!output.output_truncated);
}

#[test]
fn command_transcript_rows_report_bounded_window_truncation() {
    let mut transcript = String::from("old-one\nold-two\n");
    transcript.push_str(&"x".repeat(1024 * 1024 + 128));
    transcript.push('\n');
    transcript.push_str("kept-one\nkept-two\n");
    let (env, command_session_id) =
        session_with_driver(TranscriptLaunchDriver::running(&transcript));

    let output = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: 0,
            limit: 10,
        })
        .expect("owner can read bounded row window");

    assert_eq!(output.start_offset, 0);
    assert_eq!(output.truncated_before, 3);
    assert_eq!(output.total_lines, 5);
    assert_eq!(output.end_offset, 5);
    assert!(output.output_truncated);
    assert_eq!(
        output.output,
        vec![
            CommandTranscriptRow {
                offset: 3,
                stream: CommandStream::Stdout,
                text: "kept-one".to_owned(),
            },
            CommandTranscriptRow {
                offset: 4,
                stream: CommandStream::Stdout,
                text: "kept-two".to_owned(),
            },
        ]
    );
}

#[test]
fn command_transcript_rows_allow_active_missing_transcript_as_empty_pending_window() {
    let (env, command_session_id) = session_with_driver(MissingTranscriptLaunchDriver::running());

    let output = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: 0,
            limit: 10,
        })
        .expect("active command without output yet returns an empty pending window");

    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.exit_code, None);
    assert_eq!(output.end_offset, 0);
    assert_eq!(output.total_lines, 0);
    assert!(!output.output_truncated);
    assert!(output.output.is_empty());
}

#[test]
fn command_transcript_rows_error_when_completed_transcript_is_missing() {
    let (env, command_session_id) = session_with_driver(MissingTranscriptLaunchDriver::completed(
        "terminal stdout\n",
    ));

    let error = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id: command_session_id.clone(),
            start_offset: 0,
            limit: 10,
        })
        .expect_err("completed command with missing retained transcript is not empty output");

    assert!(matches!(
        error,
        CommandServiceError::CommandTranscriptUnavailable { command_session_id: id, path: Some(path), error }
            if id == command_session_id
                && path.ends_with("transcript.log")
                && error.contains("open transcript")
    ));
}

#[test]
fn command_transcript_rows_keep_completed_rows() {
    let transcript = "completed one\ncompleted two\ncompleted three\n";
    let (env, command_session_id) = session_with_driver(TranscriptLaunchDriver::completed(
        transcript,
        "terminal stdout\n",
    ));

    let lines = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id: command_session_id.clone(),
            start_offset: 0,
            limit: 10,
        })
        .expect("owner can read completed rows");
    let poll = env
        .command
        .poll(PollCommandInput {
            command_session_id: command_session_id.clone(),
            last_n_lines: Some(10),
        })
        .expect("owner can poll completed command");

    assert_eq!(lines.status, CommandStatus::Completed);
    assert_eq!(lines.exit_code, Some(0));
    assert_eq!(poll.status, lines.status);
    assert_eq!(poll.exit_code, lines.exit_code);
    assert_eq!(lines.total_lines, 3);
    assert_eq!(
        lines.output,
        vec![
            CommandTranscriptRow {
                offset: 0,
                stream: CommandStream::Stdout,
                text: "completed one".to_owned(),
            },
            CommandTranscriptRow {
                offset: 1,
                stream: CommandStream::Stdout,
                text: "completed two".to_owned(),
            },
            CommandTranscriptRow {
                offset: 2,
                stream: CommandStream::Stdout,
                text: "completed three".to_owned(),
            },
        ]
    );

    let window = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id: command_session_id.clone(),
            start_offset: 1,
            limit: 1,
        })
        .expect("owner can read a completed command window");
    assert_eq!(window.status, CommandStatus::Completed);
    assert_eq!(window.exit_code, Some(0));
    assert_eq!(window.total_lines, 3);
    assert_eq!(window.truncated_before, 0);
    assert_eq!(window.end_offset, 2);
    assert!(window.output_truncated);
    assert_eq!(
        window.output,
        vec![CommandTranscriptRow {
            offset: 1,
            stream: CommandStream::Stdout,
            text: "completed two".to_owned(),
        }]
    );
}

#[test]
fn command_transcript_rows_report_running_status_for_active_command() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::SharedNetwork,
    )));
    let env = build_services_with_launch_driver(
        fake,
        Arc::new(TranscriptLaunchDriver::running("one-shot row\n")),
    );
    let handler = env
        .workspace
        .create_workspace_session(create_request(workspace_root))
        .expect("session create succeeds");
    let output = env
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: handler.workspace_session_id,
            cmd: "printf rows".to_owned(),
            timeout_seconds: None,
            yield_time_ms: Some(0),
        })
        .expect("command starts");
    let command_session_id = output
        .command_session_id
        .expect("command session id is returned");

    let rows = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: 0,
            limit: 1,
        })
        .expect("active rows can be read");

    assert_eq!(rows.status, CommandStatus::Running);
    assert_eq!(rows.exit_code, None);
    assert_eq!(rows.total_lines, 1);
}
