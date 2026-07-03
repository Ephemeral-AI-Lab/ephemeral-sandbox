use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use sandbox_observability::Observer;
use sandbox_protocol::{CliOperationScope, Request};
use sandbox_runtime::command::ExecCommandInput;
use sandbox_runtime::workspace_session::{
    FinalizePolicy, WorkspaceSessionError, WorkspaceSessionService,
};
use sandbox_runtime::{CommandOperationService, LayerStackService, SandboxRuntimeOperations};
use sandbox_runtime_namespace_process::runner::protocol::RunResult;
use sandbox_runtime_workspace::{
    DestroyWorkspaceRequest, FileRunnerOp, NetworkProfile, WorkspaceError, WorkspaceHandle,
    WorkspaceSessionId,
};
use serde_json::json;

mod support;
use support::{FakeLaunchDriver, FakeWorkspaceService, ScriptedCommandYield};

fn manager_with(fake: &Arc<FakeWorkspaceService>) -> WorkspaceSessionService {
    WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(fake)),
        support::observed_layerstack_service(Observer::disabled()),
        Observer::disabled(),
    )
}

fn create_request() -> sandbox_runtime::workspace_session::CreateSessionRequest {
    support::create_request()
}

fn workspace_handle(workspace_session_id: &str, lease_id: &str) -> WorkspaceHandle {
    workspace_handle_with_profile(workspace_session_id, lease_id, NetworkProfile::Shared)
}

fn workspace_handle_with_profile(
    workspace_session_id: &str,
    lease_id: &str,
    network: NetworkProfile,
) -> WorkspaceHandle {
    support::workspace_handle(
        workspace_session_id,
        lease_id,
        PathBuf::from("/workspace"),
        network,
    )
}

fn exec_input(workspace_session_id: Option<WorkspaceSessionId>, yield_ms: u64) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id,
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(yield_ms),
    }
}

fn ok_run_result() -> RunResult {
    RunResult {
        exit_code: 0,
        payload: json!({ "status": "ok" }),
    }
}

fn wait_until(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let stop_at = Instant::now() + deadline;
    while Instant::now() < stop_at {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

#[test]
fn workspace_session_resolve_returns_session_by_id() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let manager = manager_with(&fake);

    manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");

    let handler = manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .expect("test operation succeeds");
    assert_eq!(
        handler.workspace_session_id,
        WorkspaceSessionId("workspace-1".to_owned())
    );
}

#[test]
fn workspace_session_create_rolls_back_raw_workspace_when_insert_fails() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-2")));
    let manager = manager_with(&fake);

    manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");
    let error = manager
        .create_workspace_session(create_request())
        .expect_err("test operation fails");

    assert!(matches!(
        error,
        WorkspaceSessionError::DuplicateWorkspaceSessionId { workspace_session_id }
            if workspace_session_id == WorkspaceSessionId("workspace-1".to_owned())
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
    assert!(manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .is_ok());
}

#[test]
fn workspace_session_destroy_failure_retains_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "destroy failed".to_owned(),
    }));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");

    let error = manager
        .destroy_session(handler, DestroyWorkspaceRequest::default())
        .expect_err("test operation fails");

    assert!(matches!(
        error,
        WorkspaceSessionError::Workspace(WorkspaceError::Setup { .. })
    ));
    assert!(manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .is_ok());
}

#[test]
fn workspace_session_successful_destroy_removes_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");

    manager
        .destroy_session(handler, DestroyWorkspaceRequest::default())
        .expect("test operation succeeds");

    let missing = manager
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .expect_err("test operation fails");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
}

#[test]
fn workspace_session_duplicate_destroy_does_not_call_raw_destroy_twice() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let manager = manager_with(&fake);
    let handler = manager
        .create_workspace_session(create_request())
        .expect("test operation succeeds");

    manager
        .destroy_session(handler.clone(), DestroyWorkspaceRequest::default())
        .expect("test operation succeeds");
    let duplicate = manager
        .destroy_session(handler, DestroyWorkspaceRequest::default())
        .expect_err("test operation fails");

    assert!(matches!(duplicate, WorkspaceSessionError::NotFound { .. }));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
}

// ---------------------------------------------------------------------------
// Finalize-policy matrix (§5): the completion edge is the only trigger.
// ---------------------------------------------------------------------------

#[test]
fn no_op_session_survives_command_completion() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "done\n",
    )));
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    fake.push_create_result(Ok(workspace_handle("ws-noop", "lease-1")));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;

    let output = env
        .command
        .exec_command(exec_input(Some(workspace_session_id.clone()), 250))
        .expect("session command completes");

    assert_eq!(
        output.workspace_session_id,
        Some(workspace_session_id.clone())
    );
    assert!(fake.destroy_calls().is_empty());
    assert!(fake.capture_calls().is_empty());
    assert!(env.workspace.resolve_session(workspace_session_id).is_ok());
}

#[test]
fn publish_then_destroy_session_finalizes_when_last_command_completes() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "done\n",
    )));
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    fake.push_create_result(Ok(workspace_handle("ws-ptd", "lease-1")));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(support::create_request_with_policy(
            FinalizePolicy::PublishThenDestroy,
        ))
        .expect("session create succeeds")
        .workspace_session_id;

    let output = env
        .command
        .exec_command(exec_input(Some(workspace_session_id.clone()), 250))
        .expect("session command completes");

    assert_eq!(
        output.workspace_session_id,
        Some(workspace_session_id.clone())
    );
    assert!(
        wait_until(Duration::from_secs(5), || !fake.destroy_calls().is_empty()),
        "publish_then_destroy finalizes once the ledger drains"
    );
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);
    assert_eq!(fake.capture_calls(), vec![workspace_session_id.clone()]);
    let missing = env
        .workspace
        .resolve_session(workspace_session_id)
        .expect_err("finalized session is gone");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn rider_command_defers_finalization_until_last_completion() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-rider", "lease-1")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let launcher = launch_driver.launcher();
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let first = env
        .command
        .exec_command(exec_input(None, 0))
        .expect("implicit session command starts");
    let workspace_session_id = first
        .workspace_session_id
        .clone()
        .expect("exec_command returns the session id");
    let rider = env
        .command
        .exec_command(exec_input(Some(workspace_session_id.clone()), 0))
        .expect("rider attaches to the running session");
    assert_eq!(
        rider.workspace_session_id,
        Some(workspace_session_id.clone())
    );

    let rider_id = rider.command_session_id.expect("rider is running");
    launcher.complete_request(&rider_id.0, ok_run_result());
    assert!(
        !wait_until(Duration::from_millis(300), || {
            !fake.destroy_calls().is_empty()
        }),
        "rider completion must not finalize while the first command runs"
    );
    assert!(env
        .workspace
        .resolve_session(workspace_session_id.clone())
        .is_ok());

    let first_id = first.command_session_id.expect("first command is running");
    launcher.complete_request(&first_id.0, ok_run_result());
    assert!(
        wait_until(Duration::from_secs(5), || !fake.destroy_calls().is_empty()),
        "last completion drains the ledger and finalizes"
    );
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);
    let missing = env
        .workspace
        .resolve_session(workspace_session_id)
        .expect_err("finalized session is gone");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
}

#[test]
fn sweep_remount_does_not_finalize_idle_publish_then_destroy_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-sweep", "lease-1")));
    let env = support::build_services(Arc::clone(&fake));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(support::create_request_with_policy(
            FinalizePolicy::PublishThenDestroy,
        ))
        .expect("session create succeeds")
        .workspace_session_id;

    for swept_id in env.workspace.session_ids() {
        let _ = env.workspace.remount_session(&swept_id);
    }

    assert!(
        env.workspace.resolve_session(workspace_session_id).is_ok(),
        "an idle publish_then_destroy session survives the remount sweep"
    );
    assert!(fake.destroy_calls().is_empty());
    assert!(fake.capture_calls().is_empty());
}

#[test]
fn sync_op_racing_last_completion_blocks_on_gate_and_gets_not_found() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-race", "lease-1")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "done\n",
    )));
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let (entered, release) = fake.park_next_destroy();

    let output = env
        .command
        .exec_command(exec_input(None, 0))
        .expect("implicit session command starts");
    let workspace_session_id = output
        .workspace_session_id
        .expect("exec_command returns the session id");
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("finalize reached the destroy hook under the gate");

    let file_op_workspace = Arc::clone(&env.workspace);
    let file_op_id = workspace_session_id.clone();
    let file_op = std::thread::spawn(move || {
        file_op_workspace.run_file_op(
            &file_op_id,
            FileRunnerOp::ReadFile {
                rel: "f.txt".to_owned(),
                max_bytes: 16,
            },
        )
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        fake.run_file_op_calls().is_empty(),
        "the file op must wait on the session gate while finalize holds it"
    );

    release.send(()).expect("release the parked destroy");
    let result = file_op.join().expect("file op thread");
    assert!(matches!(
        result,
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert!(
        fake.run_file_op_calls().is_empty(),
        "a sync op racing the last completion never runs against the finalized session"
    );
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn launch_failure_completes_under_the_held_guard_without_deadlock() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-launch-fail", "lease-1")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_spawn_error(
        sandbox_runtime::command::CommandServiceError::InvalidCommand {
            message: "scripted spawn failure".to_owned(),
        },
    );
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let (result_tx, result_rx) = mpsc::channel();
    let command = Arc::clone(&env.command);
    std::thread::spawn(move || {
        let _ = result_tx.send(command.exec_command(exec_input(None, 0)));
    });

    let result = result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("launch-failure completion must not deadlock on the admission gate");
    assert!(matches!(
        result,
        Err(sandbox_runtime::command::CommandServiceError::CommandIo { .. })
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("ws-launch-fail".to_owned())],
        "the failed launch completes through the ordinary trigger and finalizes"
    );
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn completion_against_a_missing_session_is_a_silent_no_op() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-faulty", "lease-1")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let launcher = launch_driver.launcher();
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;
    let output = env
        .command
        .exec_command(exec_input(Some(workspace_session_id.clone()), 0))
        .expect("command starts");
    let command_session_id = output.command_session_id.expect("command is running");

    let lease_errors = env.workspace.destroy_faulty_session(&workspace_session_id);
    assert!(lease_errors.is_empty());
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);

    launcher.complete_request(&command_session_id.0, ok_run_result());
    assert!(
        wait_until(Duration::from_secs(5), || {
            env.workspace.gate_entry_count() == 0
        }),
        "the late completion no-ops against the missing session and leaves no gate entry"
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![workspace_session_id.clone()],
        "the late completion never destroys again"
    );
}

#[test]
fn guarded_destroy_accepts_a_finalize_failed_session() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-stuck", "lease-1")));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "finalize destroy failed".to_owned(),
    }));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "done\n",
    )));
    let env = support::build_services_with_launch_driver(Arc::clone(&fake), launch_driver);

    let _ = env
        .command
        .exec_command(exec_input(None, 250))
        .expect("implicit session command completes");
    assert!(
        wait_until(Duration::from_secs(5), || !fake.destroy_calls().is_empty()),
        "finalize attempts the destroy"
    );
    let workspace_session_id = WorkspaceSessionId("ws-stuck".to_owned());
    assert!(
        env.workspace
            .resolve_session(workspace_session_id.clone())
            .is_ok(),
        "a failed finalize leaves the session resolvable for recovery"
    );

    let result = env
        .workspace
        .guarded_destroy(workspace_session_id.clone(), None)
        .expect("guarded destroy recovers a finalize_failed session");
    assert_eq!(result.workspace_session_id, workspace_session_id);
    assert_eq!(fake.destroy_calls().len(), 2);
    let missing = env
        .workspace
        .resolve_session(workspace_session_id)
        .expect_err("recovered session is gone");
    assert!(matches!(missing, WorkspaceSessionError::NotFound { .. }));
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn gates_map_does_not_grow_on_dead_id_touches() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let env = support::build_services(Arc::clone(&fake));
    let dead = WorkspaceSessionId("ws-dead".to_owned());

    let file_op = env.workspace.run_file_op(
        &dead,
        FileRunnerOp::ReadFile {
            rel: "f.txt".to_owned(),
            max_bytes: 16,
        },
    );
    assert!(matches!(
        file_op,
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert_eq!(env.workspace.gate_entry_count(), 0);

    let destroy = env.workspace.guarded_destroy(dead.clone(), None);
    assert!(matches!(
        destroy,
        Err(WorkspaceSessionError::NotFound { .. })
    ));
    assert_eq!(env.workspace.gate_entry_count(), 0);

    let admission = env.command.exec_command(exec_input(Some(dead.clone()), 0));
    assert!(admission.is_err());
    assert_eq!(env.workspace.gate_entry_count(), 0);

    let swept = env.workspace.remount_session(&dead);
    assert_eq!(
        swept.disposition,
        sandbox_runtime::workspace_session::SweptDisposition::SessionGone
    );
    assert_eq!(env.workspace.gate_entry_count(), 0);

    assert!(env.workspace.destroy_faulty_session(&dead).is_empty());
    assert_eq!(env.workspace.gate_entry_count(), 0);
}

#[test]
fn sessions_map_stays_free_while_destroy_io_runs() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("ws-io", "lease-1")));
    fake.push_create_result(Ok(workspace_handle("ws-other", "lease-2")));
    let env = support::build_services(Arc::clone(&fake));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;
    let other_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("second session create succeeds")
        .workspace_session_id;

    let (entered, release) = fake.park_next_destroy();
    let destroy_workspace = Arc::clone(&env.workspace);
    let destroy_id = workspace_session_id.clone();
    let destroyer = std::thread::spawn(move || destroy_workspace.guarded_destroy(destroy_id, None));
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("destroy reached the workspace hook");

    let (done_tx, done_rx) = mpsc::channel();
    let read_workspace = Arc::clone(&env.workspace);
    let read_id = other_id.clone();
    std::thread::spawn(move || {
        let resolved = read_workspace.resolve_session(read_id).is_ok();
        let ids = read_workspace.session_ids();
        let _ = done_tx.send((resolved, ids));
    });
    let (resolved, ids) = done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("sessions map reads must not block behind destroy I/O");
    assert!(resolved);
    assert!(ids.contains(&other_id));

    release.send(()).expect("release the parked destroy");
    destroyer
        .join()
        .expect("destroy thread")
        .expect("guarded destroy succeeds");
}

// ---------------------------------------------------------------------------
// CLI dispatch surface.
// ---------------------------------------------------------------------------

#[test]
fn workspace_session_create_operation_defaults_host_profile_and_projects_minimal_json(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let operations = operations_with_fake(&fake)?;

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request("create_workspace_session", json!({})),
    )
    .into_json_value();

    assert_eq!(
        response,
        json!({
            "workspace_session_id": "workspace-1",
            "network_profile": "shared",
            "finalize_policy": "no_op",
        })
    );
    assert_eq!(fake.create_requests(), vec![support::raw_create_request()]);
    Ok(())
}

#[test]
fn workspace_session_create_operation_accepts_isolated_profile(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle_with_profile(
        "workspace-1",
        "lease-1",
        NetworkProfile::Isolated,
    )));
    let operations = operations_with_fake(&fake)?;

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "create_workspace_session",
            json!({ "network_profile": "isolated" }),
        ),
    )
    .into_json_value();

    assert_eq!(
        response,
        json!({
            "workspace_session_id": "workspace-1",
            "network_profile": "isolated",
            "finalize_policy": "no_op",
        })
    );
    Ok(())
}

#[test]
fn workspace_session_create_operation_rejects_invalid_profiles(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for args in [
        json!({ "network_profile": "unknown" }),
        json!({ "network_profile": "" }),
        json!({ "network_profile": 7 }),
    ] {
        let fake = Arc::new(FakeWorkspaceService::new());
        let operations = operations_with_fake(&fake)?;

        let response = sandbox_runtime::dispatch_operation(
            &operations,
            &runtime_request("create_workspace_session", args),
        )
        .into_json_value();

        assert_eq!(response["error"]["kind"], "invalid_request");
        assert!(fake.create_requests().is_empty());
    }
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_rejects_invalid_args_without_raw_destroy(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for args in [
        json!({}),
        json!({ "workspace_session_id": "" }),
        json!({ "workspace_session_id": 7 }),
        json!({ "workspace_session_id": "workspace-1", "grace_s": "NaN" }),
        json!({ "workspace_session_id": "workspace-1", "grace_s": -0.1 }),
    ] {
        let fake = Arc::new(FakeWorkspaceService::new());
        let operations = operations_with_fake(&fake)?;

        let response = sandbox_runtime::dispatch_operation(
            &operations,
            &runtime_request("destroy_workspace_session", args),
        )
        .into_json_value();

        assert_eq!(response["error"]["kind"], "invalid_request");
        assert!(fake.destroy_calls().is_empty());
    }
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_unknown_session_does_not_call_raw_destroy(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    let operations = operations_with_fake(&fake)?;

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "destroy_workspace_session",
            json!({ "workspace_session_id": "missing" }),
        ),
    )
    .into_json_value();

    assert_eq!(response["error"]["kind"], "operation_failed");
    assert!(fake.destroy_calls().is_empty());
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_rejects_active_commands_without_raw_destroy(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(support::FakeWorkspaceService::new());
    fake.push_create_result(Ok(support::workspace_handle(
        "workspace-1",
        "lease-1",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    )));
    let services = support::build_services(Arc::clone(&fake));
    let workspace_session_id = services
        .workspace
        .create_workspace_session(support::create_request())
        .expect("session create succeeds")
        .workspace_session_id;
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service()?,
        support::test_file_service(),
    );

    let exec_response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "exec_command",
            json!({
                "workspace_session_id": workspace_session_id.0.clone(),
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        ),
    )
    .into_json_value();
    assert_eq!(exec_response["command_session_id"], "namespace_execution_1");
    assert_eq!(
        exec_response["workspace_session_id"],
        workspace_session_id.0
    );

    let destroy_response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "destroy_workspace_session",
            json!({ "workspace_session_id": workspace_session_id.0 }),
        ),
    )
    .into_json_value();

    assert_eq!(destroy_response["error"]["kind"], "operation_failed");
    assert_eq!(
        destroy_response["error"]["details"]["active_command_session_ids"],
        json!(["namespace_execution_1"])
    );
    assert!(fake.destroy_calls().is_empty());
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_success_projects_minimal_json(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    let operations = operations_with_fake(&fake)?;
    operations
        .workspace_session
        .create_workspace_session(create_request())
        .expect("session create succeeds");

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "destroy_workspace_session",
            json!({ "workspace_session_id": "workspace-1", "grace_s": 2.5 }),
        ),
    )
    .into_json_value();

    assert_eq!(
        response,
        json!({
            "workspace_session_id": "workspace-1",
            "destroyed": true,
            "evicted_upperdir_bytes": 4096,
        })
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
    Ok(())
}

#[test]
fn workspace_session_destroy_operation_failure_retains_session(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle("workspace-1", "lease-1")));
    fake.push_destroy_result(Err(WorkspaceError::Setup {
        step: "destroy failed".to_owned(),
    }));
    let operations = operations_with_fake(&fake)?;
    operations
        .workspace_session
        .create_workspace_session(create_request())
        .expect("session create succeeds");

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &runtime_request(
            "destroy_workspace_session",
            json!({ "workspace_session_id": "workspace-1" }),
        ),
    )
    .into_json_value();

    assert_eq!(response["error"]["kind"], "operation_failed");
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("workspace-1".to_owned())]
    );
    assert!(operations
        .workspace_session
        .resolve_session(WorkspaceSessionId("workspace-1".to_owned()))
        .is_ok());
    Ok(())
}

#[test]
fn workspace_session_files_do_not_import_command_service() {
    let core = include_str!("../src/workspace_session/service/core.rs");
    let admission = include_str!("../src/workspace_session/service/impls/admission.rs");
    let finalize_session =
        include_str!("../src/workspace_session/service/impls/finalize_session.rs");
    let guarded_destroy = include_str!("../src/workspace_session/service/impls/guarded_destroy.rs");
    let create_workspace_session =
        include_str!("../src/workspace_session/service/impls/create_workspace_session.rs");
    let destroy_session = include_str!("../src/workspace_session/service/impls/destroy_session.rs");
    let resolve_session = include_str!("../src/workspace_session/service/impls/resolve_session.rs");
    let model = include_str!("../src/workspace_session/service/model.rs");
    let service = include_str!("../src/workspace_session/service.rs");
    let error = include_str!("../src/workspace_session/error.rs");

    for source in [
        core,
        admission,
        finalize_session,
        guarded_destroy,
        create_workspace_session,
        destroy_session,
        resolve_session,
        model,
        service,
        error,
    ] {
        assert!(!source.contains("crate::command"));
        assert!(!source.contains("CommandOperationService"));
    }
}

fn operations_with_fake(
    fake: &Arc<FakeWorkspaceService>,
) -> Result<SandboxRuntimeOperations, Box<dyn std::error::Error + Send + Sync>> {
    let layerstack = layerstack_service()?;
    let workspace = Arc::new(WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(fake)),
        Arc::clone(&layerstack),
        Observer::disabled(),
    ));
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&workspace),
        sandbox_runtime::command::CommandConfig::default(),
        Observer::disabled(),
    ));
    Ok(SandboxRuntimeOperations::new(
        command,
        workspace,
        layerstack,
        support::test_file_service(),
    ))
}

fn runtime_request(op: &str, args: serde_json::Value) -> Request {
    Request::new(op, "req-test", CliOperationScope::system(), args)
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
        Observer::disabled(),
        support::test_file_service(),
    )?))
}

fn temp_root() -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "sandbox-runtime-workspace-session-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ))
}
