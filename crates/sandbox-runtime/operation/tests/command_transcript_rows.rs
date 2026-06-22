pub mod support;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sandbox_runtime::command::{
    CommandLaunchDriver, CommandServiceError, CommandSessionId, CommandStatus, ExecCommandInput,
    ReadCommandLinesInput, WriteCommandStdinInput,
};
use sandbox_runtime_command::process::{
    CommandProcess, CommandProcessExit, CommandProcessSpawn, CommandProcessSpec,
};
use sandbox_runtime_command::yield_wait_loop::WaitOutcome;
use sandbox_runtime_workspace::{WorkspaceEntry, WorkspaceProfile};

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

    fn running_then_completed(transcript: &str, stdout: &str) -> Self {
        Self {
            transcript: transcript.to_owned(),
            outcomes: Mutex::new(VecDeque::from([
                WaitOutcome::Running(String::new()),
                WaitOutcome::Completed(success_exit(stdout)),
            ])),
        }
    }
}

impl MissingTranscriptLaunchDriver {
    fn running() -> Self {
        Self {
            outcomes: Mutex::new(VecDeque::from([WaitOutcome::Running(String::new())])),
        }
    }

    fn running_then_completed(stdout: &str) -> Self {
        Self {
            outcomes: Mutex::new(VecDeque::from([
                WaitOutcome::Running(String::new()),
                WaitOutcome::Completed(success_exit(stdout)),
            ])),
        }
    }
}

impl CommandLaunchDriver for TranscriptLaunchDriver {
    fn spawn(
        &self,
        spec: CommandProcessSpec,
        workspace_entry: WorkspaceEntry,
        config: &sandbox_runtime_command::CommandConfig,
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
        Ok(CommandProcess::inactive_with_transcript_for_test(
            spec,
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
        config: &sandbox_runtime_command::CommandConfig,
    ) -> Result<CommandProcess, CommandServiceError> {
        let parts =
            CommandProcessSpawn::prepare(&spec.id, workspace_entry, config).map_err(|error| {
                CommandServiceError::CommandIo {
                    command_session_id: CommandSessionId(spec.id.clone()),
                    error: error.to_string(),
                }
            })?;
        Ok(CommandProcess::inactive_with_transcript_for_test(
            spec,
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
        WorkspaceProfile::HostCompatible,
    )));
    let env = build_services_with_launch_driver(Arc::clone(&fake), Arc::new(driver));
    let handler = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds");

    let output = env
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(handler.workspace_session_id.clone()),
            cmd: "printf rows".to_owned(),
            timeout_ms: None,
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

fn completed_session_with_driver(
    driver: impl CommandLaunchDriver + 'static,
) -> (TestServices, CommandSessionId) {
    let (env, command_session_id) = session_with_driver(driver);
    env.command
        .write_command_stdin(WriteCommandStdinInput {
            command_session_id: command_session_id.clone(),
            stdin: "\n".to_owned(),
            yield_time_ms: Some(1),
        })
        .expect("command finalizes after stdin write");
    (env, command_session_id)
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
            start_offset: Some(1),
            limit: Some(1),
        })
        .expect("owner can read active command rows");

    assert_eq!(output.command_session_id, command_session_id);
    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.exit_code, None);
    assert_eq!(output.start_offset, 1);
    assert_eq!(output.end_offset, 2);
    assert_eq!(output.total_lines, 3);
    assert_eq!(output.original_token_count, 2);
    assert_eq!(output.output, "warning");
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
            start_offset: None,
            limit: None,
        })
        .expect("owner can read raw transcript rows with default window");

    assert_eq!(output.end_offset, 3);
    assert_eq!(output.total_lines, 3);
    assert_eq!(output.output, "first\nsecond\nthird");
}

#[test]
fn command_transcript_rows_keep_empty_window_end_offset_at_request() {
    let (env, command_session_id) =
        session_with_driver(TranscriptLaunchDriver::running("one\ntwo\nthree\n"));

    let output = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: Some(10),
            limit: Some(5),
        })
        .expect("owner can request beyond retained rows");

    assert_eq!(output.start_offset, 10);
    assert_eq!(output.end_offset, 10);
    assert_eq!(output.total_lines, 3);
    assert!(output.output.is_empty());
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
            start_offset: Some(0),
            limit: Some(10),
        })
        .expect("owner can read bounded row window");

    assert_eq!(output.start_offset, 0);
    assert_eq!(output.total_lines, 5);
    assert_eq!(output.end_offset, 5);
    assert_eq!(output.output, "kept-one\nkept-two");
}

#[test]
fn command_transcript_rows_allow_active_missing_transcript_as_empty_pending_window() {
    let (env, command_session_id) = session_with_driver(MissingTranscriptLaunchDriver::running());

    let output = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: Some(0),
            limit: Some(10),
        })
        .expect("active command without output yet returns an empty pending window");

    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.exit_code, None);
    assert_eq!(output.end_offset, 0);
    assert_eq!(output.total_lines, 0);
    assert!(output.output.is_empty());
}

#[test]
fn command_transcript_rows_error_when_completed_transcript_is_missing() {
    let (env, command_session_id) = completed_session_with_driver(
        MissingTranscriptLaunchDriver::running_then_completed("terminal stdout\n"),
    );

    let error = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id: command_session_id.clone(),
            start_offset: Some(0),
            limit: Some(10),
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
    let (env, command_session_id) = completed_session_with_driver(
        TranscriptLaunchDriver::running_then_completed(transcript, "terminal stdout\n"),
    );

    let lines = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id: command_session_id.clone(),
            start_offset: Some(0),
            limit: Some(10),
        })
        .expect("owner can read completed rows");
    assert_eq!(lines.status, CommandStatus::Ok);
    assert_eq!(lines.exit_code, Some(0));
    assert_eq!(lines.total_lines, 3);
    assert_eq!(
        lines.output,
        "completed one\ncompleted two\ncompleted three"
    );

    let window = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id: command_session_id.clone(),
            start_offset: Some(1),
            limit: Some(1),
        })
        .expect("owner can read a completed command window");
    assert_eq!(window.status, CommandStatus::Ok);
    assert_eq!(window.exit_code, Some(0));
    assert_eq!(window.total_lines, 3);
    assert_eq!(window.end_offset, 2);
    assert_eq!(window.output, "completed two");
}

#[test]
fn command_transcript_rows_report_running_status_for_active_command() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::HostCompatible,
    )));
    let env = build_services_with_launch_driver(
        fake,
        Arc::new(TranscriptLaunchDriver::running("one-shot row\n")),
    );
    let handler = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds");
    let output = env
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(handler.workspace_session_id),
            cmd: "printf rows".to_owned(),
            timeout_ms: None,
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
            start_offset: Some(0),
            limit: Some(1),
        })
        .expect("active rows can be read");

    assert_eq!(rows.status, CommandStatus::Running);
    assert_eq!(rows.exit_code, None);
    assert_eq!(rows.total_lines, 1);
}
