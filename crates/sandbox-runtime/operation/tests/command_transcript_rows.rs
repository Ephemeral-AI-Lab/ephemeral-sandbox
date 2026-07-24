pub mod support;

use std::path::PathBuf;
use std::sync::Arc;

use sandbox_runtime::command::{
    CommandStatus, ExecCommandInput, ReadCommandLinesInput, WriteCommandStdinInput,
};
use sandbox_runtime::NamespaceExecutionId;
use sandbox_runtime_workspace::NetworkProfile;

use support::{
    build_services_with_launch_driver, create_request, success_exit, workspace_handle,
    FakeLaunchDriver, FakeWorkspaceService, ScriptedCommandYield, TestServices,
};

/// Build a running (not yet completed) command session whose transcript file
/// already contains `transcript`. This reproduces `TranscriptLaunchDriver::running`
/// by pushing a `Running(transcript)` outcome: `FakeRunnerScript::running` writes
/// the bytes to the transcript file at spawn time and parks the child.
fn session_with_transcript(
    transcript: &str,
) -> (TestServices, NamespaceExecutionId, Arc<FakeLaunchDriver>) {
    let driver = Arc::new(FakeLaunchDriver::new());
    driver.push_outcome(ScriptedCommandYield::Running(transcript.to_owned()));
    let (env, id) = build_session(&driver);
    (env, id, driver)
}

/// Build a running command session with no transcript output (no file written).
/// Reproduces `MissingTranscriptLaunchDriver::running`: command stays alive, no
/// bytes are written because `FakeRunnerScript::running(Vec::new())` is a no-op.
fn session_pending() -> (TestServices, NamespaceExecutionId, Arc<FakeLaunchDriver>) {
    let driver = Arc::new(FakeLaunchDriver::new());
    driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let (env, id) = build_session(&driver);
    (env, id, driver)
}

/// Build a completed command session whose transcript file contains `transcript`.
/// Reproduces `TranscriptLaunchDriver::running_then_completed` by pushing a
/// single `Completed` outcome that writes `transcript` to the file at spawn
/// time and fires the watcher immediately. Uses a longer yield window so the
/// watcher thread has time to resolve the promise before the yield loop exits.
fn completed_session_with_transcript(transcript: &str) -> (TestServices, NamespaceExecutionId) {
    let driver = Arc::new(FakeLaunchDriver::new());
    driver.push_outcome(ScriptedCommandYield::Completed(success_exit(transcript)));
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "lease-1",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    )));
    let env = build_services_with_launch_driver(Arc::clone(&fake), Arc::clone(&driver));
    let handler = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds");

    // Use yield_time_ms = 500 so the yield loop waits long enough for the
    // watcher thread to resolve the promise after the Completed script fires.
    let output = env
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(handler.workspace_session_id.clone()),
            cmd: "printf rows".to_owned(),
            timeout_ms: None,
            yield_time_ms: Some(500),
        })
        .expect("command exec succeeds");

    // Completed commands with small output return command_session_id=None;
    // recover the id from the exec id allocator (first command = namespace_execution_1).
    let command_session_id = output
        .command_session_id
        .unwrap_or_else(|| NamespaceExecutionId("namespace_execution_1".to_owned()));
    (env, command_session_id)
}

fn build_session(driver: &Arc<FakeLaunchDriver>) -> (TestServices, NamespaceExecutionId) {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "lease-1",
        workspace_root.clone(),
        NetworkProfile::Shared,
    )));
    let env = build_services_with_launch_driver(Arc::clone(&fake), Arc::clone(driver));
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

    // When a Completed script fires before the yield check the command_session_id
    // may be None; fall back to the deterministic first-allocated ID.
    let command_session_id = output
        .command_session_id
        .unwrap_or_else(|| NamespaceExecutionId("namespace_execution_1".to_owned()));
    (env, command_session_id)
}

#[test]
fn command_transcript_rows_preserve_offsets_streams_and_window_metadata() {
    let transcript = concat!(
        "{\"offset\":0,\"stream\":\"stdout\",\"text\":\"first\"}\n",
        "{\"offset\":1,\"stream\":\"stderr\",\"text\":\"warning\"}\n",
        "{\"offset\":2,\"stream\":\"stdout\",\"text\":\"third\"}\n",
    );
    let (env, command_session_id, _driver) = session_with_transcript(transcript);

    let output = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id: command_session_id.clone(),
        start_offset: Some(1),
        limit: Some(1),
    });

    assert_eq!(output.command_session_id, Some(command_session_id.clone()));
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
    let (env, command_session_id, _driver) = session_with_transcript(transcript);

    let output = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id,
        start_offset: None,
        limit: None,
    });

    assert_eq!(output.end_offset, 3);
    assert_eq!(output.total_lines, 3);
    assert_eq!(output.output, "first\nsecond\nthird");
}

#[test]
fn command_transcript_rows_keep_empty_window_end_offset_at_request() {
    let (env, command_session_id, _driver) = session_with_transcript("one\ntwo\nthree\n");

    let output = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id,
        start_offset: Some(10),
        limit: Some(5),
    });

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
    let (env, command_session_id, _driver) = session_with_transcript(&transcript);

    let output = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id,
        start_offset: Some(0),
        limit: Some(10),
    });

    assert_eq!(output.start_offset, 0);
    assert_eq!(output.total_lines, 5);
    assert_eq!(output.end_offset, 5);
    assert_eq!(output.output, "kept-one\nkept-two");
}

#[test]
fn command_transcript_rows_allow_active_missing_transcript_as_empty_pending_window() {
    // MissingTranscriptLaunchDriver::running() — no transcript output, command stays alive.
    let (env, command_session_id, _driver) = session_pending();

    let output = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id,
        start_offset: Some(0),
        limit: Some(10),
    });

    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.exit_code, None);
    assert_eq!(output.end_offset, 0);
    assert_eq!(output.total_lines, 0);
    assert!(output.output.is_empty());
}

#[test]
fn command_transcript_rows_completed_missing_transcript_reads_empty_best_effort() {
    // command completes but the transcript file is absent. The infallible reader
    // returns the terminal status with an empty best-effort window rather than an
    // error: deleting the transcript after completion reproduces the "missing" case.
    let driver = Arc::new(FakeLaunchDriver::new());
    driver.push_outcome(ScriptedCommandYield::Completed(success_exit(
        "terminal stdout\n",
    )));
    let (env, command_session_id) = build_session(&driver);

    let _ = env.command.write_command_stdin(WriteCommandStdinInput {
        command_session_id: command_session_id.clone(),
        stdin: "\n".to_owned(),
        yield_time_ms: Some(1),
    });

    let transcript_path = env
        .command
        .config()
        .scratch_root
        .join("workspace-session")
        .join("executions")
        .join("namespace_execution_1")
        .join("transcript.log");
    let _ = std::fs::remove_file(&transcript_path);

    let output = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id: command_session_id.clone(),
        start_offset: Some(0),
        limit: Some(10),
    });

    assert_eq!(output.command_session_id, Some(command_session_id));
    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.total_lines, 0);
    assert!(output.output.is_empty());
}

#[test]
fn command_transcript_rows_keep_completed_rows() {
    // TranscriptLaunchDriver::running_then_completed(transcript, "terminal stdout\n"):
    // Reproduced by a single Completed outcome that writes the transcript rows to
    // the file at spawn time and fires the watcher immediately.
    let transcript = "completed one\ncompleted two\ncompleted three\n";
    let (env, command_session_id) = completed_session_with_transcript(transcript);

    let lines = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id: command_session_id.clone(),
        start_offset: Some(0),
        limit: Some(10),
    });
    assert_eq!(lines.status, CommandStatus::Ok);
    assert_eq!(lines.exit_code, Some(0));
    assert_eq!(lines.total_lines, 3);
    assert_eq!(
        lines.output,
        "completed one\ncompleted two\ncompleted three"
    );

    let window = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id: command_session_id.clone(),
        start_offset: Some(1),
        limit: Some(1),
    });
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
        NetworkProfile::Shared,
    )));
    let driver = Arc::new(FakeLaunchDriver::new());
    driver.push_outcome(ScriptedCommandYield::Running("implicit row\n".to_owned()));
    let env = build_services_with_launch_driver(fake, Arc::clone(&driver));
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

    let rows = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id,
        start_offset: Some(0),
        limit: Some(1),
    });

    assert_eq!(rows.status, CommandStatus::Running);
    assert_eq!(rows.exit_code, None);
    assert_eq!(rows.total_lines, 1);
}
