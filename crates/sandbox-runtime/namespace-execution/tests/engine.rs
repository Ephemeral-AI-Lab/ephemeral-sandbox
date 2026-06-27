//! Behavioral coverage of the engine dispatch and watcher against the fake
//! launcher.

include!("support/namespace_execution_src.rs");

mod support;

use std::sync::Arc;

use sandbox_observability::{NoopHook, SpanStatus};
use sandbox_runtime_namespace_process::runner::protocol::NamespaceRunnerRequest;
use serde_json::json;
use support::{
    run_result, sample_target, ErrShellOp, FakeLauncher, FakeObserver, OkShellOp, PanicShellOp,
    TimedShellOp,
};

fn id(suffix: &str) -> NamespaceExecutionId {
    NamespaceExecutionId(format!("namespace_execution_{suffix}"))
}

fn test_engine(
    fake: &FakeLauncher,
    observer: Arc<FakeObserver>,
    max_active: usize,
) -> NamespaceExecutionEngine {
    NamespaceExecutionEngine::with_launcher(Box::new(fake.clone()), observer, max_active, 30.0)
}

fn assert_request_target_fields(
    request: &NamespaceRunnerRequest,
    target: &NamespaceTarget,
    id: &NamespaceExecutionId,
) {
    assert_eq!(request.request_id, id.0);
    assert_eq!(request.workspace_root, target.workspace_root);
    assert_eq!(request.layer_paths, target.layer_paths);
    assert_eq!(request.upperdir, target.upperdir);
    assert_eq!(request.workdir, target.workdir);
    assert_eq!(request.ns_fds, Some(target.ns_fds));
}

#[test]
fn shell_execution_resolves_finalized_output_and_records_terminal() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);
    let id = id("ok");

    let exec = engine
        .run_shell_interactive(OkShellOp, sample_target(), id.clone(), |_| {}, None)
        .expect("shell admitted");
    assert_eq!(exec.id().0, id.0);

    fake.complete_latest(run_result(0, "ok"));
    assert_eq!(exec.wait().expect("resolved Ok"), 0);

    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, SpanStatus::Completed);
    assert_eq!(exit_code, Some(0));
    assert_eq!(
        observer.events().len(),
        1,
        "only the terminal edge is recorded"
    );
}

#[test]
fn shell_finalize_error_resolves_terminal_error() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);

    let exec = engine
        .run_shell_interactive(
            ErrShellOp,
            sample_target(),
            id("finalize_err"),
            |_| {},
            None,
        )
        .expect("admitted");
    fake.complete_latest(run_result(0, "ok"));

    let error = exec.wait().expect_err("finalize error surfaced");
    assert!(matches!(error, NamespaceExecutionError::Finalize(_)));
    // The terminal hook records the child's own exit (recorded before finalize),
    // so a finalize failure surfaces on the live result, not the span status.
    let (status, _exit) = observer.await_terminal();
    assert_eq!(status, SpanStatus::Completed);
}

#[test]
fn shell_finalize_panic_resolves_terminal_error_and_completes_registry() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);
    let id = id("finalize_panic");

    let exec = engine
        .run_shell_interactive(PanicShellOp, sample_target(), id.clone(), |_| {}, None)
        .expect("admitted");
    fake.complete_latest(run_result(0, "ok"));

    let error = exec.wait().expect_err("panic is mapped to finalize error");
    assert!(matches!(
        error,
        NamespaceExecutionError::Finalize(detail) if detail.contains("panic shell op")
    ));
    assert!(engine.is_completed(&id));
    // The hook fires with the child's exit before the panicking finalize runs, so
    // it records the execution's own outcome; the panic surfaces on the result.
    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, SpanStatus::Completed);
    assert_eq!(exit_code, Some(0));
}

#[test]
fn wait_completion_error_resolves_terminal_error_and_completes_registry() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);
    let id = id("wait_error");

    let exec = engine
        .run_shell_interactive(OkShellOp, sample_target(), id.clone(), |_| {}, None)
        .expect("admitted");
    fake.fail_latest_wait("result fd read failed");

    let error = exec.wait().expect_err("wait error surfaced");
    assert!(matches!(
        error,
        NamespaceExecutionError::Spawn(detail) if detail == "result fd read failed"
    ));
    assert!(engine.is_completed(&id));
    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, SpanStatus::Error);
    assert_eq!(exit_code, None);
}

#[test]
fn cancel_unblocks_the_blocked_watcher() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);

    let exec = engine
        .run_shell_interactive(OkShellOp, sample_target(), id("cancel"), |_| {}, None)
        .expect("admitted");

    // The watcher is blocked in wait_completion; cancel trips the fake completion
    // (a real concurrent unblock), and the promise resolves promptly.
    (exec.cancel_handle())();
    assert_eq!(exec.wait().expect("resolved after cancel"), 130);
    let (status, _exit) = observer.await_terminal();
    assert_eq!(status, SpanStatus::Cancelled);
}

#[test]
fn admission_refuses_when_full_then_readmits_after_completion() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer, 1);

    let first_id = id("1");
    let first = engine
        .run_shell_interactive(OkShellOp, sample_target(), first_id.clone(), |_| {}, None)
        .expect("first admitted");
    let refused = engine
        .run_shell_interactive(OkShellOp, sample_target(), id("2"), |_| {}, None)
        .err()
        .expect("second refused while full");
    assert!(matches!(
        refused,
        NamespaceExecutionError::Admission { max_active: 1 }
    ));

    // complete-before-resolve ⟹ the slot is freed by the time wait() returns.
    fake.complete_latest(run_result(0, "ok"));
    assert_eq!(first.wait().expect("first resolved"), 0);
    assert!(engine.is_completed(&first_id));

    let third = engine
        .run_shell_interactive(OkShellOp, sample_target(), id("3"), |_| {}, None)
        .expect("readmitted after completion");
    fake.complete_latest(run_result(0, "ok"));
    assert_eq!(third.wait().expect("third resolved"), 0);
}

#[test]
fn mount_overlay_execution_resolves_unit_output() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);
    let id = id("mount");

    let handle = engine
        .mount_overlay(sample_target(), id.clone())
        .expect("mount admitted");
    assert_eq!(handle.id().0, id.0);

    fake.complete_latest(run_result(0, "ok"));
    handle.wait().expect("mount resolved");
    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, SpanStatus::Completed);
    assert_eq!(exit_code, Some(0));
}

#[test]
fn shell_request_carries_args_timeout_and_target_fields() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer, 4);
    let target = sample_target();
    let id = id("shell_request");

    let exec = engine
        .run_shell_interactive(TimedShellOp, target.clone(), id.clone(), |_| {}, None)
        .expect("admitted");

    let requests = fake.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_request_target_fields(request, &target, &id);
    assert_eq!(
        request.args,
        json!({ "command": "printf ready", "cwd": "." })
    );
    assert_eq!(request.timeout_seconds, Some(2.5));

    fake.complete_latest(run_result(0, "ok"));
    assert_eq!(exec.wait().expect("resolved"), 0);
}

#[test]
fn mount_overlay_passes_empty_args_to_the_runner_request() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer, 4);
    let target = sample_target();
    let id = id("args");

    let handle = engine
        .mount_overlay(target.clone(), id.clone())
        .expect("admitted");

    let requests = fake.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_request_target_fields(request, &target, &id);
    assert_eq!(request.args, json!({}));
    assert_eq!(request.timeout_seconds, None);
    fake.complete_latest(run_result(0, "ok"));
    handle.wait().expect("resolved");
}

#[test]
fn mount_overlay_nonzero_exit_is_terminal_error() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);

    let handle = engine
        .mount_overlay(sample_target(), id("nonzero"))
        .expect("admitted");

    fake.complete_latest(
        sandbox_runtime_namespace_process::runner::protocol::RunResult {
            exit_code: 1,
            payload: json!({"error": "mount exploded", "status": "ok"}),
        },
    );

    let error = handle
        .wait()
        .expect_err("nonzero mount exit is terminal error");
    assert!(matches!(
        error,
        NamespaceExecutionError::Finalize(detail)
            if detail.contains("--mount-overlay") && detail.contains("mount exploded")
    ));
    // The hook records the runner's reported outcome (payload `status: ok`) at
    // child-exit; the nonzero-exit mount failure surfaces on the live result.
    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, SpanStatus::Completed);
    assert_eq!(exit_code, Some(1));
}

#[test]
fn mount_overlay_passes_setup_timeout_to_launcher() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine: NamespaceExecutionEngine =
        NamespaceExecutionEngine::with_launcher(Box::new(fake.clone()), observer, 4, 12.5);

    let handle = engine
        .mount_overlay(sample_target(), id("timeout"))
        .expect("admitted");

    assert_eq!(fake.overlay_mount_setup_timeouts(), vec![12.5]);
    fake.complete_latest(run_result(0, "ok"));
    handle.wait().expect("resolved");
}

#[test]
fn engine_allocates_monotonic_namespace_execution_ids() {
    let engine: NamespaceExecutionEngine =
        NamespaceExecutionEngine::new(Arc::new(NoopHook), 4, 30.0);

    assert_eq!(engine.allocate_id().0, "namespace_execution_1");
    assert_eq!(engine.allocate_id().0, "namespace_execution_2");
}

#[test]
fn namespace_execution_id_is_the_runner_request_id() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer, 4);
    let id = id("42");

    let exec = engine
        .run_shell_interactive(OkShellOp, sample_target(), id.clone(), |_| {}, None)
        .expect("admitted");
    assert_eq!(exec.id().0, id.0);
    assert_eq!(fake.recorded_request_ids(), vec![id.0.clone()]);

    fake.complete_latest(run_result(0, "ok"));
    let _ = exec.wait();
}
