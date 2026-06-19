mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use command::yield_wait_loop::WaitOutcome;
use operation_service::command::{
    CommandCallContext, CommandFinalizationOutcome, CommandFinalizedPolicy, CommandId,
    CommandServiceError, CommandStatus, ExecCommandInput, OperationTraceContext, PollCommandInput,
};
use workspace::{
    CallerId, NetworkMode, WorkspaceId, WorkspaceLaunchContext, WorkspaceLaunchNamespaceFds,
};

use support::{
    assert_private_create_request, build_services, build_services_with_launch_driver,
    create_request, success_exit, workspace_handle, workspace_handle_with_launch, FakeLaunchDriver,
    FakeWorkspaceService,
};

fn exec_input(
    caller_id: &str,
    workspace_root: PathBuf,
    workspace_id: Option<WorkspaceId>,
) -> ExecCommandInput {
    ExecCommandInput {
        caller_id: CallerId(caller_id.to_owned()),
        workspace_root,
        workspace_id,
        cmd: "printf ok".to_owned(),
        cwd: None,
        timeout_seconds: None,
        yield_time_ms: Some(0),
    }
}

#[test]
fn command_exec_some_uses_resolved_session_without_workspace_create_or_destroy() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
    )));
    let env = build_services(Arc::clone(&fake));
    let handler = env
        .workspace
        .create(create_request("caller-1", workspace_root.clone()))
        .expect("session create succeeds");
    let create_count_before_exec = fake.create_requests().len();

    let output = env
        .command
        .exec_command(
            exec_input(
                "caller-1",
                workspace_root,
                Some(handler.workspace_id.clone()),
            ),
            context("caller-1"),
        )
        .expect("session command exec succeeds");

    let command_id = output.command_id.expect("running command id is returned");
    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(fake.create_requests().len(), create_count_before_exec);
    assert!(fake.destroy_calls().is_empty());
    let poll = env
        .command
        .poll(
            PollCommandInput {
                command_id: command_id.clone(),
                last_n_lines: Some(10),
            },
            context("caller-1"),
        )
        .expect("owner can poll session command");
    assert_eq!(poll.command_id, command_id);
    assert_eq!(poll.status, CommandStatus::Running);
}

#[test]
fn command_exec_none_creates_private_host_workspace_and_binds_it() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/one-shot");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-one-shot",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
    )));
    let env = build_services(Arc::clone(&fake));

    let output = env
        .command
        .exec_command(
            exec_input("caller-1", workspace_root.clone(), None),
            context("caller-1"),
        )
        .expect("one-shot command exec succeeds");

    let command_id = output.command_id.expect("running command id is returned");
    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.exit_code, None);
    let create_requests = fake.create_requests();
    assert_eq!(create_requests.len(), 1);
    assert_private_create_request(
        &create_requests[0],
        "caller-1",
        &workspace_root,
        NetworkMode::Host,
    );
    assert!(fake.destroy_calls().is_empty());
    let poll = env
        .command
        .poll(
            PollCommandInput {
                command_id: command_id.clone(),
                last_n_lines: Some(10),
            },
            context("caller-1"),
        )
        .expect("owner can poll one-shot command");
    assert_eq!(poll.command_id, command_id);
    assert_eq!(poll.status, CommandStatus::Running);
}

#[test]
fn command_exec_rejects_context_caller_mismatch_before_workspace_create() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/one-shot");
    let env = build_services(Arc::clone(&fake));

    let error = env
        .command
        .exec_command(
            exec_input("caller-1", workspace_root.clone(), None),
            context("caller-2"),
        )
        .expect_err("caller mismatch rejects before one-shot create");

    assert!(matches!(
        error,
        CommandServiceError::InvalidCommand { message }
            if message.contains("exec caller must match command call context")
    ));
    assert!(fake.create_requests().is_empty());

    fake.push_create_result(Ok(workspace_handle(
        "workspace-one-shot",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
    )));
    let output = env
        .command
        .exec_command(
            exec_input("caller-1", workspace_root, None),
            context("caller-1"),
        )
        .expect("subsequent valid exec succeeds");
    assert_eq!(output.command_id, Some(CommandId("cmd_1".to_owned())));
}

#[test]
fn command_exec_spawn_failure_destroys_created_one_shot_workspace() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/one-shot");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-one-shot",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_spawn_error(CommandServiceError::CommandIo {
        command_id: CommandId("cmd_1".to_owned()),
        error: "spawn failed".to_owned(),
    });
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let error = env
        .services
        .exec_command(
            exec_input("caller-1", workspace_root, None),
            OperationTraceContext,
        )
        .expect_err("spawn failure rejects exec");

    match error {
        CommandServiceError::CommandIo { command_id, error } => {
            assert_eq!(command_id, CommandId("cmd_1".to_owned()));
            assert_eq!(error, "spawn failed");
        }
        other => panic!("expected command io error, got {other:?}"),
    }
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceId("workspace-one-shot".to_owned())]
    );
    assert!(
        !env.command.config().scratch_root.join("cmd_1").exists(),
        "spawn failure should clean up unretained command artifacts"
    );
}

#[test]
fn command_exec_spawn_failure_keeps_session_workspace_alive() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_spawn_error(CommandServiceError::CommandIo {
        command_id: CommandId("cmd_1".to_owned()),
        error: "spawn failed".to_owned(),
    });
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let handler = env
        .workspace
        .create(create_request("caller-1", workspace_root.clone()))
        .expect("session create succeeds");

    let error = env
        .services
        .exec_command(
            exec_input(
                "caller-1",
                workspace_root,
                Some(handler.workspace_id.clone()),
            ),
            OperationTraceContext,
        )
        .expect_err("spawn failure rejects session exec");

    assert!(matches!(
        error,
        CommandServiceError::CommandIo { command_id, error }
            if command_id == CommandId("cmd_1".to_owned()) && error == "spawn failed"
    ));
    assert!(fake.destroy_calls().is_empty());
    assert!(
        !env.command.config().scratch_root.join("cmd_1").exists(),
        "session spawn failure should clean up unretained command artifacts"
    );
}

#[test]
fn command_exec_passes_launch_material_to_runner_request_and_spawn_paths() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/one-shot");
    let launch = WorkspaceLaunchContext {
        upperdir: PathBuf::from("/upper/custom"),
        workdir: PathBuf::from("/work/custom"),
        namespace_fds: Some(WorkspaceLaunchNamespaceFds {
            user: Some(10),
            mnt: Some(11),
            pid: Some(12),
            net: Some(13),
        }),
        cgroup_path: Some(PathBuf::from("/sys/fs/cgroup/eos")),
    };
    fake.push_create_result(Ok(workspace_handle_with_launch(
        "workspace-one-shot",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Isolated,
        Some(launch),
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    let mut input = exec_input("caller-1", workspace_root.clone(), None);
    input.cwd = Some(PathBuf::from("/workspace/one-shot/src"));
    input.timeout_seconds = Some(2.5);

    let output = env
        .services
        .exec_command(input, OperationTraceContext)
        .expect("one-shot command exec succeeds");

    assert_eq!(output.command_id, Some(CommandId("cmd_1".to_owned())));
    let observations = launch_driver.spawn_observations();
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(observation.spec_id, "cmd_1");
    assert_eq!(observation.spec_caller_id, "caller-1");
    assert_eq!(
        observation.request_path,
        env.command
            .config()
            .scratch_root
            .join("cmd_1")
            .join("runner-request.json")
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
    assert_eq!(
        observation.transcript_timestamp_timezone,
        env.command.config().transcript_timestamp_timezone
    );
    assert_eq!(
        observation.output_drain_grace_ms,
        env.command.config().output_drain_grace_ms
    );

    let request = &observation.run_request;
    assert_eq!(request["mode"], "set_ns");
    assert_eq!(request["tool_call"]["invocation_id"], "cmd_1");
    assert_eq!(request["tool_call"]["caller_id"], "caller-1");
    assert_eq!(request["tool_call"]["verb"], "exec_command");
    assert_eq!(request["tool_call"]["background"], false);
    assert_eq!(request["tool_call"]["args"]["command"], "printf ok");
    assert_eq!(
        request["tool_call"]["args"]["cwd"],
        "/workspace/one-shot/src"
    );
    assert_eq!(
        request["workspace_root"].as_str(),
        Some(workspace_root.to_string_lossy().as_ref())
    );
    assert_eq!(request["layer_paths"][0], "/lower/one");
    assert_eq!(request["upperdir"], "/upper/custom");
    assert_eq!(request["workdir"], "/work/custom");
    assert_eq!(request["ns_fds"]["user"], 10);
    assert_eq!(request["ns_fds"]["mnt"], 11);
    assert_eq!(request["ns_fds"]["pid"], 12);
    assert_eq!(request["ns_fds"]["net"], 13);
    assert_eq!(request["cgroup_path"], "/sys/fs/cgroup/eos");
    assert_eq!(request["timeout_seconds"], 2.5);
}

#[test]
fn command_exec_host_compatible_launch_uses_setns_without_net_fd() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/one-shot");
    let launch = WorkspaceLaunchContext {
        upperdir: PathBuf::from("/upper/host"),
        workdir: PathBuf::from("/work/host"),
        namespace_fds: Some(WorkspaceLaunchNamespaceFds {
            user: Some(20),
            mnt: Some(21),
            pid: Some(22),
            net: None,
        }),
        cgroup_path: Some(PathBuf::from("/sys/fs/cgroup/eos-host")),
    };
    fake.push_create_result(Ok(workspace_handle_with_launch(
        "workspace-one-shot",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
        Some(launch),
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());

    let output = env
        .services
        .exec_command(
            exec_input("caller-1", workspace_root, None),
            OperationTraceContext,
        )
        .expect("host-compatible one-shot command exec succeeds");

    assert_eq!(output.command_id, Some(CommandId("cmd_1".to_owned())));
    let observations = launch_driver.spawn_observations();
    assert_eq!(observations.len(), 1);
    let request = &observations[0].run_request;
    assert_eq!(request["mode"], "set_ns");
    assert_eq!(request["ns_fds"]["user"], 20);
    assert_eq!(request["ns_fds"]["mnt"], 21);
    assert_eq!(request["ns_fds"]["pid"], 22);
    assert!(request["ns_fds"]["net"].is_null());
    assert_eq!(request["cgroup_path"], "/sys/fs/cgroup/eos-host");
}

#[test]
fn command_exec_missing_launch_material_destroys_one_shot_without_spawn() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/one-shot");
    fake.push_create_result(Ok(workspace_handle_with_launch(
        "workspace-one-shot",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
        None,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());

    let error = env
        .services
        .exec_command(
            exec_input("caller-1", workspace_root, None),
            OperationTraceContext,
        )
        .expect_err("missing launch material rejects exec");

    assert!(matches!(
        error,
        CommandServiceError::InvalidCommand { message }
            if message.contains("lacks command launch material")
    ));
    assert!(launch_driver.spawn_observations().is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceId("workspace-one-shot".to_owned())]
    );
    assert!(
        !env.command.config().scratch_root.join("cmd_1").exists(),
        "missing launch material should not leave command artifacts"
    );
}

#[test]
fn command_exec_missing_namespace_fds_destroys_one_shot_without_spawn() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/one-shot");
    let launch = WorkspaceLaunchContext {
        upperdir: PathBuf::from("/upper/missing-fds"),
        workdir: PathBuf::from("/work/missing-fds"),
        namespace_fds: None,
        cgroup_path: None,
    };
    fake.push_create_result(Ok(workspace_handle_with_launch(
        "workspace-one-shot",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
        Some(launch),
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());

    let error = env
        .services
        .exec_command(
            exec_input("caller-1", workspace_root, None),
            OperationTraceContext,
        )
        .expect_err("missing namespace fds reject holder-backed workspace exec");

    assert!(matches!(
        error,
        CommandServiceError::InvalidCommand { message }
            if message.contains("requires namespace FDs")
    ));
    assert!(launch_driver.spawn_observations().is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceId("workspace-one-shot".to_owned())]
    );
    assert!(
        !env.command.config().scratch_root.join("cmd_1").exists(),
        "missing namespace fds should not leave command artifacts"
    );
}

#[test]
fn command_exec_partial_namespace_fds_destroy_one_shot_without_spawn() {
    for (network, namespace_fds, missing_name) in [
        (
            NetworkMode::Host,
            WorkspaceLaunchNamespaceFds {
                user: Some(10),
                mnt: None,
                pid: Some(12),
                net: None,
            },
            "mnt",
        ),
        (
            NetworkMode::Isolated,
            WorkspaceLaunchNamespaceFds {
                user: Some(10),
                mnt: Some(11),
                pid: Some(12),
                net: None,
            },
            "net",
        ),
    ] {
        let fake = Arc::new(FakeWorkspaceService::new());
        let workspace_root = PathBuf::from(format!("/workspace/{network:?}"));
        let launch = WorkspaceLaunchContext {
            upperdir: PathBuf::from("/upper/partial-fds"),
            workdir: PathBuf::from("/work/partial-fds"),
            namespace_fds: Some(namespace_fds),
            cgroup_path: None,
        };
        fake.push_create_result(Ok(workspace_handle_with_launch(
            "workspace-one-shot",
            "caller-1",
            "lease-1",
            workspace_root.clone(),
            network,
            Some(launch),
        )));
        let launch_driver = Arc::new(FakeLaunchDriver::new());
        let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());

        let error = env
            .services
            .exec_command(
                exec_input("caller-1", workspace_root, None),
                OperationTraceContext,
            )
            .expect_err("partial namespace fds reject holder-backed workspace exec");

        assert!(matches!(
            error,
            CommandServiceError::InvalidCommand { message }
                if message.contains("requires namespace FDs") && message.contains(missing_name)
        ));
        assert!(launch_driver.spawn_observations().is_empty());
        assert_eq!(
            fake.destroy_calls(),
            vec![WorkspaceId("workspace-one-shot".to_owned())]
        );
    }
}

#[test]
fn command_exec_artifact_directory_failure_destroys_one_shot_without_spawn() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/one-shot");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-one-shot",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    std::fs::write(
        env.command.config().scratch_root.clone(),
        b"not a directory",
    )
    .expect("scratch root file fixture is written");

    let error = env
        .services
        .exec_command(
            exec_input("caller-1", workspace_root, None),
            OperationTraceContext,
        )
        .expect_err("artifact directory failure rejects exec");

    assert!(matches!(
        error,
        CommandServiceError::CommandIo { command_id, error }
            if command_id == CommandId("cmd_1".to_owned())
                && error.contains("prepare command artifact directory")
    ));
    assert!(launch_driver.spawn_observations().is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceId("workspace-one-shot".to_owned())]
    );
}

#[test]
fn command_exec_initial_running_yield_returns_wait_loop_output() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/one-shot");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-one-shot",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Running("hello from wait\n".to_owned()));
    let env = build_services_with_launch_driver(fake, launch_driver);

    let output = env
        .services
        .exec_command(
            exec_input("caller-1", workspace_root, None),
            OperationTraceContext,
        )
        .expect("exec returns initial running yield");

    assert_eq!(output.status, CommandStatus::Running);
    assert_eq!(output.output.stdout, "hello from wait\n");
}

#[test]
fn command_exec_initial_completed_session_returns_finalized_metadata() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "caller-1",
        "lease-1",
        workspace_root.clone(),
        NetworkMode::Host,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Completed(success_exit("session done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let handler = env
        .workspace
        .create(create_request("caller-1", workspace_root.clone()))
        .expect("session create succeeds");

    let output = env
        .services
        .exec_command(
            exec_input(
                "caller-1",
                workspace_root,
                Some(handler.workspace_id.clone()),
            ),
            OperationTraceContext,
        )
        .expect("session command completes during initial yield");

    let command_id = output.command_id.expect("command id is returned");
    assert_eq!(output.status, CommandStatus::Completed);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.output.stdout, "session done\n");
    let finalized = output.finalized.expect("session metadata is returned");
    assert_eq!(finalized.policy, CommandFinalizedPolicy::Session);
    assert_eq!(
        finalized.outcome,
        CommandFinalizationOutcome::SessionComplete
    );
    assert!(fake.destroy_calls().is_empty());

    let poll = env
        .command
        .poll(
            PollCommandInput {
                command_id: command_id.clone(),
                last_n_lines: None,
            },
            context("caller-1"),
        )
        .expect("owner can poll completed session command");
    assert_eq!(poll.command_id, command_id);
    assert_eq!(poll.status, CommandStatus::Completed);
}

#[test]
fn command_exec_rejects_workspace_root_mismatch_before_command_allocation() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "caller-1",
        "lease-1",
        PathBuf::from("/workspace/session"),
        NetworkMode::Host,
    )));
    let env = build_services(Arc::clone(&fake));
    let handler = env
        .workspace
        .create(create_request(
            "caller-1",
            PathBuf::from("/workspace/session"),
        ))
        .expect("session create succeeds");

    let error = env
        .command
        .exec_command(
            exec_input(
                "caller-1",
                PathBuf::from("/workspace/other"),
                Some(handler.workspace_id),
            ),
            context("caller-1"),
        )
        .expect_err("root mismatch is rejected");

    match error {
        CommandServiceError::WorkspaceRootMismatch { expected, actual } => {
            assert_eq!(expected.as_path(), Path::new("/workspace/session"));
            assert_eq!(actual.as_path(), Path::new("/workspace/other"));
        }
        other => panic!("expected workspace root mismatch, got {other:?}"),
    }
    let output = env
        .command
        .exec_command(
            exec_input(
                "caller-1",
                PathBuf::from("/workspace/session"),
                Some(WorkspaceId("workspace-session".to_owned())),
            ),
            context("caller-1"),
        )
        .expect("subsequent valid exec succeeds");
    assert_eq!(output.command_id, Some(CommandId("cmd_1".to_owned())));
}

fn context(caller_id: &str) -> CommandCallContext {
    CommandCallContext {
        caller_id: CallerId(caller_id.to_owned()),
        trace: OperationTraceContext,
    }
}
