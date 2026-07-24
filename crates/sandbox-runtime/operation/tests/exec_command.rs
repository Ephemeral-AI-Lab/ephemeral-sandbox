mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use sandbox_operation_contract::{OperationRequest, OperationScope};
use sandbox_runtime::command::{
    CommandServiceError, CommandStatus, ExecCommandInput, ReadCommandLinesInput,
    WriteCommandStdinInput,
};
use sandbox_runtime::workspace_session::{
    FinalizationState, HolderExitDisposition, WorkspaceSessionError,
};
use sandbox_runtime::{
    LayerStackService, NamespaceExecutionId, SandboxRuntimeOperations, WorkloadCgroupLimits,
};
use sandbox_runtime_namespace_execution::{
    NamespaceExecutionError, NsRunnerLauncher, PtyMaster, RunnerChild, RunnerPlacement,
};
use sandbox_runtime_namespace_process::runner::protocol::{Fd, NamespaceRunnerRequest};
use sandbox_runtime_workspace::{
    CapturedWorkspaceChanges, HolderFinalization, HolderFinalizationProof,
    HolderFinalizationUnknownClass, NetworkProfile, WorkspaceError, WorkspaceHandle,
    WorkspaceSessionId,
};
use serde_json::json;

use support::{
    build_services, build_services_with_launch_driver,
    build_services_with_launch_driver_and_cgroup_root,
    build_services_with_launch_driver_and_layerstack,
    build_services_with_launch_driver_and_workload_cgroup, create_request, success_exit,
    workspace_handle, workspace_handle_unavailable_launch, workspace_handle_without_launch,
    FakeLaunchDriver, FakeLauncher, FakeRunnerScript, FakeWorkspaceService, ScriptedCommandYield,
    TestServices,
};

fn exec_input(workspace_session_id: WorkspaceSessionId) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: Some(workspace_session_id),
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(0),
    }
}

fn implicit_exec_input() -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: None,
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(0),
    }
}

fn implicit_exec_input_await_completion() -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: None,
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(250),
    }
}

fn exec_input_await_completion(workspace_session_id: WorkspaceSessionId) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: Some(workspace_session_id),
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(250),
    }
}

fn create_session(
    fake: &Arc<FakeWorkspaceService>,
    env: &support::TestServices,
    workspace_session_id: &str,
    workspace_root: PathBuf,
    network: NetworkProfile,
) -> WorkspaceSessionId {
    fake.push_create_result(Ok(workspace_handle(
        workspace_session_id,
        "lease-1",
        workspace_root.clone(),
        network,
    )));
    env.workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id
}

fn empty_capture(handle: &WorkspaceHandle) -> CapturedWorkspaceChanges {
    CapturedWorkspaceChanges {
        workspace_session_id: handle.id.clone(),
        base_revision: handle.base_revision(),
        base_manifest: handle.snapshot.manifest.clone(),
        changed_path_kinds: Default::default(),
        protected_drops: Vec::new(),
        stats: None,
        metadata_path_count: 0,
        changed_paths: Vec::new(),
        changes: Vec::new(),
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
        NetworkProfile::Shared,
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
    let lines = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id: command_session_id.clone(),
        start_offset: Some(0),
        limit: Some(10),
    });
    assert_eq!(lines.command_session_id, Some(command_session_id));
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
fn command_admission_overload_is_a_structured_server_busy_fault(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver
        .launcher()
        .push_script(FakeRunnerScript::spawn_error(
            NamespaceExecutionError::Admission { max_active: 4 },
        ));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    );
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&env.command),
        Arc::clone(&env.workspace),
        layerstack_service()?,
        support::test_file_service(),
    );
    let request = OperationRequest::new(
        "exec_command",
        "req-command-overload",
        OperationScope::sandbox("sbox-test"),
        json!({
            "workspace_session_id": workspace_session_id.0,
            "cmd": "sleep 30",
            "yield_time_ms": 0
        }),
    );

    let response = sandbox_runtime::dispatch_operation(&operations, &request).into_json_value();

    assert_eq!(response["error"]["kind"], "server_busy");
    assert_eq!(response["error"]["details"]["max_active_commands"], 4);
    Ok(())
}

#[test]
fn duplicate_command_id_is_a_structured_conflict_fault(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver
        .launcher()
        .push_script(FakeRunnerScript::spawn_error(
            NamespaceExecutionError::Duplicate {
                execution_id: "namespace_execution_1".to_owned(),
            },
        ));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    );
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&env.command),
        Arc::clone(&env.workspace),
        layerstack_service()?,
        support::test_file_service(),
    );
    let request = OperationRequest::new(
        "exec_command",
        "req-command-duplicate",
        OperationScope::sandbox("sbox-test"),
        json!({
            "workspace_session_id": workspace_session_id.0,
            "cmd": "printf ok",
            "yield_time_ms": 0
        }),
    );

    let response = sandbox_runtime::dispatch_operation(&operations, &request).into_json_value();

    assert_eq!(response["error"]["kind"], "operation_conflict");
    assert_eq!(
        response["error"]["details"]["command_session_id"],
        "namespace_execution_1"
    );
    assert_eq!(
        response["error"]["details"]["conflict"],
        "duplicate_command_session_id"
    );
    Ok(())
}

#[test]
fn closed_workspace_command_error_keeps_the_structured_session_id(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let env = build_services(Arc::clone(&fake));
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&env.command),
        Arc::clone(&env.workspace),
        layerstack_service()?,
        support::test_file_service(),
    );
    let request = OperationRequest::new(
        "exec_command",
        "req-closed-workspace",
        OperationScope::sandbox("sbox-test"),
        json!({
            "workspace_session_id": "ws-closed",
            "cmd": "printf ok",
            "yield_time_ms": 0
        }),
    );

    let response = sandbox_runtime::dispatch_operation(&operations, &request).into_json_value();

    assert_eq!(response["error"]["kind"], "operation_failed");
    assert_eq!(
        response["error"]["details"]["workspace_session_id"],
        "ws-closed"
    );
    assert!(fake.create_requests().is_empty());
    assert!(fake.destroy_calls().is_empty());
    Ok(())
}

#[test]
fn exec_command_without_workspace_session_creates_and_finalizes_implicit_session_on_completion() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle(
        "implicit-session",
        "lease-1",
        PathBuf::from("/workspace/implicit"),
        NetworkProfile::Shared,
    );
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(empty_capture(&handle)));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit(
        "implicit done\n",
    )));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let output = env
        .command
        .exec_command(implicit_exec_input_await_completion())
        .expect("implicit-session command completes");

    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.output, "implicit done");
    assert!(output.command_session_id.is_none());
    assert_eq!(
        output.workspace_session_id,
        Some(WorkspaceSessionId("implicit-session".to_owned())),
        "the response names the implicit session even though it is already finalized"
    );
    assert_eq!(output.publish_rejected, None);
    assert_eq!(output.finalization_failed, None);
    assert_eq!(
        fake.create_requests(),
        vec![support::raw_create_request("implicit-session")]
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("implicit-session".to_owned())]
    );
}

#[test]
fn transient_unknown_holder_finalization_retries_before_implicit_finalize() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle(
        "implicit-probe-retry",
        "lease-probe-retry",
        PathBuf::from("/workspace/implicit-probe-retry"),
        NetworkProfile::Shared,
    );
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Ok(empty_capture(&handle)));
    fake.push_holder_finalization_result(HolderFinalization::Unknown {
        class: HolderFinalizationUnknownClass::TimedOut,
    });
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let output = env
        .command
        .exec_command(implicit_exec_input_await_completion())
        .expect("bounded probe retry lets ordinary finalization finish");

    assert_eq!(fake.holder_finalization_calls(), 2);
    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.output, "done");
    assert_eq!(output.publish_rejected, None);
    assert_eq!(output.finalization_failed, None);
    assert_eq!(
        fake.capture_calls(),
        vec![WorkspaceSessionId("implicit-probe-retry".to_owned())]
    );
    assert_eq!(
        fake.capture_after_quiesce_calls(),
        vec![WorkspaceSessionId("implicit-probe-retry".to_owned())]
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("implicit-probe-retry".to_owned())]
    );
    assert!(matches!(
        env.workspace
            .resolve_session(WorkspaceSessionId("implicit-probe-retry".to_owned())),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
}

#[test]
fn persistent_unknown_holder_finalization_is_visible_and_recovers_after_exit() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle(
        "implicit-probe-failed",
        "lease-probe-failed",
        PathBuf::from("/workspace/implicit-probe-failed"),
        NetworkProfile::Shared,
    );
    let upperdir = handle
        .entry()
        .expect("holder exposes a recovery source")
        .upperdir;
    std::fs::create_dir_all(&upperdir).expect("create recovery source");
    std::fs::write(upperdir.join("unfinished.txt"), b"preserve me\n")
        .expect("seed recovery source");
    fake.push_create_result(Ok(handle.clone()));
    for _ in 0..3 {
        fake.push_holder_finalization_result(HolderFinalization::Unknown {
            class: HolderFinalizationUnknownClass::Overloaded,
        });
    }
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&env.command),
        Arc::clone(&env.workspace),
        layerstack_service().expect("create operation layerstack"),
        support::test_file_service(),
    );
    let request = OperationRequest::new(
        "exec_command",
        "req-persistent-holder-finalization-failure",
        OperationScope::sandbox("sbox-test"),
        json!({
            "cmd": "printf ok",
            "yield_time_ms": 250
        }),
    );

    let output = sandbox_runtime::dispatch_operation(&operations, &request).into_json_value();
    let workspace_session_id = WorkspaceSessionId("implicit-probe-failed".to_owned());

    assert_eq!(fake.holder_finalization_calls(), 3);
    assert_eq!(output["status"], "ok");
    assert_eq!(output["exit_code"], 0);
    assert_eq!(output["output"], "done");
    assert!(output.get("publish_rejected").is_none());
    assert_eq!(output["finalization_failed"], true);
    assert_eq!(
        output["finalization_failure_class"],
        "holder_finalization_overloaded"
    );
    assert_eq!(output["finalization_attempts"], 3);
    assert!(fake.capture_calls().is_empty());
    assert!(fake.destroy_calls().is_empty());
    assert!(matches!(
        env.workspace
            .with_gated_session(&workspace_session_id, |_| ()),
        Err(WorkspaceSessionError::NotFound { .. })
    ));

    fake.mark_holder_exited(&handle, "signal:9");
    assert!(matches!(
        env.workspace.resolve_session(workspace_session_id.clone()),
        Err(WorkspaceSessionError::HolderExited {
            workspace_session_id: failed_id,
            reason,
            cleanup_state: FinalizationState::FinalizeFailed,
        }) if failed_id == workspace_session_id && reason == "signal:9"
    ));

    let outcomes = env.workspace.reconcile_holder_exits();
    let artifact = match outcomes.as_slice() {
        [outcome] => match &outcome.disposition {
            HolderExitDisposition::RecoveryRequired { artifact } => artifact,
            disposition => panic!("unexpected holder-exit disposition: {disposition:?}"),
        },
        outcomes => panic!("unexpected holder-exit outcomes: {outcomes:?}"),
    };
    assert!(artifact.is_dir());
    assert_eq!(
        std::fs::read(artifact.join("files/unfinished.txt"))
            .expect("recovery artifact preserves the unfinished file"),
        b"preserve me\n"
    );
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(artifact.join("manifest.json")).expect("recovery manifest is durable"),
    )
    .expect("recovery manifest is valid json");
    assert_eq!(manifest["workspace_session_id"], "implicit-probe-failed");
    assert_eq!(manifest["finalization_state"], "finalization_failed");
    assert!(fake.capture_calls().is_empty());
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);
    assert!(matches!(
        env.workspace.resolve_session(workspace_session_id),
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert!(env.workspace.reconcile_holder_exits().is_empty());
}

#[test]
fn implicit_capture_failure_preserves_recovery_artifact_and_converges_teardown() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle(
        "implicit-capture-failed",
        "lease-capture-failed",
        PathBuf::from("/workspace/implicit-capture-failed"),
        NetworkProfile::Shared,
    );
    let upperdir = handle
        .entry()
        .expect("holder exposes a recovery source")
        .upperdir;
    std::fs::create_dir_all(&upperdir).expect("create recovery source");
    std::fs::write(
        upperdir.join("unfinished.txt"),
        b"preserve capture failure\n",
    )
    .expect("seed recovery source");
    fake.push_create_result(Ok(handle.clone()));
    fake.push_capture_result(Err(WorkspaceError::Capture {
        message: "injected proof-aware capture failure".to_owned(),
    }));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let layerstack =
        support::observed_layerstack_service(sandbox_observability_telemetry::Observer::disabled());
    let recovery_root = layerstack
        .layer_stack_root()
        .parent()
        .unwrap_or(layerstack.layer_stack_root())
        .join("storage")
        .join("workspace_recovery");
    let env = build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        launch_driver,
        layerstack,
    );

    let output = env
        .command
        .exec_command(implicit_exec_input_await_completion())
        .expect("command output survives implicit capture failure");

    assert_eq!(
        output.finalization_failed,
        Some("holder_finalization_capture_failed")
    );
    assert_eq!(output.finalization_attempts, Some(1));
    assert_eq!(fake.holder_finalization_calls(), 1);
    assert_eq!(
        fake.capture_after_quiesce_calls(),
        vec![WorkspaceSessionId("implicit-capture-failed".to_owned())]
    );
    assert_eq!(
        fake.capture_calls(),
        vec![WorkspaceSessionId("implicit-capture-failed".to_owned())]
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("implicit-capture-failed".to_owned())]
    );
    let artifacts = std::fs::read_dir(recovery_root)
        .expect("capture failure creates a recovery artifact")
        .collect::<Result<Vec<_>, _>>()
        .expect("recovery artifact directory is readable");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        std::fs::read(artifacts[0].path().join("files/unfinished.txt"))
            .expect("recovery artifact preserves unfinished data"),
        b"preserve capture failure\n"
    );
}

#[test]
fn implicit_forged_quiesce_proof_is_refused_before_capture() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let handle = workspace_handle(
        "implicit-forged-proof",
        "lease-forged-proof",
        PathBuf::from("/workspace/implicit-forged-proof"),
        NetworkProfile::Shared,
    );
    let upperdir = handle
        .entry()
        .expect("holder exposes a recovery source")
        .upperdir;
    std::fs::create_dir_all(&upperdir).expect("create recovery source");
    std::fs::write(upperdir.join("unfinished.txt"), b"preserve forged proof\n")
        .expect("seed recovery source");
    fake.push_create_result(Ok(handle.clone()));
    let mut forged_identity = handle.holder_identity();
    forged_identity.generation += 1;
    fake.push_holder_finalization_result(HolderFinalization::Quiesced {
        proof: HolderFinalizationProof {
            workspace_session_id: handle.id.clone(),
            holder_identity: forged_identity,
            exit_sequence: 1,
        },
    });
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let layerstack =
        support::observed_layerstack_service(sandbox_observability_telemetry::Observer::disabled());
    let recovery_root = layerstack
        .layer_stack_root()
        .parent()
        .unwrap_or(layerstack.layer_stack_root())
        .join("storage")
        .join("workspace_recovery");
    let env = build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        launch_driver,
        layerstack,
    );

    let output = env
        .command
        .exec_command(implicit_exec_input_await_completion())
        .expect("command output survives forged proof refusal");

    assert_eq!(
        output.finalization_failed,
        Some("holder_finalization_capture_failed")
    );
    assert_eq!(output.finalization_attempts, Some(1));
    assert_eq!(fake.holder_finalization_calls(), 1);
    assert_eq!(
        fake.capture_after_quiesce_calls(),
        vec![WorkspaceSessionId("implicit-forged-proof".to_owned())]
    );
    assert!(fake.capture_calls().is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("implicit-forged-proof".to_owned())]
    );
    let artifacts = std::fs::read_dir(recovery_root)
        .expect("forged proof creates a recovery artifact")
        .collect::<Result<Vec<_>, _>>()
        .expect("recovery artifact directory is readable");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        std::fs::read(artifacts[0].path().join("files/unfinished.txt"))
            .expect("recovery artifact preserves refused capture data"),
        b"preserve forged proof\n"
    );
}

#[test]
fn exec_command_terminal_output_returns_command_session_id_when_more_output_remains() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "implicit-session",
        "lease-1",
        PathBuf::from("/workspace/implicit"),
        NetworkProfile::Shared,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let stdout = format!("{}\nkept\n", "x".repeat(1024 * 1024 + 128));
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit(&stdout)));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let output = env
        .command
        .exec_command(implicit_exec_input_await_completion())
        .expect("implicit-session command completes");

    let command_session_id = output
        .command_session_id
        .expect("truncated terminal output keeps command session id");
    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.output, "kept");

    let lines = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id,
        start_offset: None,
        limit: None,
    });
    assert_eq!(lines.output, "kept");
}

#[test]
fn exec_command_without_workspace_session_keeps_implicit_session_until_terminal_completion() {
    use sandbox_runtime_namespace_process::runner::protocol::RunResult;

    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "implicit-session",
        "lease-1",
        PathBuf::from("/workspace/implicit"),
        NetworkProfile::Shared,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    // A single spawn that parks (never completes on its own).
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let inner_launcher = launch_driver.launcher();
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let command_session_id = env
        .command
        .exec_command(implicit_exec_input())
        .expect("implicit-session command starts")
        .command_session_id
        .expect("running command session id is returned");
    assert!(fake.destroy_calls().is_empty());

    // Complete the parked child in a background thread so write_command_stdin can observe it.
    let completer = inner_launcher;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        completer.complete_latest(RunResult {
            exit_code: 0,
            payload: serde_json::json!({ "status": "ok" }),
        });
    });

    let output = env
        .command
        .write_command_stdin(WriteCommandStdinInput {
            command_session_id: command_session_id.clone(),
            stdin: "input\n".to_owned(),
            yield_time_ms: Some(250),
        })
        .expect("implicit-session command completes after stdin write");

    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.command_session_id, Some(command_session_id.clone()));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("implicit-session".to_owned())]
    );
    let lines = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id,
        start_offset: Some(0),
        limit: Some(10),
    });
    assert_eq!(lines.status, CommandStatus::Ok);
}

#[test]
fn exec_command_without_workspace_session_destroys_implicit_session_after_spawn_failure() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "implicit-session",
        "lease-1",
        PathBuf::from("/workspace/implicit"),
        NetworkProfile::Shared,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_spawn_error(CommandServiceError::CommandIo {
        command_session_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
        error: "spawn failed".to_owned(),
    });
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let error = env
        .command
        .exec_command(implicit_exec_input())
        .expect_err("implicit-session spawn failure rejects exec");

    assert!(matches!(error, CommandServiceError::CommandIo { .. }));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("implicit-session".to_owned())]
    );
}

#[test]
fn exec_command_without_workspace_session_destroys_implicit_session_after_launch_material_failure()
{
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle_without_launch(
        "implicit-session",
        "lease-1",
        PathBuf::from("/workspace/implicit"),
        NetworkProfile::Shared,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());

    let error = env
        .command
        .exec_command(implicit_exec_input())
        .expect_err("missing launch material rejects implicit-session exec");

    assert!(matches!(
        error,
        CommandServiceError::InvalidCommand { message }
            if message.contains("lacks workspace entry material")
    ));
    assert!(launch_driver.recorded_requests().is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("implicit-session".to_owned())]
    );
}

#[test]
fn exec_command_spawn_failure_keeps_session_workspace_alive() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_spawn_error(CommandServiceError::CommandIo {
        command_session_id: NamespaceExecutionId("namespace_execution_1".to_owned()),
        error: "spawn failed".to_owned(),
    });
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    );

    let error = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect_err("spawn failure rejects session exec");

    assert!(matches!(error, CommandServiceError::CommandIo { .. }));
    assert!(fake.destroy_calls().is_empty());
    assert!(
        !env.command
            .config()
            .scratch_root
            .join("workspace-session")
            .join("executions")
            .join("namespace_execution_1")
            .exists(),
        "session spawn failure should clean up unretained command artifacts"
    );
}

#[test]
fn destroy_workspace_session_waits_for_existing_session_exec_until_active_insert(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let (spawn_entered_tx, spawn_entered_rx) = mpsc::channel();
    let (release_spawn_tx, release_spawn_rx) = mpsc::channel();
    let blocking_launcher = BlockingNsLauncher::new(spawn_entered_tx, release_spawn_rx);
    let obs = sandbox_observability_telemetry::Observer::disabled();
    let workspace = Arc::new(sandbox_runtime::WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        layerstack_service()?,
        obs.clone(),
    ));
    let exec_spans = Arc::new(sandbox_observability_telemetry::SpanRegistry::new(
        obs.clone(),
    ));
    let engine = Arc::new(
        sandbox_runtime_namespace_execution::NamespaceExecutionEngine::with_launcher(
            Box::new(blocking_launcher),
            exec_spans.clone(),
            sandbox_runtime_namespace_execution::ExecutionCaps::default(),
        ),
    );
    let config = sandbox_runtime::command::CommandConfig {
        scratch_root: std::env::current_dir()
            .expect("current directory")
            .join("target")
            .join(format!(
                "blocking-launcher-test-{}-{}",
                std::process::id(),
                {
                    static N: AtomicU64 = AtomicU64::new(0);
                    N.fetch_add(1, Ordering::Relaxed)
                }
            )),
        ..sandbox_runtime::command::CommandConfig::default()
    };
    let command = Arc::new(
        sandbox_runtime::command::CommandOperationService::with_engine(
            Arc::clone(&workspace),
            config,
            engine,
            exec_spans,
            obs,
        ),
    );
    let env = TestServices {
        workspace: Arc::clone(&workspace),
        command: Arc::clone(&command),
    };
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    );
    let exec_workspace_session_id = workspace_session_id.clone();
    let exec_handle =
        std::thread::spawn(move || command.exec_command(exec_input(exec_workspace_session_id)));

    spawn_entered_rx.recv_timeout(Duration::from_secs(1))?;
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&env.command),
        Arc::clone(&env.workspace),
        layerstack_service()?,
        support::test_file_service(),
    );
    let destroy_request = OperationRequest::new(
        "destroy_workspace_session",
        "req-destroy-race",
        OperationScope::sandbox("sbox-test"),
        json!({ "workspace_session_id": workspace_session_id.0 }),
    );
    let destroy_handle = std::thread::spawn(move || {
        sandbox_runtime::dispatch_operation(&operations, &destroy_request).into_json_value()
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
        Some(NamespaceExecutionId("namespace_execution_1".to_owned()))
    );

    let destroy_response = destroy_handle
        .join()
        .map_err(|_| "destroy thread panicked")?;
    assert_eq!(destroy_response["error"]["kind"], "operation_failed");
    assert_eq!(
        destroy_response["error"]["details"]["active_command_session_ids"],
        json!(["namespace_execution_1"])
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
        NetworkProfile::Isolated,
    );
    let mut input = exec_input(workspace_session_id);
    input.timeout_ms = Some(2500);

    let output = env
        .command
        .exec_command(input)
        .expect("session command exec succeeds");

    assert_eq!(
        output.command_session_id,
        Some(NamespaceExecutionId("namespace_execution_1".to_owned()))
    );
    let requests = launch_driver.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.request_id, "namespace_execution_1");
    assert_eq!(
        request.args["command"]
            .as_str()
            .expect("command arg is a string"),
        "printf ok"
    );
    assert_eq!(request.args["cwd"], ".");
    assert_eq!(request.timeout_seconds, Some(2.5));
    assert_eq!(
        launch_driver.recorded_transcript_paths()[0]
            .clone()
            .expect("transcript path recorded"),
        env.command
            .config()
            .scratch_root
            .join("workspace-session")
            .join("executions")
            .join("namespace_execution_1")
            .join("transcript.log")
    );
    assert_eq!(&request.workspace_root, &workspace_root);
    assert_eq!(
        request.layer_paths.as_slice(),
        &[PathBuf::from("/lower/one")]
    );
    assert_eq!(
        request
            .upperdir
            .as_ref()
            .expect("upperdir present")
            .file_name()
            .and_then(|name| name.to_str()),
        Some("upper")
    );
    assert_eq!(
        request
            .workdir
            .as_ref()
            .expect("workdir present")
            .file_name()
            .and_then(|name| name.to_str()),
        Some("work")
    );
    let ns_fds = request.ns_fds.expect("ns_fds present");
    assert_eq!(ns_fds.user, Some(Fd(10)));
    assert_eq!(ns_fds.mnt, Some(Fd(11)));
    assert_eq!(ns_fds.pid, Some(Fd(12)));
    assert_eq!(ns_fds.net, Some(Fd(13)));
}

#[test]
fn exec_command_places_shared_session_child_in_workspace_cgroup() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let cgroup_root = temp_root().join("cgroup-root");
    let env = build_services_with_launch_driver_and_cgroup_root(
        Arc::clone(&fake),
        Arc::clone(&launch_driver),
        Some(cgroup_root.clone()),
    );
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    );

    env.command
        .exec_command(exec_input(workspace_session_id))
        .expect("session command exec succeeds");

    let expected_workspace_cgroup = cgroup_root.join("workspace-workspace-session");
    assert!(expected_workspace_cgroup.is_dir());
    assert_eq!(
        launch_driver.recorded_cgroup_procs_paths(),
        vec![Some(expected_workspace_cgroup.join("cgroup.procs"))]
    );
}

#[test]
fn workload_cgroup_applies_profile_limits_before_command_admission() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    let cgroup_root = temp_root().join("cgroup-root");
    let limits = WorkloadCgroupLimits {
        nano_cpus: 1_500_000_000,
        memory_high_bytes: 256 * 1024 * 1024,
        memory_max_bytes: 320 * 1024 * 1024,
        pids_max: 96,
    };
    let env = build_services_with_launch_driver_and_workload_cgroup(
        Arc::clone(&fake),
        Arc::clone(&launch_driver),
        cgroup_root.clone(),
        limits,
    );
    let workspace_session_id = create_session(
        &fake,
        &env,
        "profiled-session",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    );

    env.command
        .exec_command(exec_input(workspace_session_id))
        .expect("session command exec succeeds");

    let leaf = cgroup_root.join("workspace-profiled-session");
    assert_eq!(
        std::fs::read_to_string(leaf.join("cpu.max")).expect("read cpu.max"),
        "150000 100000"
    );
    assert_eq!(
        std::fs::read_to_string(leaf.join("memory.high")).expect("read memory.high"),
        (256 * 1024 * 1024).to_string()
    );
    assert_eq!(
        std::fs::read_to_string(leaf.join("memory.max")).expect("read memory.max"),
        (320 * 1024 * 1024).to_string()
    );
    assert_eq!(
        std::fs::read_to_string(leaf.join("pids.max")).expect("read pids.max"),
        "96"
    );
    assert_eq!(
        std::fs::read_to_string(leaf.join("memory.oom.group")).expect("read memory.oom.group"),
        "1"
    );
    assert_eq!(
        launch_driver.recorded_cgroup_procs_paths(),
        vec![Some(leaf.join("cgroup.procs"))]
    );
}

#[test]
fn exec_command_missing_launch_material_rejects_without_spawn() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_root = PathBuf::from("/workspace/session");
    fake.push_create_result(Ok(workspace_handle_without_launch(
        "workspace-session",
        "lease-1",
        workspace_root.clone(),
        NetworkProfile::Shared,
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
    assert!(launch_driver.recorded_requests().is_empty());
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
        NetworkProfile::Shared,
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
    assert!(launch_driver.recorded_requests().is_empty());
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
        NetworkProfile::Shared,
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
        CommandServiceError::CommandIo { command_session_id, .. }
            if command_session_id == NamespaceExecutionId("namespace_execution_1".to_owned())
    ));
    assert!(launch_driver.recorded_requests().is_empty());
    assert!(fake.destroy_calls().is_empty());
}

/// A custom `NsRunnerLauncher` that signals when `spawn_pty` is entered, then
/// blocks until released, before delegating to a parked `FakeLauncher` script.
/// Used to reproduce the concurrency race between exec admission and workspace destroy.
struct BlockingNsLauncher {
    spawn_entered: Mutex<Option<mpsc::Sender<()>>>,
    release_spawn: Mutex<mpsc::Receiver<()>>,
    inner: FakeLauncher,
}

impl BlockingNsLauncher {
    fn new(spawn_entered: mpsc::Sender<()>, release_spawn: mpsc::Receiver<()>) -> Self {
        let inner = FakeLauncher::new();
        // After unblocking, the launcher parks — never completes — so exec returns Running.
        inner.push_script(FakeRunnerScript::running(vec![]));
        Self {
            spawn_entered: Mutex::new(Some(spawn_entered)),
            release_spawn: Mutex::new(release_spawn),
            inner,
        }
    }
}

impl NsRunnerLauncher for BlockingNsLauncher {
    fn spawn_pty(
        &self,
        request: NamespaceRunnerRequest,
        transcript_path: Option<PathBuf>,
        cancelled: Arc<AtomicBool>,
        placement: RunnerPlacement,
    ) -> Result<(Box<dyn RunnerChild>, PtyMaster), NamespaceExecutionError> {
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
            .map_err(|error| NamespaceExecutionError::Spawn(error.to_string()))?;
        self.inner
            .spawn_pty(request, transcript_path, cancelled, placement)
    }

    fn spawn_overlay_mount(
        &self,
        request: NamespaceRunnerRequest,
        placement: RunnerPlacement,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError> {
        self.inner
            .spawn_overlay_mount(request, placement, setup_timeout_s)
    }

    fn spawn_file_op(
        &self,
        request: NamespaceRunnerRequest,
        placement: RunnerPlacement,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError> {
        self.inner
            .spawn_file_op(request, placement, setup_timeout_s)
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
        NetworkProfile::Shared,
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
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit(
        "session done\n",
    )));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    );

    let output = env
        .command
        .exec_command(exec_input_await_completion(workspace_session_id))
        .expect("session command completes during initial yield");

    assert!(output.command_session_id.is_none());
    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.output, "session done");
    assert!(fake.destroy_calls().is_empty());
}

#[test]
fn write_command_stdin_waits_for_output_after_write() {
    // In the new engine model a single spawn either parks or completes. To simulate
    // output arriving AFTER exec returns (during the write_command_stdin wait window),
    // a background thread appends a transcript row before the 250 ms deadline.
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    // Park the child — no output at spawn, no completion.
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver.clone());
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    );
    let command_session_id = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect("session command exec succeeds")
        .command_session_id
        .expect("running command session id is returned");

    // Append a transcript row shortly after exec returns, before the 250 ms yield deadline.
    let transcript_path = launch_driver
        .recorded_transcript_paths()
        .into_iter()
        .next()
        .flatten()
        .expect("transcript path was recorded");
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript_path)
        {
            // Write a JSONL transcript row that the transcript reader will produce "after input".
            let _ = writeln!(
                f,
                "{{\"offset\":0,\"stream\":\"stdout\",\"text\":\"after input\"}}"
            );
        }
    });

    let output = env
        .command
        .write_command_stdin(WriteCommandStdinInput {
            command_session_id,
            stdin: "input\n".to_owned(),
            yield_time_ms: Some(250),
        })
        .expect("stdin write waits for output");

    // The PTY echoes the written stdin bytes into the transcript; the assertion
    // verifies the command is still running and that the appended output arrived.
    assert_eq!(output.status, CommandStatus::Running);
    assert!(
        output.output.contains("after input"),
        "expected 'after input' in output, got: {:?}",
        output.output
    );
}

#[test]
fn write_command_stdin_finalizes_when_command_completes_after_write() {
    // In the new engine a single parked child is completed via FakeLauncher.complete_latest()
    // from a background thread, simulating the command finishing after stdin is written.
    use sandbox_runtime_namespace_process::runner::protocol::RunResult;

    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    // Park the child — no output at spawn, no completion.
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let inner_launcher = launch_driver.launcher();
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = create_session(
        &fake,
        &env,
        "workspace-session",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    );
    let command_session_id = env
        .command
        .exec_command(exec_input(workspace_session_id))
        .expect("session command exec succeeds")
        .command_session_id
        .expect("running command session id is returned");

    // Complete the parked child shortly after the write, within the 250 ms yield window.
    let completer = inner_launcher;
    let transcript_path_for_output = env
        .command
        .config()
        .scratch_root
        .join("workspace-session")
        .join("executions")
        .join("namespace_execution_1")
        .join("transcript.log");
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript_path_for_output)
        {
            let _ = writeln!(
                f,
                "{{\"offset\":0,\"stream\":\"stdout\",\"text\":\"done\"}}"
            );
        }
        completer.complete_latest(RunResult {
            exit_code: 0,
            payload: serde_json::json!({ "status": "ok" }),
        });
    });

    let output = env
        .command
        .write_command_stdin(WriteCommandStdinInput {
            command_session_id: command_session_id.clone(),
            stdin: "input\n".to_owned(),
            yield_time_ms: Some(250),
        })
        .expect("stdin write finalizes completed command");

    // The PTY echoes the written stdin bytes into the transcript; the assertion
    // verifies the command completed with the expected output content.
    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(output.exit_code, Some(0));
    assert!(
        output.output.contains("done"),
        "expected 'done' in output, got: {:?}",
        output.output
    );
    assert_eq!(output.command_session_id, Some(command_session_id.clone()));

    let lines = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id,
        start_offset: Some(0),
        limit: Some(10),
    });
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
    Ok(Arc::new(LayerStackService::new(
        root,
        base.join("scratch"),
        sandbox_runtime::LayerstackRuntimeConfig::default(),
        sandbox_observability_telemetry::Observer::disabled(),
        support::test_file_service(),
    )?))
}

fn temp_root() -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "sandbox-runtime-exec-command-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ))
}
