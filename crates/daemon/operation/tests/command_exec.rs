mod support;

use std::path::PathBuf;
use std::sync::Arc;

use command::yield_wait_loop::WaitOutcome;
use daemon_operation::command::{
    CommandServiceError, CommandSessionId, CommandStatus, ExecCommandInput, PollCommandInput,
};
use workspace::{WorkspaceProfile, WorkspaceSessionId};

use support::{
    build_services, build_services_with_launch_driver, create_request, success_exit,
    workspace_handle, workspace_handle_unavailable_launch, workspace_handle_without_launch,
    FakeLaunchDriver, FakeWorkspaceService,
};

fn exec_input(workspace_session_id: WorkspaceSessionId) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id,
        cmd: "printf ok".to_owned(),
        timeout_seconds: None,
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
        .create_workspace_session(create_request(workspace_root))
        .expect("session create succeeds")
        .workspace_session_id
}

#[test]
fn command_exec_uses_resolved_session_without_workspace_create_or_destroy() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let env = build_services(Arc::clone(&fake));
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::SharedNetwork,
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
    let poll = env
        .command
        .poll(PollCommandInput {
            command_session_id: command_session_id.clone(),
            last_n_lines: Some(10),
        })
        .expect("session command can be polled");
    assert_eq!(poll.command_session_id, command_session_id);
    assert_eq!(poll.status, CommandStatus::Running);
}

#[test]
fn command_exec_rejects_empty_command_before_workspace_resolution() {
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
fn command_exec_spawn_failure_keeps_session_workspace_alive() {
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
        WorkspaceProfile::SharedNetwork,
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
fn command_exec_passes_workspace_entry_to_spawn_paths() {
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
    input.timeout_seconds = Some(2.5);

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
        observation.request_path,
        env.command
            .config()
            .scratch_root
            .join("cmd_1")
            .join("command-request.json")
    );
    assert_eq!(
        observation.output_path,
        env.command
            .config()
            .scratch_root
            .join("cmd_1")
            .join("runner-result.json")
    );
    assert_eq!(
        observation.final_path,
        env.command
            .config()
            .scratch_root
            .join("cmd_1")
            .join("final.json")
    );
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
fn command_exec_missing_launch_material_rejects_without_spawn() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle_without_launch(
        "workspace-session",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::SharedNetwork,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request(workspace_root))
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
fn command_exec_unavailable_workspace_launch_rejects_without_spawn() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle_unavailable_launch(
        "workspace-session",
        "lease-1",
        workspace_root.clone(),
        WorkspaceProfile::SharedNetwork,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request(workspace_root))
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
fn command_exec_artifact_directory_failure_keeps_session_workspace_alive() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::SharedNetwork,
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
fn command_exec_initial_running_yield_returns_wait_loop_output() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Running("hello from wait\n".to_owned()));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::SharedNetwork,
    );

    let output = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect("exec returns initial running yield");

    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.output.stdout, "hello from wait\n");
}

#[test]
fn command_exec_initial_completed_session_returns_finalized_metadata() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Completed(success_exit("session done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::SharedNetwork,
    );

    let output = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect("session command completes during initial yield");

    let command_session_id = output
        .command_session_id
        .expect("command session id is returned");
    assert_eq!(output.status, CommandStatus::Completed);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.output.stdout, "session done\n");
    assert!(output.finalized.is_some());
    assert!(fake.destroy_calls().is_empty());

    let poll = env
        .command
        .poll(PollCommandInput {
            command_session_id: command_session_id.clone(),
            last_n_lines: None,
        })
        .expect("completed session command can be polled");
    assert_eq!(poll.command_session_id, command_session_id);
    assert_eq!(poll.status, CommandStatus::Completed);
}
