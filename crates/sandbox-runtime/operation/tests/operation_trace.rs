use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use sandbox_protocol::{CliOperationScope, Request};
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::{
    span_keys, AsyncTraceSink, CommandFinalizationTraceMetadata, CompletedOperationTrace,
    OperationTrace, SandboxRuntimeOperations, WorkspaceProfile,
};
use serde_json::json;

mod support;
use support::{
    build_services, build_services_with_launch_driver,
    build_services_with_launch_driver_and_async_trace_sink, create_request, success_exit,
    workspace_handle, FakeLaunchDriver, FakeWorkspaceService, ScriptedCommandYield,
};

#[test]
fn operation_trace_records_call_order_and_parentage() {
    let trace = OperationTrace::new();
    {
        let _root = trace.enter("root");
        {
            let _child = trace.enter("child");
        }
        {
            let _sibling = trace.enter("sibling");
        }
    }

    let completed = trace.complete();
    let root = span(&completed, "root");
    let child = span(&completed, "child");
    let sibling = span(&completed, "sibling");
    assert_eq!(root.call_index, 0);
    assert_eq!(root.parent_call_index, None);
    assert_eq!(child.call_index, 1);
    assert_eq!(child.parent_call_index, Some(0));
    assert_eq!(sibling.call_index, 2);
    assert_eq!(sibling.parent_call_index, Some(0));
}

#[test]
fn operation_trace_closes_span_on_early_return() {
    fn return_early(trace: &OperationTrace) {
        let _span = trace.enter("early_return");
        if std::hint::black_box(true) {
            return;
        }
        let _not_reached = trace.enter("not_reached");
    }

    let trace = OperationTrace::new();
    return_early(&trace);

    let completed = trace.complete();
    let span = span(&completed, "early_return");
    assert_eq!(span.status, "ok");
    assert!(span.duration_ms >= 0.0);
}

#[test]
fn operation_trace_marks_span_closed_during_panic_unwind() {
    let trace = OperationTrace::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _span = trace.enter("panic_span");
        panic!("intentional trace test panic");
    }));

    assert!(result.is_err());
    let completed = trace.complete();
    assert_eq!(span(&completed, "panic_span").status, "panic");
}

#[test]
fn operation_trace_disabled_span_key_calls_through_without_recording_child() {
    let trace = OperationTrace::new();
    let mut called = false;

    trace.measure("parent", || {
        trace.measure_if(span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE, || {
            called = true;
        });
    });

    assert!(called);
    assert_span_sequence(&trace, &[("parent", None)]);
}

#[test]
fn operation_trace_enabled_span_key_records_child_under_current_parent() {
    let trace =
        OperationTrace::new_with_enabled_span_keys([span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE]);

    trace.measure("parent", || {
        trace.measure_if(span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE, || {});
    });

    assert_span_sequence(
        &trace,
        &[
            ("parent", None),
            ("command.exec.workspace.resolve", Some(0)),
        ],
    );
}

#[test]
fn operation_trace_disabled_span_key_does_not_consume_call_index() {
    let trace = OperationTrace::new_with_enabled_span_keys([span_keys::COMMAND_EXEC_PROCESS_START]);

    trace.measure("parent", || {
        trace.measure_if(span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE, || {});
        trace.measure("coarse_sibling", || {});
        trace.measure_if(span_keys::COMMAND_EXEC_PROCESS_START, || {});
    });

    assert_span_sequence(
        &trace,
        &[
            ("parent", None),
            ("coarse_sibling", Some(0)),
            ("command.exec.process.start", Some(0)),
        ],
    );
}

#[test]
fn operation_trace_mixed_enabled_disabled_keys_keep_stable_ordering() {
    let trace = OperationTrace::new_with_enabled_span_keys([
        span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE,
        span_keys::COMMAND_EXEC_PROCESS_START,
    ]);

    trace.measure("parent", || {
        trace.measure_if(span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE, || {});
        trace.measure_if(
            span_keys::COMMAND_EXEC_WORKSPACE_CREATE_ONE_SHOT_SESSION,
            || {},
        );
        trace.measure_if(span_keys::COMMAND_EXEC_PROCESS_START, || {});
    });

    assert_span_sequence(
        &trace,
        &[
            ("parent", None),
            ("command.exec.workspace.resolve", Some(0)),
            ("command.exec.process.start", Some(0)),
        ],
    );
}

#[test]
fn operation_trace_records_selected_exec_command_span_set() -> Result<(), Box<dyn std::error::Error>>
{
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let (operations, workspace_session_id) = command_operations(launch_driver)?;
    let trace = OperationTrace::new();

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "exec_command",
            "req-exec",
            CliOperationScope::system(),
            json!({
                "workspace_session_id": workspace_session_id,
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        ),
        Some(&trace),
    )
    .into_json_value();

    assert!(response["command_session_id"].is_string());
    assert_selected_span_set(
        &trace,
        &[
            "dispatch_operation",
            "exec_command::dispatch",
            "CommandOperationService::exec_command",
        ],
    );
    Ok(())
}

#[test]
fn operation_trace_records_enabled_exec_command_child_spans(
) -> Result<(), Box<dyn std::error::Error>> {
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let (operations, workspace_session_id) = command_operations(launch_driver)?;
    let trace = OperationTrace::new_with_enabled_span_keys([
        span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE,
        span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE_EXISTING_SESSION,
        span_keys::COMMAND_EXEC_PROCESS_START,
    ]);

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "exec_command",
            "req-exec-deep",
            CliOperationScope::system(),
            json!({
                "workspace_session_id": workspace_session_id,
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        ),
        Some(&trace),
    )
    .into_json_value();

    assert!(response["command_session_id"].is_string());
    assert_span_sequence(
        &trace,
        &[
            ("dispatch_operation", None),
            ("exec_command::dispatch", Some(0)),
            ("CommandOperationService::exec_command", Some(1)),
            ("command.exec.workspace.resolve", Some(2)),
            ("command.exec.workspace.resolve_existing_session", Some(3)),
            ("command.exec.process.start", Some(2)),
        ],
    );
    Ok(())
}

#[test]
fn operation_trace_records_enabled_exec_command_one_shot_child_span(
) -> Result<(), Box<dyn std::error::Error>> {
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    let operations = one_shot_command_operations(launch_driver)?;
    let trace = OperationTrace::new_with_enabled_span_keys([
        span_keys::COMMAND_EXEC_WORKSPACE_RESOLVE,
        span_keys::COMMAND_EXEC_WORKSPACE_CREATE_ONE_SHOT_SESSION,
        span_keys::COMMAND_EXEC_PROCESS_START,
    ]);

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "exec_command",
            "req-exec-one-shot-deep",
            CliOperationScope::system(),
            json!({
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        ),
        Some(&trace),
    )
    .into_json_value();

    assert!(response["command_session_id"].is_string());
    assert_span_sequence(
        &trace,
        &[
            ("dispatch_operation", None),
            ("exec_command::dispatch", Some(0)),
            ("CommandOperationService::exec_command", Some(1)),
            ("command.exec.workspace.resolve", Some(2)),
            ("command.exec.workspace.create_one_shot_session", Some(3)),
            ("command.exec.process.start", Some(2)),
        ],
    );
    Ok(())
}

#[test]
fn operation_trace_records_command_finalization_async_span_tree(
) -> Result<(), Box<dyn std::error::Error>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "one-shot-session",
        "lease-1",
        PathBuf::from("/workspace/one-shot"),
        WorkspaceProfile::HostCompatible,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let (tx, rx) = mpsc::channel::<(CompletedOperationTrace, CommandFinalizationTraceMetadata)>();
    let sink: AsyncTraceSink = Arc::new(move |trace, metadata| {
        tx.send((trace, metadata))
            .expect("async trace test receiver stays open");
    });
    let services = build_services_with_launch_driver_and_async_trace_sink(
        Arc::clone(&fake),
        launch_driver,
        Some(sink),
    );
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service()?,
    );
    let request_trace = OperationTrace::new();

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "exec_command",
            "req-async-finalize",
            CliOperationScope::system(),
            json!({
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        ),
        Some(&request_trace),
    )
    .into_json_value();

    assert_eq!(response["status"], "ok");
    let (async_trace, metadata) = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("async finalization trace is emitted");
    assert_eq!(metadata.origin_request_id, "req-async-finalize");
    assert_eq!(
        metadata.workspace_session_id,
        Some(sandbox_runtime::WorkspaceSessionId(
            "one-shot-session".to_owned()
        ))
    );
    assert_eq!(metadata.command_session_id.0, "cmd_1");
    assert_eq!(metadata.finalizer_status, "ok");
    assert!(metadata.finalizer_error.is_none());
    assert_completed_span_sequence(
        &async_trace,
        &[
            ("complete_terminal_command_with_services", None),
            ("apply_workspace_completion_policy", Some(0)),
            ("complete_command_record", Some(0)),
        ],
    );
    Ok(())
}

#[test]
fn operation_trace_records_selected_write_command_stdin_span_set(
) -> Result<(), Box<dyn std::error::Error>> {
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running(String::new()));
    launch_driver.push_outcome(ScriptedCommandYield::Running("after input\n".to_owned()));
    let (operations, workspace_session_id) = command_operations(launch_driver)?;
    let command_session_id = start_running_command(&operations, workspace_session_id);
    let trace = OperationTrace::new();

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "write_command_stdin",
            "req-write",
            CliOperationScope::system(),
            json!({
                "command_session_id": command_session_id,
                "stdin": "input\n",
                "yield_time_ms": 0,
            }),
        ),
        Some(&trace),
    )
    .into_json_value();

    assert_eq!(response["output"], "after input");
    assert_selected_span_set(
        &trace,
        &[
            "dispatch_operation",
            "write_command_stdin::dispatch",
            "CommandOperationService::write_command_stdin",
        ],
    );
    Ok(())
}

#[test]
fn operation_trace_records_selected_read_command_lines_span_set(
) -> Result<(), Box<dyn std::error::Error>> {
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Running("initial\n".to_owned()));
    let (operations, workspace_session_id) = command_operations(launch_driver)?;
    let command_session_id = start_running_command(&operations, workspace_session_id);
    let trace = OperationTrace::new();

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "read_command_lines",
            "req-read",
            CliOperationScope::system(),
            json!({
                "command_session_id": command_session_id,
                "start_offset": 0,
                "limit": 10,
            }),
        ),
        Some(&trace),
    )
    .into_json_value();

    assert_eq!(response["output"], "initial");
    assert_selected_span_set(
        &trace,
        &[
            "dispatch_operation",
            "read_command_lines::dispatch",
            "CommandOperationService::read_command_lines",
        ],
    );
    Ok(())
}

#[test]
fn operation_trace_records_selected_squash_span_set() -> Result<(), Box<dyn std::error::Error>> {
    let services = build_services(Arc::new(FakeWorkspaceService::new()));
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service()?,
    );
    let trace = OperationTrace::new();

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "squash",
            "req-squash",
            CliOperationScope::system(),
            json!({}),
        ),
        Some(&trace),
    )
    .into_json_value();

    assert_eq!(response["squashed"], false);
    assert_selected_span_set(
        &trace,
        &[
            "dispatch_operation",
            "squash::dispatch",
            "LayerStackService::squash",
        ],
    );
    Ok(())
}

#[test]
fn operation_trace_records_selected_create_workspace_session_span_set(
) -> Result<(), Box<dyn std::error::Error>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "lease-1",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    )));
    let services = build_services(Arc::clone(&fake));
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service()?,
    );
    let trace = OperationTrace::new();

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "create_workspace_session",
            "req-create-workspace-session",
            CliOperationScope::system(),
            json!({}),
        ),
        Some(&trace),
    )
    .into_json_value();

    assert_eq!(response["workspace_session_id"], "workspace-session");
    assert_selected_span_set(
        &trace,
        &[
            "dispatch_operation",
            "create_workspace_session::dispatch",
            "WorkspaceSessionService::create_workspace_session",
        ],
    );
    Ok(())
}

#[test]
fn operation_trace_records_selected_destroy_workspace_session_span_set(
) -> Result<(), Box<dyn std::error::Error>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "lease-1",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    )));
    let services = build_services(Arc::clone(&fake));
    let handler = services
        .workspace
        .create_workspace_session(create_request())?;
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service()?,
    );
    let trace = OperationTrace::new();

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "destroy_workspace_session",
            "req-destroy-workspace-session",
            CliOperationScope::system(),
            json!({ "workspace_session_id": handler.workspace_session_id.0 }),
        ),
        Some(&trace),
    )
    .into_json_value();

    assert_eq!(response["destroyed"], true);
    assert_selected_span_set(
        &trace,
        &[
            "dispatch_operation",
            "destroy_workspace_session::dispatch",
            "WorkspaceSessionService::destroy_session",
        ],
    );
    Ok(())
}

#[test]
fn operation_trace_records_enabled_squash_child_spans() -> Result<(), Box<dyn std::error::Error>> {
    let services = build_services(Arc::new(FakeWorkspaceService::new()));
    let operations = SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service()?,
    );
    let trace = OperationTrace::new_with_enabled_span_keys([
        span_keys::LAYERSTACK_SQUASH_OPEN_STACK,
        span_keys::LAYERSTACK_SQUASH_COMPACT_STACK,
    ]);

    let response = sandbox_runtime::dispatch_operation(
        &operations,
        &Request::new(
            "squash",
            "req-squash-deep",
            CliOperationScope::system(),
            json!({}),
        ),
        Some(&trace),
    )
    .into_json_value();

    assert_eq!(response["squashed"], false);
    assert_span_sequence(
        &trace,
        &[
            ("dispatch_operation", None),
            ("squash::dispatch", Some(0)),
            ("LayerStackService::squash", Some(1)),
            ("layerstack.squash.open_stack", Some(2)),
            ("layerstack.squash.compact_stack", Some(2)),
        ],
    );
    Ok(())
}

fn span<'a>(
    trace: &'a sandbox_runtime::CompletedOperationTrace,
    method_name: &str,
) -> &'a sandbox_runtime::CompletedOperationSpan {
    trace
        .spans
        .iter()
        .find(|span| span.method_name == method_name)
        .expect("span recorded")
}

fn assert_selected_span_set(trace: &OperationTrace, expected: &[&str]) {
    let completed = trace.complete();
    let mut spans = completed.spans.iter().collect::<Vec<_>>();
    spans.sort_by_key(|span| span.call_index);
    let names = spans
        .iter()
        .map(|span| span.method_name)
        .collect::<Vec<_>>();
    assert_eq!(names, expected);
    assert_eq!(spans[0].parent_call_index, None);
    assert_eq!(spans[1].parent_call_index, Some(0));
    assert_eq!(spans[2].parent_call_index, Some(1));
}

fn assert_span_sequence(trace: &OperationTrace, expected: &[(&str, Option<i64>)]) {
    let completed = trace.complete();
    let spans = completed.spans.iter().collect::<Vec<_>>();
    let actual = spans
        .iter()
        .map(|span| (span.method_name, span.parent_call_index))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    for (index, span) in spans.iter().enumerate() {
        assert_eq!(span.call_index, index as i64);
    }
}

fn assert_completed_span_sequence(
    trace: &CompletedOperationTrace,
    expected: &[(&str, Option<i64>)],
) {
    let actual = trace
        .spans
        .iter()
        .map(|span| (span.method_name, span.parent_call_index))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    for (index, span) in trace.spans.iter().enumerate() {
        assert_eq!(span.call_index, index as i64);
    }
}

fn command_operations(
    launch_driver: Arc<FakeLaunchDriver>,
) -> Result<(SandboxRuntimeOperations, String), Box<dyn std::error::Error>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "lease-1",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    )));
    let services = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    let handler = services
        .workspace
        .create_workspace_session(create_request())?;
    Ok((
        SandboxRuntimeOperations::new(
            Arc::clone(&services.command),
            Arc::clone(&services.workspace),
            layerstack_service()?,
        ),
        handler.workspace_session_id.0,
    ))
}

fn one_shot_command_operations(
    launch_driver: Arc<FakeLaunchDriver>,
) -> Result<SandboxRuntimeOperations, Box<dyn std::error::Error>> {
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "workspace-session",
        "lease-1",
        PathBuf::from("/workspace/session"),
        WorkspaceProfile::HostCompatible,
    )));
    let services = build_services_with_launch_driver(Arc::clone(&fake), launch_driver);
    Ok(SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        layerstack_service()?,
    ))
}

fn start_running_command(
    operations: &SandboxRuntimeOperations,
    workspace_session_id: String,
) -> String {
    let response = sandbox_runtime::dispatch_operation(
        operations,
        &Request::new(
            "exec_command",
            "req-setup",
            CliOperationScope::system(),
            json!({
                "workspace_session_id": workspace_session_id,
                "cmd": "cat",
                "yield_time_ms": 0,
            }),
        ),
        None,
    )
    .into_json_value();
    response["command_session_id"]
        .as_str()
        .expect("running command session id")
        .to_owned()
}

fn layerstack_service() -> Result<Arc<LayerStackService>, Box<dyn std::error::Error>> {
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
        "sandbox-runtime-operation-trace-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ))
}
