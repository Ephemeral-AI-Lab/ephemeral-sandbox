//! Completed-entry retention at the command-service level: the engine registry
//! keeps a bounded set of terminal entries, and eviction drops the stored
//! `CommandExecValue` — closing the pty master fd it wraps and removing the
//! command's scratch directory through the value's `Drop`. A drain against an
//! evicted id observes `CommandNotFound`.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use sandbox_observability::{Observer, SpanRegistry};
use sandbox_runtime::command::{
    CommandOperationService, CommandServiceError, CommandStatus, ExecCommandInput,
    ReadCommandLinesInput, WriteCommandStdinInput,
};
use sandbox_runtime::workspace_session::WorkspaceSessionService;
use sandbox_runtime_namespace_execution::NamespaceExecutionEngine;
use sandbox_runtime_namespace_process::runner::protocol::CommandSecurityPolicy;
use sandbox_runtime_workspace::{NetworkProfile, WorkspaceSessionId};

use support::{FakeLaunchDriver, FakeWorkspaceService, ScriptedCommandYield, TestServices};

fn retention_services(
    fake: &Arc<FakeWorkspaceService>,
    launch_driver: &Arc<FakeLaunchDriver>,
    max_terminal: usize,
) -> TestServices {
    let obs = Observer::disabled();
    let workspace = Arc::new(WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(fake)),
        support::observed_layerstack_service(Observer::disabled()),
        obs.clone(),
    ));
    let exec_spans = Arc::new(SpanRegistry::new(obs.clone()));
    let engine = Arc::new(NamespaceExecutionEngine::with_launcher(
        Box::new(launch_driver.launcher()),
        exec_spans.clone(),
        256,
        30.0,
    ));
    engine.set_terminal_retention(max_terminal);
    let command = Arc::new(CommandOperationService::with_engine(
        Arc::clone(&workspace),
        sandbox_runtime::command::CommandConfig {
            scratch_root: std::env::temp_dir().join(format!(
                "namespace-execution-retention-{}-{max_terminal}",
                std::process::id()
            )),
            command_security: CommandSecurityPolicy::off(),
        },
        engine,
        exec_spans,
        obs,
    ));
    TestServices { workspace, command }
}

fn exec_await(workspace_session_id: WorkspaceSessionId) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: Some(workspace_session_id),
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(250),
    }
}

#[test]
fn terminal_eviction_removes_scratch_dir_and_drains_return_command_not_found() {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(support::workspace_handle(
        "ws-retention",
        "lease-1",
        PathBuf::from("/workspace/retention"),
        NetworkProfile::Shared,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "first\n",
    )));
    launch_driver.push_outcome(ScriptedCommandYield::Completed(support::success_exit(
        "second\n",
    )));
    let env = retention_services(&fake, &launch_driver, 1);
    let workspace_session_id = env
        .workspace
        .create_workspace_session(support::create_request())
        .expect("session create succeeds")
        .workspace_session_id;

    let first = env
        .command
        .exec_command(exec_await(workspace_session_id.clone()))
        .expect("first command completes");
    assert_eq!(first.status, CommandStatus::Ok);
    let first_scratch = env
        .command
        .config()
        .scratch_root
        .join("namespace_execution_1");
    assert!(
        first_scratch.is_dir(),
        "the retained first command keeps its scratch dir"
    );

    let second = env
        .command
        .exec_command(exec_await(workspace_session_id))
        .expect("second command completes");
    assert_eq!(second.status, CommandStatus::Ok);

    assert!(
        !first_scratch.exists(),
        "evicting the oldest terminal entry drops its value, which removes the scratch dir \
         (the same drop closes the wrapped pty master fd)"
    );
    let drained = env.command.write_command_stdin(WriteCommandStdinInput {
        command_session_id: sandbox_runtime::NamespaceExecutionId(
            "namespace_execution_1".to_owned(),
        ),
        stdin: "late\n".to_owned(),
        yield_time_ms: Some(0),
    });
    assert!(
        matches!(
            drained,
            Err(CommandServiceError::CommandNotFound { command_session_id })
                if command_session_id.0 == "namespace_execution_1"
        ),
        "a drain against the evicted id returns CommandNotFound"
    );

    let survivor = env.command.read_command_lines(ReadCommandLinesInput {
        command_session_id: sandbox_runtime::NamespaceExecutionId(
            "namespace_execution_2".to_owned(),
        ),
        start_offset: Some(0),
        limit: Some(10),
    });
    assert_eq!(
        survivor.status,
        CommandStatus::Ok,
        "the newest terminal entry stays drainable"
    );
}
