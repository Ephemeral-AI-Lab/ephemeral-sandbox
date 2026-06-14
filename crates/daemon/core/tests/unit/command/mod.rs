use serde_json::json;

use command::{CollectCompleted, ReadCommandProgress};
use config::configs::daemon::PluginRuntimeConfig;
use config::configs::isolated_workspace::IsolatedWorkspaceConfig;
use namespace::protocol::{RunRequest, RunResult};
use operation::command::contract::{
    CancelCommandInput, CommandCompletion, CommandResponse, CommandStatus, ExecCommandInput,
    ReadProgressInput, WriteStdinInput,
};
use operation::control::contract::CallerCountInput;
use operation::{CommandId, OpRequest};
use protocol::catalog::BuiltinOp;
use std::path::Path;
use std::process::Child;
use std::sync::Arc;
use std::time::Duration;
use workspace::{LaunchError, NsRunnerLauncher};

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const TEST_INVOCATION_ID: &str = "test-invocation";

#[test]
fn exec_command_requires_string_wire_shape() {
    assert!(parse_exec_input(json!({"cmd": "echo hi"})).is_ok());
    assert!(parse_exec_input(json!({"cmd": ["true"]})).is_err());
}

#[test]
fn exec_command_preserves_shell_string_bytes_after_validation() -> TestResult {
    assert_eq!(
        parse_exec_input(json!({"cmd": "  printf hi\n"}))
            .expect("valid command input")
            .cmd,
        "  printf hi\n"
    );
    Ok(())
}

#[test]
fn optional_u64_accepts_unsigned_and_nonnegative_signed_numbers() {
    assert_eq!(
        parse_exec_input(json!({"cmd": "true", "timeout": 7_u64}))
            .expect("valid command input")
            .timeout,
        Some(7)
    );
    assert_eq!(
        parse_exec_input(json!({"cmd": "true", "timeout": 7_i64}))
            .expect("valid command input")
            .timeout,
        Some(7)
    );
    assert_eq!(
        parse_exec_input(json!({"cmd": "true", "timeout": -1_i64}))
            .expect("valid command input")
            .timeout,
        None
    );
}

#[test]
fn exec_timeout_uses_config_default_only_when_omitted() {
    let config = command::CommandConfig {
        default_timeout_s: 600,
        ..command::CommandConfig::default()
    };

    assert_eq!(
        exec_timeout_seconds(
            &parse_exec_input(json!({"cmd": "true"})).expect("valid command input"),
            &config
        ),
        600.0
    );
    assert_eq!(
        exec_timeout_seconds(
            &parse_exec_input(json!({"cmd": "true", "timeout": 12}))
                .expect("valid command input with timeout"),
            &config
        ),
        12.0
    );
    assert_eq!(
        exec_timeout_seconds(
            &parse_exec_input(json!({"cmd": "true", "timeout_seconds": 34}))
                .expect("valid command input with timeout_seconds"),
            &config
        ),
        34.0
    );
    assert_eq!(
        exec_timeout_seconds(
            &parse_exec_input(json!({"cmd": "true", "timeout": 12, "timeout_seconds": 34}))
                .expect("valid command input with both timeout spellings"),
            &config
        ),
        12.0
    );
}

#[test]
fn command_completion_result_can_be_read_by_progress_tool() -> TestResult {
    let manager = operation::command::CommandOps::new(command::CommandConfig::default());
    manager.push_completed(test_completion("cmd_keep", "caller", "keep\n"));
    manager.push_completed(test_completion("cmd_done", "caller", "a\ndone\n"));

    let result = manager.read_command_progress(ReadCommandProgress {
        command_id: "cmd_done".to_owned(),
        last_n_lines: 1,
    })?;
    assert_eq!(result.status, CommandStatus::Ok);
    assert_eq!(result.stdout, "done\n");

    let redelivered = manager.read_command_progress(ReadCommandProgress {
        command_id: "cmd_done".to_owned(),
        last_n_lines: 2,
    })?;
    assert_eq!(redelivered.stdout, "a\ndone\n");

    let remaining = manager.collect_completed(&CollectCompleted {
        command_ids: Some(vec!["cmd_keep".to_owned()]),
        caller_id: None,
    });
    assert_eq!(remaining.completions.len(), 1);

    // Remove-on-deliver: a second collect finds nothing, so delivered entries do
    // not accumulate forever.
    let redelivered = manager.collect_completed(&CollectCompleted {
        command_ids: Some(vec!["cmd_keep".to_owned()]),
        caller_id: None,
    });
    assert_eq!(redelivered.completions.len(), 0);
    Ok(())
}

#[test]
fn command_count_uses_runtime_manager() -> TestResult {
    let services = test_services();
    let response = op_command_count(
        parse_count_input(json!({"caller_id": "no-live-session"})),
        DispatchContext::with_services(&services),
    )?;

    assert_eq!(response["success"], true);
    assert_eq!(response["caller_id"], "no-live-session");
    assert_eq!(response["count"], 0);
    Ok(())
}

#[test]
fn command_read_progress_returns_completed_result_when_live_command_is_gone() -> TestResult {
    let id = "cmd_progress_done_unit";
    let services = test_services();
    services
        .command
        .push_completed(test_completion(id, "caller", "written\n"));

    let response = command_read_progress(
        parse_read_progress_input(json!({"command_id": id, "last_n_lines": 1})),
        DispatchContext::with_services(&services),
    )?;

    assert_eq!(response["status"], "ok");
    assert_eq!(response["output"]["stdout"], "written\n");
    let remaining = services.command.collect_completed(&CollectCompleted {
        command_ids: Some(vec![id.to_owned()]),
        caller_id: None,
    });
    assert_eq!(remaining.completions.len(), 1);
    Ok(())
}

#[test]
fn command_write_stdin_does_not_claim_parked_completion() -> TestResult {
    let id = "cmd_stdin_done_unit";
    let services = test_services();
    services
        .command
        .push_completed(test_completion(id, "caller", "written\n"));

    let response = command_write_stdin(
        parse_write_stdin_input(json!({"command_id": id, "chars": "ignored"})),
        DispatchContext::with_services(&services),
    )?;

    assert_eq!(response["status"], "error");
    assert_eq!(response["output"]["stderr"], "command_not_found");
    Ok(())
}

#[test]
fn command_stdin_written_event_records_bounded_wait_facts() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = DispatchContext::empty().with_trace_events(sink.clone());
    record_stdin_written(
        &context,
        &CommandStdinTraceFacts {
            command_id: "cmd_stdin_event".to_owned(),
            bytes: 12,
            wait_ms: 34,
            waited_for_output: true,
            status: CommandStatus::Running,
        },
    );

    let events = sink.drain();
    assert_eq!(events.len(), 1);
    let event = events.first().expect("stdin trace event");
    assert_eq!(event.module, "command");
    assert_eq!(event.name, "stdin_written");
    assert_eq!(event.details["command_id"], "cmd_stdin_event");
    assert_eq!(event.details["bytes"], 12);
    assert_eq!(event.details["wait_ms"], 34);
    assert_eq!(event.details["waited_for_output"], true);
    assert_eq!(event.details["status"], "running");
}

#[test]
fn command_start_trace_events_are_recorded_in_request_sidecar_sink() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = DispatchContext::empty().with_trace_events(sink.clone());
    record_command_trace_events(
        &context,
        &[
            CommandTraceEvent::new(
                "prepared",
                json!({"command_id": "cmd_exec", "workspace": "ephemeral"}),
            ),
            CommandTraceEvent::artifact_written(
                "metadata",
                std::path::Path::new("/tmp/metadata.json"),
                15,
            ),
            CommandTraceEvent::artifact_failed(
                "runner_request",
                std::path::Path::new("/tmp/runner-request.json"),
                "permission denied",
            ),
        ],
    );

    let events = sink.drain();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].module, "command");
    assert_eq!(events[0].name, "prepared");
    assert_eq!(events[0].details["command_id"], "cmd_exec");
    assert_eq!(events[1].module, "command");
    assert_eq!(events[1].name, "artifact_written");
    assert_eq!(events[1].details["artifact"], "metadata");
    assert_eq!(events[1].details["bytes"], 15);
    assert_eq!(events[2].module, "command");
    assert_eq!(events[2].name, "artifact_failed");
    assert_eq!(events[2].details["artifact"], "runner_request");
    assert_eq!(events[2].details["path"], "/tmp/runner-request.json");
    assert_eq!(events[2].details["error"], "permission denied");
}

#[test]
fn command_resource_stats_trace_events_use_resource_module() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = DispatchContext::empty().with_trace_events(sink.clone());
    record_command_trace_events(
        &context,
        &[CommandTraceEvent::new(
            "resource_stats",
            json!({
                "meta": {
                    "stats_kind": "cgroup_process",
                    "phase": "before",
                    "source": "command.process.wait",
                },
            }),
        )],
    );

    let events = sink.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].module, "resource");
    assert_eq!(events[0].name, "resource_stats");
    assert_eq!(events[0].details["meta"]["phase"], "before");
    assert_eq!(events[0].details["meta"]["source"], "command.process.wait");
    assert!(events[0].details["meta"]["inflight_requests"].is_number());
}

#[test]
fn command_progress_read_event_records_source_and_output_size() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = DispatchContext::empty().with_trace_events(sink.clone());
    record_progress_read(
        &context,
        &CommandProgressTraceFacts {
            command_id: "cmd_progress_event".to_owned(),
            last_n_lines: 3,
            status: CommandStatus::Ok,
            source: "completed_buffer",
            stdout_bytes: 17,
        },
    );

    let events = sink.drain();
    assert_eq!(events.len(), 1);
    let event = events.first().expect("progress trace event");
    assert_eq!(event.module, "command");
    assert_eq!(event.name, "progress_read");
    assert_eq!(event.details["command_id"], "cmd_progress_event");
    assert_eq!(event.details["last_n_lines"], 3);
    assert_eq!(event.details["status"], "ok");
    assert_eq!(event.details["source"], "completed_buffer");
    assert_eq!(event.details["stdout_bytes"], 17);
}

#[test]
fn command_cancel_returns_completed_result_when_live_command_is_gone() -> TestResult {
    let id = "command_cancel_done_unit";
    let services = test_services();
    services
        .command
        .push_completed(test_completion(id, "caller", "already-finished\n"));

    let response = command_cancel(
        parse_cancel_input(json!({"command_id": id})),
        DispatchContext::with_services(&services),
    )?;

    assert_eq!(response["status"], "ok");
    assert_eq!(response["output"]["stdout"], "already-finished\n");
    let remaining = services.command.collect_completed(&CollectCompleted {
        command_ids: Some(vec![id.to_owned()]),
        caller_id: None,
    });
    assert_eq!(remaining.completions.len(), 0);
    Ok(())
}

fn test_services() -> crate::RuntimeServices {
    crate::RuntimeServices::new(
        PluginRuntimeConfig::default(),
        IsolatedWorkspaceConfig::default(),
        command::CommandConfig::default(),
        Arc::new(NoLaunch),
    )
}

struct NoLaunch;

impl NsRunnerLauncher for NoLaunch {
    fn run(&self, _request: &RunRequest) -> Result<RunResult, LaunchError> {
        Err(LaunchError::Failed(
            "command unit tests do not start ns-runner".to_owned(),
        ))
    }

    fn spawn_detached(
        &self,
        _request: &RunRequest,
        _stderr_path: &Path,
    ) -> Result<Child, LaunchError> {
        Err(LaunchError::Failed(
            "command unit tests do not start ns-runner".to_owned(),
        ))
    }

    fn remount_in(
        &self,
        _target_pid: u32,
        _request: &RunRequest,
        _timeout: Duration,
    ) -> Result<(), LaunchError> {
        Err(LaunchError::Failed(
            "command unit tests do not start ns-runner".to_owned(),
        ))
    }
}

fn parse_exec_input(args: serde_json::Value) -> Result<ExecCommandInput, operation::RequestError> {
    match OpRequest::parse(BuiltinOp::ExecCommand, &args, TEST_INVOCATION_ID)? {
        OpRequest::ExecCommand(input) => Ok(input),
        _ => unreachable!("exec op parses to exec input"),
    }
}

fn parse_count_input(args: serde_json::Value) -> CallerCountInput {
    match OpRequest::parse(BuiltinOp::CommandCount, &args, TEST_INVOCATION_ID)
        .expect("valid count input")
    {
        OpRequest::CommandCount(input) => input,
        _ => unreachable!("count op parses to count input"),
    }
}

fn parse_read_progress_input(args: serde_json::Value) -> ReadProgressInput {
    match OpRequest::parse(BuiltinOp::CommandReadProgress, &args, TEST_INVOCATION_ID)
        .expect("valid poll input")
    {
        OpRequest::CommandReadProgress(input) => input,
        _ => unreachable!("poll op parses to poll input"),
    }
}

fn parse_write_stdin_input(args: serde_json::Value) -> WriteStdinInput {
    match OpRequest::parse(BuiltinOp::WriteStdin, &args, TEST_INVOCATION_ID)
        .expect("valid stdin input")
    {
        OpRequest::WriteStdin(input) => input,
        _ => unreachable!("stdin op parses to stdin input"),
    }
}

fn parse_cancel_input(args: serde_json::Value) -> CancelCommandInput {
    match OpRequest::parse(BuiltinOp::CommandCancel, &args, TEST_INVOCATION_ID)
        .expect("valid cancel input")
    {
        OpRequest::CommandCancel(input) => input,
        _ => unreachable!("cancel op parses to cancel input"),
    }
}

fn test_completion(id: &str, caller_id: &str, stdout: &str) -> CommandCompletion {
    let result = CommandResponse {
        status: CommandStatus::Ok,
        exit_code: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
        command_id: Some(CommandId::new(id.to_owned())),
        finalized: None,
    };
    CommandCompletion {
        command_id: id.to_owned(),
        caller_id: caller_id.to_owned(),
        command: "test".to_owned(),
        result,
    }
}
