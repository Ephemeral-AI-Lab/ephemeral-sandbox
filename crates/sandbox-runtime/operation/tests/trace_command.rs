mod support;

use std::path::PathBuf;
use std::sync::Arc;

use sandbox_runtime::command::{ExecCommandInput, ReadCommandLinesInput, WriteCommandStdinInput};
use sandbox_runtime_command::yield_wait_loop::WaitOutcome;
use sandbox_runtime_workspace::WorkspaceProfile;

use support::{
    build_services_with_launch_driver, create_request, success_exit, trace::capture_traces,
    workspace_handle, FakeLaunchDriver, FakeWorkspaceService,
};

#[test]
fn command_trace_spans_omit_sensitive_values() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(WaitOutcome::Running(
        "STDOUT_SECRET_SENTINEL initial\n".to_owned(),
    ));
    launch_driver.push_outcome(WaitOutcome::Completed(success_exit(
        "STDERR_STDOUT_SECRET_SENTINEL final\n",
    )));
    let env = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    fake.push_create_result(Ok(workspace_handle(
        "workspace-secret",
        "lease-secret",
        PathBuf::from("/workspace/PATH_SECRET_SENTINEL"),
        WorkspaceProfile::HostCompatible,
    )));
    let workspace_session_id = env
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id;

    let traces = capture_traces(|| {
        let command_session_id = env
            .command
            .exec_command(ExecCommandInput {
                workspace_session_id: Some(workspace_session_id),
                cmd: "printf COMMAND_SECRET_SENTINEL && export TOKEN=AUTH_ENV_SECRET_SENTINEL"
                    .to_owned(),
                timeout_ms: Some(2500),
                yield_time_ms: Some(1),
            })
            .expect("command starts")
            .command_session_id
            .expect("running command id returned");

        env.command
            .write_command_stdin(WriteCommandStdinInput {
                command_session_id: command_session_id.clone(),
                stdin: "STDIN_SECRET_SENTINEL\n".to_owned(),
                yield_time_ms: Some(1),
            })
            .expect("stdin write completes command");

        env.command
            .read_command_lines(ReadCommandLinesInput {
                command_session_id,
                start_offset: Some(0),
                limit: Some(10),
            })
            .expect("completed command output remains readable");
    });

    for expected in [
        "runtime.exec_command",
        "command.spawn",
        "command.wait_initial_yield",
        "runtime.write_command_stdin",
        "command.finalize",
        "runtime.read_command_lines",
    ] {
        assert!(traces.contains(expected), "missing {expected} in {traces}");
    }
    for forbidden in [
        "COMMAND_SECRET_SENTINEL",
        "AUTH_ENV_SECRET_SENTINEL",
        "STDIN_SECRET_SENTINEL",
        "STDOUT_SECRET_SENTINEL",
        "STDERR_STDOUT_SECRET_SENTINEL",
        "PATH_SECRET_SENTINEL",
        "/workspace/",
        "/lower/one",
        "transcript.log",
        "lease-secret",
    ] {
        assert!(
            !traces.contains(forbidden),
            "forbidden value {forbidden} appeared in traces: {traces}"
        );
    }
}
