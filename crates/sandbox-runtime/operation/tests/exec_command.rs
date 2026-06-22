mod support;

use std::path::PathBuf;
use std::sync::Arc;

use sandbox_runtime::command::{
    CommandServiceError, CommandSessionId, CommandStatus, ExecCommandInput, ReadCommandLinesInput,
    WriteCommandStdinInput,
};
use sandbox_runtime_command::yield_wait_loop::WaitOutcome;
use sandbox_runtime_workspace::{WorkspaceProfile, WorkspaceSessionId};

use support::{
    build_services, build_services_with_launch_driver, create_request, success_exit,
    workspace_handle, workspace_handle_unavailable_launch, workspace_handle_without_launch,
    FakeLaunchDriver, FakeWorkspaceService,
};

fn exec_input(workspace_session_id: WorkspaceSessionId) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: Some(workspace_session_id),
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(0),
    }
}

fn one_shot_exec_input() -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: None,
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(0),
    }
}

fn create_session(
    fake: &Arc<FakeWorkspaceService>,
    env: &support::TestServices,
    workspace_session_id: &str,
    workspace_root: PathBuf,
    profile: WorkspaceProfile,
) -> WorkspaceSessionId {
    fake.push_create_result(Ok(workspace_handle(
        workspace_session_id,
        "lease-1",
        workspace_root.clone(),
        profile,
    )));
    env.workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id
}

#[test]
fn exec_command_uses_resolved_session_without_workspace_create_or_destroy() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let env = build_services(Arc::clone(&fake));
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );
    let create_count_before_exec = fake.create_requests().len();

    let output = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect("session command exec succeeds");

    let command_session_id = output
        .command_session_id
        .expect("running command session id is returned");
    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(fake.create_requests().len(), create_count_before_exec);
    assert!(fake.destroy_calls().is_empty());
    let lines = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id: command_session_id.clone(),
            start_offset: Some(0),
            limit: Some(10),
        })
        .expect("session command can be read");
    assert_eq!(lines.command_session_id, command_session_id);
    assert_eq!(lines.status, CommandStatus::Running);
}

#[test]
fn exec_command_rejects_empty_command_before_workspace_resolution() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let env = build_services(Arc::clone(&fake));
    let mut input = exec_input(WorkspaceSessionId("workspace-session".to_owned()));
    input.cmd = "   ".to_owned();

    let error = env
        .command
        .exec_command(input)
        .expect_err("empty command rejects exec");

    assert!(matches!(
        error,
        CommandServiceError::InvalidCommand { message } if message == "cmd must be non-empty"
    ));
    assert!(fake.create_requests().is_empty());
    assert!(fake.destroy_calls().is_empty());
}

#[test]
fn exec_command_without_workspace_session_creates_and_destroys_one_shot_on_completion() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "one-shot-session",
        "lease-1",
        PathBuf::from("/workspace/one-shot"),
        WorkspaceProfile::HostCompatible,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Completed(success_exit("one-shot done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let output = env
        .command
        .exec_command(one_shot_exec_input())
        .expect("one-shot command completes");

    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.output, "one-shot done");
    assert!(output.command_session_id.is_none());
    assert_eq!(fake.create_requests(), vec![create_request()]);
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("one-shot-session".to_owned())]
    );
}

#[test]
fn exec_command_terminal_output_returns_command_session_id_when_more_output_remains() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "one-shot-session",
        "lease-1",
        PathBuf::from("/workspace/one-shot"),
        WorkspaceProfile::HostCompatible,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let stdout = format!("{}\nkept\n", "x".repeat(1024 * 1024 + 128));
    launch_driver.push_outcome(WaitOutcome::Completed(success_exit(&stdout)));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let output = env
        .command
        .exec_command(one_shot_exec_input())
        .expect("one-shot command completes");

    let command_session_id = output
        .command_session_id
        .expect("truncated terminal output keeps command session id");
    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.output, "kept");

    let lines = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: None,
            limit: None,
        })
        .expect("completed command output remains readable");
    assert_eq!(lines.output, "kept");
}

#[test]
fn exec_command_without_workspace_session_keeps_one_shot_until_terminal_completion() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "one-shot-session",
        "lease-1",
        PathBuf::from("/workspace/one-shot"),
        WorkspaceProfile::HostCompatible,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Running(String::new()));
    launch_driver.push_outcome(WaitOutcome::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let command_session_id = env
        .command
        .exec_command(one_shot_exec_input())
        .expect("one-shot command starts")
        .command_session_id
        .expect("running command session id is returned");
    assert!(fake.destroy_calls().is_empty());

    let output = env
        .command
        .write_command_stdin(WriteCommandStdinInput {
            command_session_id: command_session_id.clone(),
            stdin: "input\n".to_owned(),
            yield_time_ms: Some(250),
        })
        .expect("one-shot command completes after stdin write");

    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.command_session_id, Some(command_session_id.clone()));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("one-shot-session".to_owned())]
    );
    let lines = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: Some(0),
            limit: Some(10),
        })
        .expect("completed one-shot command can be read");
    assert_eq!(lines.status, CommandStatus::Ok);
}

#[test]
fn exec_command_without_workspace_session_destroys_one_shot_after_spawn_failure() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "one-shot-session",
        "lease-1",
        PathBuf::from("/workspace/one-shot"),
        WorkspaceProfile::HostCompatible,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_spawn_error(CommandServiceError::CommandIo {
        command_session_id: CommandSessionId("cmd_1".to_owned()),
        error: "spawn failed".to_owned(),
    });
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let error = env
        .command
        .exec_command(one_shot_exec_input())
        .expect_err("one-shot spawn failure rejects exec");

    assert!(matches!(
        error,
        CommandServiceError::CommandIo { command_session_id, error }
            if command_session_id == CommandSessionId("cmd_1".to_owned()) && error == "spawn failed"
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("one-shot-session".to_owned())]
    );
}

#[test]
fn exec_command_without_workspace_session_destroys_one_shot_after_launch_material_failure() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle_without_launch(
        "one-shot-session",
        "lease-1",
        PathBuf::from("/workspace/one-shot"),
        WorkspaceProfile::HostCompatible,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());

    let error = env
        .command
        .exec_command(one_shot_exec_input())
        .expect_err("missing launch material rejects one-shot exec");

    assert!(matches!(
        error,
        CommandServiceError::InvalidCommand { message }
            if message.contains("lacks workspace entry material")
    ));
    assert!(launch_driver.spawn_observations().is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("one-shot-session".to_owned())]
    );
}

#[test]
fn exec_command_spawn_failure_keeps_session_workspace_alive() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_spawn_error(CommandServiceError::CommandIo {
        command_session_id: CommandSessionId("cmd_1".to_owned()),
        error: "spawn failed".to_owned(),
    });
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );

    let error = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect_err("spawn failure rejects session exec");

    assert!(matches!(
        error,
        CommandServiceError::CommandIo { command_session_id, error }
            if command_session_id == CommandSessionId("cmd_1".to_owned()) && error == "spawn failed"
    ));
    assert!(fake.destroy_calls().is_empty());
    assert!(
        !env.command.config().scratch_root.join("cmd_1").exists(),
        "session spawn failure should clean up unretained command artifacts"
    );
}

#[test]
fn exec_command_passes_workspace_entry_to_spawn_paths() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    let workspace_root = PathBuf::from("/workspace/session");
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        workspace_root.clone(),
        WorkspaceProfile::Isolated,
    );
    let mut input = exec_input(workspace_session_id);
    input.timeout_ms = Some(2500);

    let output = env
        .command
        .exec_command(input)
        .expect("session command exec succeeds");

    assert_eq!(
        output.command_session_id,
        Some(CommandSessionId("cmd_1".to_owned()))
    );
    let observations = launch_driver.spawn_observations();
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(observation.spec_id, "cmd_1");
    assert_eq!(observation.spec_command, "printf ok");
    assert_eq!(observation.spec_cwd, None);
    assert_eq!(observation.spec_timeout_seconds, Some(2.5));
    assert_eq!(
        observation.transcript_path,
        env.command
            .config()
            .scratch_root
            .join("cmd_1")
            .join("transcript.log")
    );
    let entry = &observation.workspace_entry;
    assert_eq!(&entry.workspace_root, &workspace_root);
    assert_eq!(entry.layer_paths.as_slice(), &[PathBuf::from("/lower/one")]);
    assert_eq!(
        entry.upperdir.file_name().and_then(|name| name.to_str()),
        Some("upper")
    );
    assert_eq!(
        entry.workdir.file_name().and_then(|name| name.to_str()),
        Some("work")
    );
    assert_eq!(entry.ns_fds.user, 10);
    assert_eq!(entry.ns_fds.mnt, 11);
    assert_eq!(entry.ns_fds.pid, 12);
    assert_eq!(entry.ns_fds.net, Some(13));
    assert!(entry.cgroup_path.is_none());
}

#[test]
fn exec_command_missing_launch_material_rejects_without_spawn() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle_without_launch(
        "workspace-session",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::HostCompatible,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;

    let error = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect_err("missing launch material rejects exec");

    assert!(matches!(
        error,
        CommandServiceError::InvalidCommand { message }
            if message.contains("lacks workspace entry material")
    ));
    assert!(launch_driver.spawn_observations().is_empty());
    assert!(fake.destroy_calls().is_empty());
}

#[test]
fn exec_command_unavailable_workspace_launch_rejects_without_spawn() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle_unavailable_launch(
        "workspace-session",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::HostCompatible,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;

    let error = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect_err("unavailable workspace launch rejects exec");

    assert!(matches!(
        error,
        CommandServiceError::InvalidCommand { message }
            if message.contains("workspace entry context is incomplete")
    ));
    assert!(launch_driver.spawn_observations().is_empty());
    assert!(fake.destroy_calls().is_empty());
}

#[test]
fn exec_command_artifact_directory_failure_keeps_session_workspace_alive() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );
    std::fs::write(
        env.command.config().scratch_root.clone(),
        b"not a directory",
    )
    .expect("scratch root file fixture is written");

    let error = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect_err("artifact directory failure rejects exec");

    assert!(matches!(
        error,
        CommandServiceError::CommandIo { command_session_id, error }
            if command_session_id == CommandSessionId("cmd_1".to_owned())
                && error.contains("command_artifact_directory")
    ));
    assert!(launch_driver.spawn_observations().is_empty());
    assert!(fake.destroy_calls().is_empty());
}

#[test]
fn exec_command_initial_running_yield_returns_wait_loop_output() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Running("hello from wait\n".to_owned()));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );

    let output = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect("exec returns initial running yield");

    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.output, "hello from wait");
    assert_eq!(output.start_offset, 0);
    assert_eq!(output.end_offset, 1);
}

#[test]
fn exec_command_initial_completed_session_does_not_finalize_workspace() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Completed(success_exit("session done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );

    let output = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect("session command completes during initial yield");

    assert!(output.command_session_id.is_none());
    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.output, "session done");
    assert!(fake.destroy_calls().is_empty());
}

#[test]
fn write_command_stdin_waits_for_output_after_write() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Running(String::new()));
    launch_driver.push_outcome(WaitOutcome::Running("after input\n".to_owned()));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );
    let command_session_id = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect("session command exec succeeds")
        .command_session_id
        .expect("running command session id is returned");

    let output = env
        .command
        .write_command_stdin(WriteCommandStdinInput {
            command_session_id,
            stdin: "input\n".to_owned(),
            yield_time_ms: Some(250),
        })
        .expect("stdin write waits for output");

    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.output, "after input");
}

#[test]
fn write_command_stdin_finalizes_when_command_completes_after_write() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Running(String::new()));
    launch_driver.push_outcome(WaitOutcome::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );
    let command_session_id = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect("session command exec succeeds")
        .command_session_id
        .expect("running command session id is returned");

    let output = env
        .command
        .write_command_stdin(WriteCommandStdinInput {
            command_session_id: command_session_id.clone(),
            stdin: "input\n".to_owned(),
            yield_time_ms: Some(250),
        })
        .expect("stdin write finalizes completed command");

    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.output, "done");
    assert_eq!(output.command_session_id, Some(command_session_id.clone()));

    let lines = env
        .command
        .read_command_lines(ReadCommandLinesInput {
            command_session_id,
            start_offset: Some(0),
            limit: Some(10),
        })
        .expect("completed command can still be read");
    assert_eq!(lines.status, CommandStatus::Ok);
}
