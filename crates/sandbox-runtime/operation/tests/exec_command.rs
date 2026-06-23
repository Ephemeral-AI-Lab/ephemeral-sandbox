mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use sandbox_protocol::{CliOperationScope, Request};
use sandbox_runtime::command::{
    CommandCompletionPromise, CommandCompletionWaitOutcome, CommandLaunchDriver,
    CommandServiceError, CommandSessionId, CommandStatus, ExecCommandInput, ReadCommandLinesInput,
    WriteCommandStdinInput,
};
use sandbox_runtime::{
    AsyncTraceSink, LayerStackService, NamespaceExecutionStore, NamespaceExecutionTerminalStatus,
    SandboxRuntimeOperations,
};
use sandbox_runtime_command::process::{
    CommandProcess, CommandProcessExit, CommandProcessSpawn, CommandProcessSpec,
};
use sandbox_runtime_workspace::{WorkspaceEntry, WorkspaceProfile, WorkspaceSessionId};
use serde_json::json;

use support::{
    build_services, build_services_with_launch_driver,
    build_services_with_launch_driver_and_async_trace_sink,
    build_services_with_launch_driver_namespace_store, create_request, success_exit,
    workspace_handle, workspace_handle_unavailable_launch, workspace_handle_without_launch,
    FakeLaunchDriver, FakeWorkspaceService, ScriptedCommandYield,
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

fn command_exit(status: &str, exit_code: i64, stdout: &str) -> CommandProcessExit {
    CommandProcessExit {
        status: status.to_owned(),
        exit_code,
        signal: None,
        stdout: stdout.to_owned(),
        elapsed_s: 0.1,
        kill: None,
    }
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
        .exec_command(exec_input(workspace_session_id), None)
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
        .exec_command(input, None)
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
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit(
        "one-shot done\n",
    )));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let output = env
        .command
        .exec_command(one_shot_exec_input(), None)
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
fn exec_command_without_request_trace_does_not_emit_async_finalizer_trace() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "one-shot-session",
        "lease-1",
        PathBuf::from("/workspace/one-shot"),
        WorkspaceProfile::HostCompatible,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit(
        "one-shot done\n",
    )));
    let (tx, rx) = mpsc::channel::<()>();
    let sink: AsyncTraceSink = Arc::new(move |_, _| {
        tx.send(()).expect("async trace test receiver stays open");
    });
    let env = build_services_with_launch_driver_and_async_trace_sink(
        Arc::clone(&fake),
        launch_driver,
        Some(sink),
    );

    let output = env
        .command
        .exec_command(one_shot_exec_input(), None)
        .expect("one-shot command completes");

    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.output, "one-shot done");
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("one-shot-session".to_owned())]
    );
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
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
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit(&stdout)));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let output = env
        .command
        .exec_command(one_shot_exec_input(), None)
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
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let command_session_id = env
        .command
        .exec_command(one_shot_exec_input(), None)
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
        .exec_command(one_shot_exec_input(), None)
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
        .exec_command(one_shot_exec_input(), None)
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
        .exec_command(exec_input(workspace_session_id), None)
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
fn exec_command_spawn_failure_completes_namespace_execution_error() {
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

    let _error = env
        .command
        .exec_command(exec_input(workspace_session_id.clone()), None)
        .expect_err("spawn failure rejects session exec");

    let completed = env
        .command
        .namespace_execution_store()
        .drain_completed_namespace_executions(10)
        .expect("namespace completed records drain");
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].workspace_session_id, workspace_session_id);
    assert_eq!(
        completed[0].terminal_status,
        Some(NamespaceExecutionTerminalStatus::Error)
    );
    assert_eq!(
        completed[0].error_kind.as_deref(),
        Some("command_start_failed")
    );
}

#[test]
fn namespace_store_mutation_failure_does_not_fail_command_or_drop_command_bridge_id() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let namespace_execution = Arc::new(NamespaceExecutionStore::new());
    namespace_execution.set_force_mutation_errors_for_test(true);
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let env = build_services_with_launch_driver_namespace_store(
        Arc::clone(&fake),
        launch_driver,
        Arc::clone(&namespace_execution),
    );
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );

    let output = env
        .command
        .exec_command(exec_input(workspace_session_id), None)
        .expect("command starts despite namespace store mutation failure");
    let command_session_id = output
        .command_session_id
        .expect("running command has command id");
    let namespace_execution_id = env
        .command
        .namespace_execution_id_for_command_for_test(&command_session_id)
        .expect("command record keeps allocated namespace id");

    assert_eq!(namespace_execution_id.0, "namespace_execution_1");
    let snapshot = SandboxRuntimeOperations::new(
        Arc::clone(&env.command),
        Arc::clone(&env.workspace),
        layerstack_service().expect("layerstack service"),
    )
    .observability_snapshot();
    assert!(snapshot.active_namespace_executions.is_empty());
    assert!(snapshot
        .partial_errors
        .iter()
        .any(|error| error.contains("begin_namespace_execution")));
}

#[test]
fn namespace_execution_terminal_status_maps_command_result_without_output_text() {
    for (status, exit_code, expected) in [
        ("ok", 0, NamespaceExecutionTerminalStatus::Ok),
        ("error", 2, NamespaceExecutionTerminalStatus::Error),
        ("timed_out", 124, NamespaceExecutionTerminalStatus::TimedOut),
        (
            "cancelled",
            130,
            NamespaceExecutionTerminalStatus::Cancelled,
        ),
    ] {
        let fake = Arc::new(FakeWorkspaceService::new());
        let launch_driver = Arc::new(FakeLaunchDriver::new());
        launch_driver.push_outcome(ScriptedCommandYield::Completed(command_exit(
            status,
            exit_code,
            "SECRET_OUTPUT\n",
        )));
        let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
        let workspace_session_id = create_session(
            &fake,
            &env,
            &format!("workspace-session-{status}"),
            PathBuf::from(format!("/workspace/session-{status}")),
            WorkspaceProfile::HostCompatible,
        );

        let _output = env
            .command
            .exec_command(exec_input(workspace_session_id.clone()), None)
            .expect("terminal command completes");

        let completed = env
            .command
            .namespace_execution_store()
            .drain_completed_namespace_executions(10)
            .expect("namespace completed records drain");
        assert_eq!(completed.len(), 1);
        let record = &completed[0];
        assert_eq!(record.workspace_session_id, workspace_session_id);
        assert_eq!(record.operation_name, "exec_command");
        assert_eq!(record.request_id, None);
        assert_eq!(record.terminal_status, Some(expected));
        assert_eq!(record.exit_code, Some(exit_code));
        assert!(
            !format!("{record:?}").contains("SECRET_OUTPUT"),
            "namespace execution record must not carry command output"
        );
    }
}

#[test]
fn namespace_execution_request_id_comes_from_runtime_request_not_runner_request(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&env.command),
        Arc::clone(&env.workspace),
        layerstack_service()?,
    );
    let request = Request::new(
        "exec_command",
        "req-external",
        CliOperationScope::system(),
        json!({
            "workspace_session_id": workspace_session_id.0,
            "cmd": "printf ok",
            "yield_time_ms": 0,
        }),
    );

    let response =
        sandbox_runtime::dispatch_operation(&operations, &request, None).into_json_value();
    assert_eq!(response["status"], "ok");
    let completed = env
        .command
        .namespace_execution_store()
        .drain_completed_namespace_executions(10)
        .expect("namespace completed records drain");
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].request_id.as_deref(), Some("req-external"));
    assert_ne!(completed[0].request_id.as_deref(), Some("cmd_1"));
    Ok(())
}

#[test]
fn destroy_workspace_session_waits_for_existing_session_exec_until_active_insert(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let (spawn_entered_tx, spawn_entered_rx) = mpsc::channel();
    let (release_spawn_tx, release_spawn_rx) = mpsc::channel();
    let launch_driver = Arc::new(BlockingLaunchDriver::new(
        spawn_entered_tx,
        release_spawn_rx,
    ));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    );
    let command = Arc::clone(&env.command);
    let exec_workspace_session_id = workspace_session_id.clone();
    let exec_handle = std::thread::spawn(move || {
        command.exec_command(exec_input(exec_workspace_session_id), None)
    });

    spawn_entered_rx.recv_timeout(Duration::from_secs(1))?;
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&env.command),
        Arc::clone(&env.workspace),
        layerstack_service()?,
    );
    let destroy_request = Request::new(
        "destroy_workspace_session",
        "req-destroy-race",
        CliOperationScope::system(),
        json!({ "workspace_session_id": workspace_session_id.0 }),
    );
    let destroy_handle = std::thread::spawn(move || {
        sandbox_runtime::dispatch_operation(&operations, &destroy_request, None).into_json_value()
    });

    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !destroy_handle.is_finished(),
        "destroy should wait while existing-session exec holds lifecycle admission"
    );
    assert!(fake.destroy_calls().is_empty());

    release_spawn_tx.send(())?;
    let exec_output = exec_handle
        .join()
        .map_err(|_| "exec thread panicked")?
        .expect("exec command succeeds");
    assert_eq!(
        exec_output.command_session_id,
        Some(CommandSessionId("cmd_1".to_owned()))
    );

    let destroy_response = destroy_handle
        .join()
        .map_err(|_| "destroy thread panicked")?;
    assert_eq!(destroy_response["error"]["kind"], "operation_failed");
    assert_eq!(
        destroy_response["error"]["details"]["active_command_session_ids"],
        json!(["cmd_1"])
    );
    assert!(fake.destroy_calls().is_empty());
    Ok(())
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
        .exec_command(input, None)
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
        .exec_command(exec_input(workspace_session_id), None)
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
        .exec_command(exec_input(workspace_session_id), None)
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
        .exec_command(exec_input(workspace_session_id), None)
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

struct BlockingLaunchDriver {
    spawn_entered: Mutex<Option<mpsc::Sender<()>>>,
    release_spawn: Mutex<mpsc::Receiver<()>>,
}

impl BlockingLaunchDriver {
    fn new(spawn_entered: mpsc::Sender<()>, release_spawn: mpsc::Receiver<()>) -> Self {
        Self {
            spawn_entered: Mutex::new(Some(spawn_entered)),
            release_spawn: Mutex::new(release_spawn),
        }
    }
}

impl CommandLaunchDriver for BlockingLaunchDriver {
    fn spawn(
        &self,
        spec: CommandProcessSpec,
        workspace_entry: WorkspaceEntry,
        config: &sandbox_runtime_command::CommandConfig,
    ) -> Result<CommandProcess, CommandServiceError> {
        if let Some(sender) = self
            .spawn_entered
            .lock()
            .expect("test operation succeeds")
            .take()
        {
            let _ = sender.send(());
        }
        self.release_spawn
            .lock()
            .expect("test operation succeeds")
            .recv()
            .map_err(|error| CommandServiceError::CommandIo {
                command_session_id: CommandSessionId(spec.id.clone()),
                error: error.to_string(),
            })?;
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

    fn start_completion_watcher(
        &self,
        _completion: CommandCompletionPromise,
        _process: Arc<CommandProcess>,
    ) {
    }

    fn wait_for_command_yield(
        &self,
        _process: &CommandProcess,
        _completion: &CommandCompletionPromise,
        _yield_time_ms: u64,
        _start_offset: u64,
    ) -> CommandCompletionWaitOutcome {
        CommandCompletionWaitOutcome::Running
    }
}

#[test]
fn exec_command_initial_running_yield_returns_pending_output() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(
        "hello from wait\n".to_owned(),
    ));
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
        .exec_command(exec_input(workspace_session_id), None)
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
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit(
        "session done\n",
    )));
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
        .exec_command(exec_input(workspace_session_id), None)
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
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    launch_driver.push_outcome(ScriptedCommandYield::Running("after input\n".to_owned()));
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
        .exec_command(exec_input(workspace_session_id), None)
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
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
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
        .exec_command(exec_input(workspace_session_id), None)
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

fn layerstack_service() -> Result<Arc<LayerStackService>, Box<dyn std::error::Error + Send + Sync>>
{
    let base = temp_root();
    let root = base.join("layer-stack");
    let workspace = base.join("workspace");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&workspace)?;
    sandbox_runtime_layerstack::build_workspace_base(&root, &workspace, false)?;
    Ok(Arc::new(LayerStackService::new(root)?))
}

fn temp_root() -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "sandbox-runtime-exec-command-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ))
}
