//! Phase A span/trace integration: an enabled `Observer` writes one
//! `observability.ndjson`, and these tests read it back to assert the parent
//! waterfall, lease/publish records, and the never-fail guarantees.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sandbox_observability::record::{names, proc};
use sandbox_observability::{
    Observer, ObserverConfig, RawFilter, Reader, Sink, SpanStatus, TraceContext,
};
use sandbox_protocol::{CliOperationScope, Request};
use sandbox_runtime::command::{CommandStatus, ExecCommandInput};
use sandbox_runtime::SandboxRuntimeOperations;
use sandbox_runtime_workspace::{
    BaseRevision, CapturedWorkspaceChanges, NetworkProfile, WorkspaceSessionId,
};
use serde_json::Value;

use support::{
    build_observed_services, build_services, create_request, observed_layerstack_service,
    success_exit, workspace_handle, FakeLaunchDriver, FakeWorkspaceService, ScriptedCommandYield,
    TestServices,
};

fn one_shot_await() -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: None,
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(250),
    }
}

fn session_await(workspace_session_id: WorkspaceSessionId) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: Some(workspace_session_id),
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(250),
    }
}

fn parked_exec_input(workspace_session_id: WorkspaceSessionId) -> ExecCommandInput {
    ExecCommandInput {
        workspace_session_id: Some(workspace_session_id),
        cmd: "printf ok".to_owned(),
        timeout_ms: None,
        yield_time_ms: Some(0),
    }
}

#[test]
fn case_a_one_shot_exec_writes_parent_waterfall() {
    let log = TempLog::new("case-a");
    let obs = enabled_observer(&log.path);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "one-shot-session",
        "lease-1",
        PathBuf::from("/workspace/one-shot"),
        NetworkProfile::Shared,
    )));
    fake.push_capture_result(Ok(matching_capture("one-shot-session")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let services = build_observed_services(Arc::clone(&fake), launch_driver, obs.clone());

    obs.with_context(req("req-A"), || {
        let dispatch = obs.span(names::DAEMON_DISPATCH);
        dispatch.attr("op", "exec_command");
        let output = services
            .command
            .exec_command(one_shot_await())
            .expect("one-shot command completes");
        assert_eq!(output.status, CommandStatus::Ok);
    });

    let records = read_records(&log.path);

    let dispatch = span(&records, names::DAEMON_DISPATCH);
    assert_eq!(parent(dispatch), None, "dispatch is the trace root");
    assert_eq!(dispatch["attrs"]["op"], "exec_command");

    let exec = span(&records, names::COMMAND_EXEC);
    assert_eq!(parent(exec), Some(id(dispatch)));
    assert_eq!(exec["attrs"]["one_shot"], true);

    let create = span(&records, names::WORKSPACE_SESSION_CREATE);
    assert_eq!(parent(create), Some(id(exec)));

    let acquired = event(&records, names::LEASE_ACQUIRED);
    assert_eq!(
        parent(acquired),
        Some(id(create)),
        "lease.acquired nests under create"
    );

    let run_shell = span(&records, names::NAMESPACE_EXEC_RUN_SHELL);
    assert_eq!(
        parent(run_shell),
        Some(id(exec)),
        "the async shell span is a sibling of the finalize tail under command.exec"
    );
    assert!(
        run_shell["attrs"].get("exit_code").is_some(),
        "the shell span folds the child exit code"
    );

    let capture = span(&records, names::WORKSPACE_SESSION_CAPTURE_CHANGES);
    assert_eq!(parent(capture), Some(id(exec)));
    let publish = span(&records, names::LAYERSTACK_PUBLISH);
    assert_eq!(parent(publish), Some(id(exec)));
    let destroy = span(&records, names::WORKSPACE_SESSION_DESTROY);
    assert_eq!(parent(destroy), Some(id(exec)));

    let released = event(&records, names::LEASE_RELEASED);
    assert_eq!(
        parent(released),
        Some(id(destroy)),
        "lease.released nests under destroy"
    );

    // The raw filters that back the lease stream and publish audit views.
    assert_eq!(raw(&log.path, "event", names::LEASE_ACQUIRED).len(), 1);
    assert_eq!(raw(&log.path, "event", names::LEASE_RELEASED).len(), 1);
    assert_eq!(raw(&log.path, "span", names::LAYERSTACK_PUBLISH).len(), 1);
}

#[test]
fn persistent_session_exec_writes_only_command_exec_and_run_shell() {
    let log = TempLog::new("persistent");
    let obs = enabled_observer(&log.path);
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let services = build_observed_services(Arc::clone(&fake), launch_driver, obs.clone());

    // Create the caller-owned session outside any trace context, so its own
    // create span/lease event no-op and only the exec trace is recorded.
    let session = create_detached_session(&fake, &services, "persistent-session");

    obs.with_context(req("req-P"), || {
        let dispatch = obs.span(names::DAEMON_DISPATCH);
        dispatch.attr("op", "exec_command");
        let output = services
            .command
            .exec_command(session_await(session))
            .expect("session command completes");
        assert_eq!(output.status, CommandStatus::Ok);
    });

    let records = read_records(&log.path);
    let exec = span(&records, names::COMMAND_EXEC);
    assert_eq!(exec["attrs"]["one_shot"], false);
    let run_shell = span(&records, names::NAMESPACE_EXEC_RUN_SHELL);
    assert_eq!(parent(run_shell), Some(id(exec)));

    for absent in [
        names::WORKSPACE_SESSION_CREATE,
        names::WORKSPACE_SESSION_CAPTURE_CHANGES,
        names::LAYERSTACK_PUBLISH,
        names::WORKSPACE_SESSION_DESTROY,
    ] {
        assert!(
            find_span(&records, absent).is_none(),
            "{absent} must not appear for a persistent-session exec"
        );
    }
    assert!(find_event(&records, names::LEASE_ACQUIRED).is_none());
    assert!(find_event(&records, names::LEASE_RELEASED).is_none());
}

#[test]
fn live_in_flight_command_has_no_run_shell_record_yet() {
    let log = TempLog::new("in-flight");
    let obs = enabled_observer(&log.path);
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    // A spawn that parks: the command runs but never reaches a terminal edge.
    launch_driver.push_outcome(ScriptedCommandYield::Running("partial".to_owned()));
    let services = build_observed_services(Arc::clone(&fake), launch_driver, obs.clone());
    let session = create_detached_session(&fake, &services, "live-session");

    let output = obs.with_context(req("req-L"), || {
        let _dispatch = obs.span(names::DAEMON_DISPATCH);
        services
            .command
            .exec_command(parked_exec_input(session))
            .expect("running command yields")
    });
    assert_eq!(output.status, CommandStatus::Running);

    // Read while the registry is still alive: the parked span has not been
    // recorded, while the live registry reports the active execution.
    let records = read_records(&log.path);
    assert!(
        find_span(&records, names::NAMESPACE_EXEC_RUN_SHELL).is_none(),
        "an in-flight shell writes no completed span yet"
    );
    assert_eq!(services.command.active_namespace_executions().len(), 1);
}

#[test]
fn launch_failure_writes_no_run_shell_span() {
    let log = TempLog::new("launch-fail");
    let obs = enabled_observer(&log.path);
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "launch-fail-session",
        "lease-1",
        PathBuf::from("/workspace/launch-fail"),
        NetworkProfile::Shared,
    )));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_spawn_error(
        sandbox_runtime::command::CommandServiceError::InvalidCommand {
            message: "scripted spawn failure".to_owned(),
        },
    );
    let services = build_observed_services(Arc::clone(&fake), launch_driver, obs.clone());

    obs.with_context(req("req-F"), || {
        let _dispatch = obs.span(names::DAEMON_DISPATCH);
        services
            .command
            .exec_command(one_shot_await())
            .expect_err("spawn failure surfaces");
    });

    let records = read_records(&log.path);
    assert!(
        find_span(&records, names::NAMESPACE_EXEC_RUN_SHELL).is_none(),
        "a launch that fails before the watcher writes no shell span (cancelled, not parked)"
    );
    // The command.exec scope still records, marked error by the failed body.
    let exec = span(&records, names::COMMAND_EXEC);
    assert_eq!(exec["status"], "error");
}

#[test]
fn fault_response_marks_daemon_dispatch_error() {
    let log = TempLog::new("fault");
    let obs = enabled_observer(&log.path);
    let operations = disabled_operations();
    let request = Request::new(
        "nonexistent_op",
        "req-fault",
        CliOperationScope::system(),
        serde_json::json!({}),
    );

    obs.with_context(req("req-fault"), || {
        let dispatch = obs.span(names::DAEMON_DISPATCH);
        dispatch.attr("op", request.op.clone());
        let json = sandbox_runtime::dispatch_operation(&operations, &request).into_json_value();
        if json.get("error").is_some() {
            dispatch.status(SpanStatus::Error);
        }
    });

    let records = read_records(&log.path);
    let dispatch = span(&records, names::DAEMON_DISPATCH);
    assert_eq!(
        dispatch["status"], "error",
        "a fault Response flips the dispatch root to error"
    );
}

#[test]
fn disabled_observability_runs_identically_and_writes_nothing() {
    let log = TempLog::new("disabled");
    let obs = Observer::disabled();
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "disabled-session",
        "lease-1",
        PathBuf::from("/workspace/disabled"),
        NetworkProfile::Shared,
    )));
    fake.push_capture_result(Ok(matching_capture("disabled-session")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let services = build_observed_services(Arc::clone(&fake), launch_driver, obs.clone());

    let output = obs.with_context(req("req-D"), || {
        let _dispatch = obs.span(names::DAEMON_DISPATCH);
        services
            .command
            .exec_command(one_shot_await())
            .expect("one-shot command completes with observability disabled")
    });

    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("disabled-session".to_owned())],
        "the one-shot still tears down with observability disabled"
    );
    assert!(!log.path.exists(), "disabled observability writes no log");
}

#[test]
fn sink_error_does_not_surface_to_command_result() {
    let log = TempLog::new("sink-error");
    // Point the sink at a path whose parent is a regular file, so every append
    // fails. The emit path must swallow it and leave the command unaffected.
    std::fs::create_dir_all(log.path.parent().expect("log parent")).expect("create log dir");
    std::fs::write(&log.path, b"not a directory").expect("seed blocking file");
    let wedged = log.path.join("observability.ndjson");
    let obs = Observer::new(
        ObserverConfig {
            proc: proc::DAEMON,
            enabled: true,
        },
        Sink::new(wedged),
    );
    let fake = Arc::new(FakeWorkspaceService::new());
    fake.push_create_result(Ok(workspace_handle(
        "sink-session",
        "lease-1",
        PathBuf::from("/workspace/sink"),
        NetworkProfile::Shared,
    )));
    fake.push_capture_result(Ok(matching_capture("sink-session")));
    let launch_driver = Arc::new(FakeLaunchDriver::new());
    launch_driver.push_outcome(ScriptedCommandYield::Completed(success_exit("done\n")));
    let services = build_observed_services(Arc::clone(&fake), launch_driver, obs.clone());

    let output = obs.with_context(req("req-S"), || {
        let _dispatch = obs.span(names::DAEMON_DISPATCH);
        services
            .command
            .exec_command(one_shot_await())
            .expect("a sink error never fails the command")
    });

    assert_eq!(output.status, CommandStatus::Ok);
    assert_eq!(
        fake.destroy_calls(),
        vec![WorkspaceSessionId("sink-session".to_owned())]
    );
}

fn req(trace: &str) -> TraceContext {
    TraceContext {
        trace: Arc::from(trace),
        parent: None,
    }
}

fn enabled_observer(path: &Path) -> Observer {
    Observer::new(
        ObserverConfig {
            proc: proc::DAEMON,
            enabled: true,
        },
        Sink::new(path.to_path_buf()),
    )
}

/// Build the operations graph over a disabled observer, for the dispatch-root
/// test whose fault path never reaches an emitting service.
fn disabled_operations() -> SandboxRuntimeOperations {
    let fake = Arc::new(FakeWorkspaceService::new());
    let services: TestServices = build_services(fake);
    SandboxRuntimeOperations::new(
        Arc::clone(&services.command),
        Arc::clone(&services.workspace),
        observed_layerstack_service(Observer::disabled()),
    )
}

fn create_detached_session(
    fake: &Arc<FakeWorkspaceService>,
    services: &TestServices,
    workspace_session_id: &str,
) -> WorkspaceSessionId {
    fake.push_create_result(Ok(workspace_handle(
        workspace_session_id,
        "lease-1",
        PathBuf::from("/workspace/session"),
        NetworkProfile::Shared,
    )));
    services
        .workspace
        .create_workspace_session(create_request())
        .expect("session create succeeds")
        .workspace_session_id
}

/// A capture result whose declared base revision matches its base manifest, so
/// the operation-level `expected_base` check passes and the publish span fires.
fn matching_capture(workspace_session_id: &str) -> CapturedWorkspaceChanges {
    let base_manifest = support::test_manifest();
    let base_revision = BaseRevision {
        version: base_manifest.version,
        root_hash: sandbox_runtime_layerstack::manifest_root_hash(&base_manifest),
        layer_count: base_manifest.layers.len(),
    };
    CapturedWorkspaceChanges {
        workspace_session_id: WorkspaceSessionId(workspace_session_id.to_owned()),
        base_revision,
        base_manifest,
        changed_paths: Vec::new(),
        changed_path_kinds: BTreeMap::new(),
        protected_drops: Vec::new(),
        stats: None,
        changes: Vec::new(),
        metadata_path_count: 0,
    }
}

fn read_records(path: &Path) -> Vec<Value> {
    let reader = Reader::new(path.to_path_buf(), path.with_extension("ndjson.absent"));
    reader
        .raw(RawFilter::default())
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn raw(path: &Path, kind: &str, name: &str) -> Vec<String> {
    let reader = Reader::new(path.to_path_buf(), path.with_extension("ndjson.absent"));
    reader.raw(RawFilter {
        kind: Some(kind.to_owned()),
        name: Some(name.to_owned()),
        ..RawFilter::default()
    })
}

fn find_span<'a>(records: &'a [Value], name: &str) -> Option<&'a Value> {
    records
        .iter()
        .find(|record| record["kind"] == "span" && record["name"] == name)
}

fn find_event<'a>(records: &'a [Value], name: &str) -> Option<&'a Value> {
    records
        .iter()
        .find(|record| record["kind"] == "event" && record["name"] == name)
}

fn span<'a>(records: &'a [Value], name: &str) -> &'a Value {
    find_span(records, name).unwrap_or_else(|| panic!("missing span {name}"))
}

fn event<'a>(records: &'a [Value], name: &str) -> &'a Value {
    find_event(records, name).unwrap_or_else(|| panic!("missing event {name}"))
}

fn id(record: &Value) -> &str {
    record["span"].as_str().expect("span id")
}

fn parent(record: &Value) -> Option<&str> {
    record.get("parent").and_then(Value::as_str)
}

struct TempLog {
    path: PathBuf,
}

impl TempLog {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sandbox-obs-trace-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self {
            path: dir.join("observability.ndjson"),
        }
    }
}
