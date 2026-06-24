//! Behavioral coverage of the engine dispatch + watcher against the fake
//! launcher — the authoritative Phase 2 signal (runs on darwin; no real fork).

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use sandbox_runtime_namespace_execution::{
    NamespaceExecutionEngine, NamespaceExecutionError, NamespaceExecutionId,
    NamespaceExecutionTerminalStatus, NamespaceTarget, NoopObserver,
};
use sandbox_runtime_namespace_process::runner::protocol::NamespaceRunnerRequest;
use serde_json::json;
use support::{
    run_result, sample_target, ErrShellOp, FakeLauncher, FakeObserver, ObserverEvent, OkShellOp,
    PanicShellOp, TimedShellOp,
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
        .run_shell_interactive(OkShellOp, sample_target(), id.clone())
        .expect("shell admitted");
    assert_eq!(exec.id().0, id.0);

    fake.complete_latest(run_result(0, "ok"));
    assert_eq!(exec.wait().expect("resolved Ok"), 0);

    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, NamespaceExecutionTerminalStatus::Ok);
    assert_eq!(exit_code, Some(0));
    assert_eq!(observer.events().first(), Some(&ObserverEvent::Running(id)));
}

#[test]
fn shell_finalize_error_resolves_terminal_error() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);

    let exec = engine
        .run_shell_interactive(ErrShellOp, sample_target(), id("finalize_err"))
        .expect("admitted");
    fake.complete_latest(run_result(0, "ok"));

    let error = exec.wait().expect_err("finalize error surfaced");
    assert!(matches!(error, NamespaceExecutionError::Finalize(_)));
    let (status, _exit) = observer.await_terminal();
    assert_eq!(status, NamespaceExecutionTerminalStatus::Error);
}

#[test]
fn shell_finalize_panic_resolves_terminal_error_and_completes_registry() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);
    let id = id("finalize_panic");

    let exec = engine
        .run_shell_interactive(PanicShellOp, sample_target(), id.clone())
        .expect("admitted");
    fake.complete_latest(run_result(0, "ok"));

    let error = exec.wait().expect_err("panic is mapped to finalize error");
    assert!(matches!(
        error,
        NamespaceExecutionError::Finalize(detail) if detail.contains("panic shell op")
    ));
    assert!(engine.registry_is_completed(&id));
    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, NamespaceExecutionTerminalStatus::Error);
    assert_eq!(exit_code, Some(0));
}

#[test]
fn wait_completion_error_resolves_terminal_error_and_completes_registry() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);
    let id = id("wait_error");

    let exec = engine
        .run_shell_interactive(OkShellOp, sample_target(), id.clone())
        .expect("admitted");
    fake.fail_latest_wait("result fd read failed");

    let error = exec.wait().expect_err("wait error surfaced");
    assert!(matches!(
        error,
        NamespaceExecutionError::Spawn(detail) if detail == "result fd read failed"
    ));
    assert!(engine.registry_is_completed(&id));
    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, NamespaceExecutionTerminalStatus::Error);
    assert_eq!(exit_code, None);
}

#[test]
fn cancel_unblocks_the_blocked_watcher() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);

    let exec = engine
        .run_shell_interactive(OkShellOp, sample_target(), id("cancel"))
        .expect("admitted");

    // The watcher is blocked in wait_completion; cancel trips the fake completion
    // (a real concurrent unblock), and the promise resolves promptly.
    exec.cancel();
    assert_eq!(exec.wait().expect("resolved after cancel"), 130);
    let (status, _exit) = observer.await_terminal();
    assert_eq!(status, NamespaceExecutionTerminalStatus::Cancelled);
}

#[test]
fn admission_refuses_when_full_then_readmits_after_completion() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer, 1);

    let first_id = id("1");
    let first = engine
        .run_shell_interactive(OkShellOp, sample_target(), first_id.clone())
        .expect("first admitted");
    let refused = engine
        .run_shell_interactive(OkShellOp, sample_target(), id("2"))
        .err()
        .expect("second refused while full");
    assert!(matches!(
        refused,
        NamespaceExecutionError::Admission { max_active: 1 }
    ));

    // complete-before-resolve ⟹ the slot is freed by the time wait() returns.
    fake.complete_latest(run_result(0, "ok"));
    assert_eq!(first.wait().expect("first resolved"), 0);
    assert!(engine.registry_is_completed(&first_id));

    let third = engine
        .run_shell_interactive(OkShellOp, sample_target(), id("3"))
        .expect("readmitted after completion");
    fake.complete_latest(run_result(0, "ok"));
    assert_eq!(third.wait().expect("third resolved"), 0);
}

#[test]
fn mount_execution_resolves_parsed_output() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);
    let id = id("mount");

    let handle = engine
        .run_mount(
            "--mount-overlay",
            sample_target(),
            id.clone(),
            json!({}),
            |outcome| Ok(outcome.exit_code()),
        )
        .expect("mount admitted");
    assert_eq!(handle.id().0, id.0);
    assert_eq!(fake.recorded_piped_mode_flags(), vec!["--mount-overlay"]);

    fake.complete_latest(run_result(0, "ok"));
    assert_eq!(handle.wait().expect("mount resolved"), 0);
    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, NamespaceExecutionTerminalStatus::Ok);
    assert_eq!(exit_code, Some(0));
}

#[test]
fn mount_parse_error_resolves_terminal_error() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);

    let handle = engine
        .run_mount(
            "--remount-overlay",
            sample_target(),
            id("mount_err"),
            json!({}),
            |_outcome| Err::<i64, _>(NamespaceExecutionError::Finalize("bad probe".to_owned())),
        )
        .expect("admitted");
    assert_eq!(fake.recorded_piped_mode_flags(), vec!["--remount-overlay"]);
    fake.complete_latest(run_result(0, "ok"));

    let error = handle.wait().expect_err("parse error surfaced");
    assert!(matches!(error, NamespaceExecutionError::Finalize(_)));
    let (status, _exit) = observer.await_terminal();
    assert_eq!(status, NamespaceExecutionTerminalStatus::Error);
}

#[test]
fn mount_parse_panic_resolves_terminal_error_and_completes_registry() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);
    let id = id("mount_panic");

    let handle = engine
        .run_mount(
            "--mount-overlay",
            sample_target(),
            id.clone(),
            json!({}),
            |_outcome| -> Result<i64, NamespaceExecutionError> { panic!("panic mount parse") },
        )
        .expect("admitted");
    fake.complete_latest(run_result(0, "ok"));

    let error = handle
        .wait()
        .expect_err("panic is mapped to finalize error");
    assert!(matches!(
        error,
        NamespaceExecutionError::Finalize(detail) if detail.contains("panic mount parse")
    ));
    assert!(engine.registry_is_completed(&id));
    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, NamespaceExecutionTerminalStatus::Error);
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
        .run_shell_interactive(TimedShellOp, target.clone(), id.clone())
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
fn run_mount_passes_args_to_the_runner_request() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer, 4);
    let target = sample_target();
    let id = id("args");
    let args = json!({
        "probe_path": "/tmp/remount-probe",
        "probe_content": "expected",
    });

    let handle = engine
        .run_mount(
            "--remount-overlay",
            target.clone(),
            id.clone(),
            args.clone(),
            |_| Ok(()),
        )
        .expect("admitted");

    let requests = fake.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_request_target_fields(request, &target, &id);
    assert_eq!(request.args, args);
    assert_eq!(request.timeout_seconds, None);
    assert_eq!(fake.recorded_piped_mode_flags(), vec!["--remount-overlay"]);
    fake.complete_latest(run_result(0, "ok"));
    handle.wait().expect("resolved");
}

#[test]
fn run_mount_short_circuits_nonzero_exit_before_parse() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = test_engine(&fake, observer.clone(), 4);
    let parse_called = Arc::new(AtomicBool::new(false));
    let parse_called_in_closure = Arc::clone(&parse_called);

    let handle = engine
        .run_mount(
            "--mount-overlay",
            sample_target(),
            id("nonzero"),
            json!({}),
            move |_| {
                parse_called_in_closure.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
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
    assert!(!parse_called.load(Ordering::SeqCst));
    let (status, exit_code) = observer.await_terminal();
    assert_eq!(status, NamespaceExecutionTerminalStatus::Error);
    assert_eq!(exit_code, Some(1));
}

#[test]
fn run_mount_passes_setup_timeout_to_piped_launcher() {
    let fake = FakeLauncher::new();
    let observer = Arc::new(FakeObserver::new());
    let engine = NamespaceExecutionEngine::with_launcher(Box::new(fake.clone()), observer, 4, 12.5);

    let handle = engine
        .run_mount(
            "--mount-overlay",
            sample_target(),
            id("timeout"),
            json!({}),
            |_| Ok(()),
        )
        .expect("admitted");

    assert_eq!(fake.piped_setup_timeouts(), vec![12.5]);
    fake.complete_latest(run_result(0, "ok"));
    handle.wait().expect("resolved");
}

#[test]
fn engine_allocates_monotonic_namespace_execution_ids() {
    let engine = NamespaceExecutionEngine::new(Arc::new(NoopObserver), 4, 30.0);

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
        .run_shell_interactive(OkShellOp, sample_target(), id.clone())
        .expect("admitted");
    assert_eq!(exec.id().0, id.0);
    assert_eq!(fake.recorded_request_ids(), vec![id.0.clone()]);

    fake.complete_latest(run_result(0, "ok"));
    let _ = exec.wait();
}
