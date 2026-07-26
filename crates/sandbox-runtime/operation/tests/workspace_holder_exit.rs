use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::fs::{flock, FlockOperation};
use sandbox_observability_telemetry::record::proc;
use sandbox_observability_telemetry::{Observer, ObserverConfig, Sink, TraceContext};
use sandbox_runtime::command::ExecCommandInput;
use sandbox_runtime::workspace_session::{
    FinalizationState, FinalizePolicy, HolderExitDispatcher, HolderExitDisposition,
    HolderLifecycleEventKind, WorkspaceSessionError, WorkspaceSessionService,
};
use sandbox_runtime_workspace::{CapturedWorkspaceChanges, NetworkProfile, WorkspaceSessionId};

mod support;
use support::FakeWorkspaceService;

const DISPATCH_CLEANUP_ATTEMPTS: usize = 4;

struct LockedObserverSink {
    observer: Observer,
    lock: Option<File>,
    root: PathBuf,
}

impl LockedObserverSink {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "workspace-holder-exit-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create observer barrier directory");
        let log_path = root.join("observability.ndjson");
        let lock_path = PathBuf::from(format!("{}.lock", log_path.display()));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .expect("open observer sink lock");
        flock(&lock, FlockOperation::LockExclusive).expect("lock observer sink");
        Self {
            observer: Observer::new(
                ObserverConfig {
                    proc: proc::DAEMON,
                    enabled: true,
                },
                Sink::new(log_path, sandbox_observability_telemetry::MAX_LINE_BYTES),
            ),
            lock: Some(lock),
            root,
        }
    }

    fn observer(&self) -> Observer {
        self.observer.clone()
    }

    fn release(&mut self) {
        if let Some(lock) = self.lock.take() {
            flock(&lock, FlockOperation::Unlock).expect("unlock observer sink");
        }
    }
}

impl Drop for LockedObserverSink {
    fn drop(&mut self) {
        self.release();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn test_trace(label: &str) -> TraceContext {
    TraceContext {
        trace: Arc::from(format!("trace-{label}")),
        parent: None,
    }
}

fn manager_with(fake: &Arc<FakeWorkspaceService>) -> WorkspaceSessionService {
    manager_with_observer(fake, Observer::disabled())
}

fn manager_with_observer(
    fake: &Arc<FakeWorkspaceService>,
    observer: Observer,
) -> WorkspaceSessionService {
    WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(fake)),
        support::observed_layerstack_service(Observer::disabled()),
        observer,
    )
}

fn create(
    manager: &WorkspaceSessionService,
    fake: &Arc<FakeWorkspaceService>,
    id: &str,
    policy: FinalizePolicy,
) -> sandbox_runtime::workspace_session::WorkspaceSessionHandler {
    fake.push_create_result(Ok(support::workspace_handle(
        id,
        &format!("lease-{id}"),
        PathBuf::from("/workspace"),
        NetworkProfile::Shared,
    )));
    manager
        .create_workspace_session(support::create_request_with_policy(policy))
        .expect("session creates")
}

fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while !predicate() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(predicate(), "condition did not converge within {timeout:?}");
}

fn session_is_absent(
    manager: &WorkspaceSessionService,
    workspace_session_id: &WorkspaceSessionId,
) -> bool {
    matches!(
        manager.resolve_session(workspace_session_id.clone()),
        Err(WorkspaceSessionError::NotFound { .. })
    )
}

fn holder_cleanup_failed(
    manager: &WorkspaceSessionService,
    workspace_session_id: &WorkspaceSessionId,
) -> bool {
    matches!(
        manager.resolve_session(workspace_session_id.clone()),
        Err(WorkspaceSessionError::HolderExited {
            cleanup_state: FinalizationState::FinalizeFailed,
            ..
        })
    )
}

#[test]
fn create_rejects_a_holder_that_exits_during_post_insert_reconcile() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = manager_with(&fake);
    let workspace_session_id = WorkspaceSessionId("create-exit-race".to_owned());
    let handle = support::workspace_handle(
        &workspace_session_id.0,
        "lease-create-exit-race",
        PathBuf::from("/workspace"),
        NetworkProfile::Shared,
    );
    fake.push_create_result(Ok(handle.clone()));
    fake.exit_holder_on_next_liveness_check(&handle, "signal:9");

    let result =
        manager.create_workspace_session(support::create_request_with_policy(FinalizePolicy::NoOp));

    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);
    assert!(session_is_absent(&manager, &workspace_session_id));
    let error =
        result.expect_err("create must not return a handler removed by post-insert reconciliation");

    assert!(matches!(
        error,
        WorkspaceSessionError::HolderExited {
            workspace_session_id: failed_id,
            reason,
            ..
        } if failed_id == workspace_session_id && reason == "signal:9"
    ));
}

#[test]
fn dispatcher_finishes_post_insert_teardown_before_create_commit() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let mut sink_barrier = LockedObserverSink::new("dispatcher-wins-create-commit");
    let observer = sink_barrier.observer();
    let manager = Arc::new(manager_with_observer(&fake, observer.clone()));
    let dispatcher = HolderExitDispatcher::start(&manager)
        .expect("dispatcher starts")
        .expect("fake runtime exposes supervision");
    let workspace_session_id = WorkspaceSessionId("create-dispatcher-wins".to_owned());
    let handle = support::workspace_handle(
        &workspace_session_id.0,
        "lease-create-dispatcher-wins",
        PathBuf::from("/workspace"),
        NetworkProfile::Shared,
    );
    fake.push_create_result(Ok(handle.clone()));

    let create_manager = Arc::clone(&manager);
    let create_observer = observer.clone();
    let create = std::thread::spawn(move || {
        create_observer.with_context(test_trace("dispatcher-wins-create"), || {
            create_manager
                .create_workspace_session(support::create_request_with_policy(FinalizePolicy::NoOp))
        })
    });
    wait_until(Duration::from_secs(1), || {
        manager
            .resolve_session(workspace_session_id.clone())
            .is_ok()
    });

    let (destroy_entered, release_destroy) = fake.park_next_destroy();
    fake.mark_holder_exited(&handle, "signal:9");
    fake.notify_holder_exit();
    destroy_entered
        .recv_timeout(Duration::from_secs(1))
        .expect("dispatcher owns the post-insert teardown");
    release_destroy
        .send(())
        .expect("release dispatcher teardown");
    wait_until(Duration::from_secs(1), || {
        session_is_absent(&manager, &workspace_session_id)
    });
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);

    sink_barrier.release();
    let error = create
        .join()
        .expect("create thread joins")
        .expect_err("create must reject the handler destroyed before commit");
    assert!(matches!(
        error,
        WorkspaceSessionError::HolderExited {
            workspace_session_id: failed_id,
            reason,
            ..
        } if failed_id == workspace_session_id && reason == "signal:9"
    ));
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id]);
    dispatcher.shutdown_and_join();
}

#[test]
fn dispatcher_cleanup_failure_is_returned_without_post_insert_retry() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let mut sink_barrier = LockedObserverSink::new("dispatcher-failure-create-commit");
    let observer = sink_barrier.observer();
    let manager = Arc::new(manager_with_observer(&fake, observer.clone()));
    let dispatcher = HolderExitDispatcher::start(&manager)
        .expect("dispatcher starts")
        .expect("fake runtime exposes supervision");
    let workspace_session_id = WorkspaceSessionId("create-dispatcher-failure".to_owned());
    let handle = support::workspace_handle(
        &workspace_session_id.0,
        "lease-create-dispatcher-failure",
        PathBuf::from("/workspace"),
        NetworkProfile::Shared,
    );
    fake.push_create_result(Ok(handle.clone()));
    fake.push_destroy_result(Err(sandbox_runtime_workspace::WorkspaceError::Cleanup {
        workspace_session_id: workspace_session_id.0.clone(),
        failures: vec!["Mounts: injected post-insert cleanup failure".to_owned()],
    }));

    let create_manager = Arc::clone(&manager);
    let create_observer = observer.clone();
    let create = std::thread::spawn(move || {
        create_observer.with_context(test_trace("dispatcher-failure-create"), || {
            create_manager
                .create_workspace_session(support::create_request_with_policy(FinalizePolicy::NoOp))
        })
    });
    wait_until(Duration::from_secs(1), || {
        manager
            .resolve_session(workspace_session_id.clone())
            .is_ok()
    });

    let (destroy_entered, release_destroy) = fake.park_next_destroy();
    fake.mark_holder_exited(&handle, "signal:9");
    fake.notify_holder_exit();
    destroy_entered
        .recv_timeout(Duration::from_secs(1))
        .expect("dispatcher owns the failing teardown");
    let shutdown_dispatcher = Arc::clone(&dispatcher);
    let shutdown = std::thread::spawn(move || shutdown_dispatcher.shutdown_and_join());
    release_destroy.send(()).expect("release failing teardown");
    shutdown.join().expect("dispatcher shutdown joins");
    wait_until(Duration::from_secs(1), || {
        holder_cleanup_failed(&manager, &workspace_session_id)
    });
    assert_eq!(fake.destroy_calls(), vec![workspace_session_id.clone()]);

    sink_barrier.release();
    let error = create
        .join()
        .expect("create thread joins")
        .expect_err("create returns the retained cleanup failure");
    assert!(matches!(
        error,
        WorkspaceSessionError::HolderExited {
            workspace_session_id: failed_id,
            cleanup_state: FinalizationState::FinalizeFailed,
            ..
        } if failed_id == workspace_session_id
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![workspace_session_id.clone()],
        "create commit must not start a second cleanup attempt"
    );
    assert!(holder_cleanup_failed(&manager, &workspace_session_id));

    let retried = manager.reconcile_holder_exits();
    assert!(matches!(
        retried.as_slice(),
        [outcome] if outcome.disposition == HolderExitDisposition::Destroyed
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![workspace_session_id.clone(), workspace_session_id.clone()]
    );
    assert!(session_is_absent(&manager, &workspace_session_id));
}

#[test]
fn holder_exit_dispatches_cleanup_without_a_followup_request_or_snapshot() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = Arc::new(manager_with(&fake));
    let dispatcher = HolderExitDispatcher::start(&manager)
        .expect("dispatcher starts")
        .expect("fake runtime exposes supervision");
    let failed = create(&manager, &fake, "autonomous", FinalizePolicy::NoOp);

    fake.mark_holder_exited(&failed.handle, "signal:9");
    fake.notify_holder_exit();

    wait_until(Duration::from_secs(1), || {
        fake.destroy_calls() == vec![failed.workspace_session_id.clone()]
    });
    assert!(session_is_absent(&manager, &failed.workspace_session_id));
    dispatcher.shutdown_and_join();
    dispatcher.shutdown_and_join();
}

#[test]
fn dispatched_exit_and_explicit_destroy_join_one_runtime_teardown() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = Arc::new(manager_with(&fake));
    let dispatcher = HolderExitDispatcher::start(&manager)
        .expect("dispatcher starts")
        .expect("fake runtime exposes supervision");
    let failed = create(&manager, &fake, "dispatch-race", FinalizePolicy::NoOp);
    let (entered, release) = fake.park_next_destroy();

    fake.mark_holder_exited(&failed.handle, "signal:9");
    fake.notify_holder_exit();
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("dispatcher enters raw teardown");

    let explicit_manager = Arc::clone(&manager);
    let explicit_handler = failed.clone();
    let explicit = std::thread::spawn(move || {
        explicit_manager.destroy_session(explicit_handler, Default::default())
    });
    std::thread::sleep(Duration::from_millis(20));
    release.send(()).expect("release dispatcher teardown");

    explicit
        .join()
        .expect("explicit destroy joins")
        .expect("joined destroy succeeds");
    wait_until(Duration::from_secs(1), || {
        session_is_absent(&manager, &failed.workspace_session_id)
    });
    assert_eq!(fake.destroy_calls(), vec![failed.workspace_session_id]);
    dispatcher.shutdown_and_join();
    dispatcher.shutdown_and_join();
}

#[test]
fn dispatched_cleanup_failure_stays_visible_and_retryable() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = Arc::new(manager_with(&fake));
    let dispatcher = HolderExitDispatcher::start(&manager)
        .expect("dispatcher starts")
        .expect("fake runtime exposes supervision");
    let failed = create(&manager, &fake, "dispatch-retry", FinalizePolicy::NoOp);
    for attempt in 0..DISPATCH_CLEANUP_ATTEMPTS {
        fake.push_destroy_result(Err(sandbox_runtime_workspace::WorkspaceError::Cleanup {
            workspace_session_id: failed.workspace_session_id.0.clone(),
            failures: vec![format!("Mounts: injected failure {attempt}")],
        }));
    }

    fake.mark_holder_exited(&failed.handle, "signal:9");
    fake.notify_holder_exit();
    wait_until(Duration::from_secs(1), || {
        fake.destroy_calls().len() == DISPATCH_CLEANUP_ATTEMPTS
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        fake.destroy_calls().len(),
        DISPATCH_CLEANUP_ATTEMPTS,
        "bounded retries stop without an idle cleanup loop"
    );
    assert!(holder_cleanup_failed(
        &manager,
        &failed.workspace_session_id
    ));

    let retried = manager.reconcile_holder_exits();
    assert!(matches!(
        retried.as_slice(),
        [outcome] if outcome.disposition == HolderExitDisposition::Destroyed
    ));
    assert_eq!(fake.destroy_calls().len(), DISPATCH_CLEANUP_ATTEMPTS + 1);
    dispatcher.shutdown_and_join();
    dispatcher.shutdown_and_join();
}

#[test]
fn dead_holder_rejects_new_work_immediately_and_preserves_peer() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = manager_with(&fake);
    let failed = create(&manager, &fake, "failed", FinalizePolicy::NoOp);
    let peer = create(&manager, &fake, "peer", FinalizePolicy::NoOp);

    fake.mark_holder_exited(&failed.handle, "signal:9");

    let error = manager
        .with_gated_session(&failed.workspace_session_id, |_| ())
        .expect_err("dead holder is unavailable before cleanup starts");
    assert!(matches!(
        error,
        WorkspaceSessionError::HolderExited { workspace_session_id, .. }
            if workspace_session_id == failed.workspace_session_id
    ));
    assert!(manager
        .with_gated_session(&peer.workspace_session_id, |_| ())
        .is_ok());
    assert!(fake.destroy_calls().is_empty());
}

#[test]
fn no_op_holder_exit_cleans_once_and_emits_one_terminal_result() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = manager_with(&fake);
    let failed = create(&manager, &fake, "failed", FinalizePolicy::NoOp);
    fake.mark_holder_exited(&failed.handle, "signal:9");

    let first = manager.reconcile_holder_exits();
    let second = manager.reconcile_holder_exits();

    assert!(matches!(
        first.as_slice(),
        [outcome] if outcome.disposition == HolderExitDisposition::Destroyed
    ));
    assert!(second.is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![failed.workspace_session_id.clone()]
    );
    let lifecycle = manager.holder_lifecycle_snapshot();
    assert_eq!(lifecycle.holder_exit_total, 1);
    assert_eq!(lifecycle.cleanup_attempt_total, 1);
    assert_eq!(lifecycle.cleanup_failure_total, 0);
    assert_eq!(lifecycle.cleanup_terminal_total, 1);
    assert_eq!(lifecycle.events.len(), 3);
}

#[test]
fn holder_exit_cancels_and_joins_live_commands_before_destroy() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(support::FakeLaunchDriver::new());
    let services =
        support::build_services_with_launch_driver(Arc::clone(&fake), Arc::clone(&launch_driver));
    let failed = create(
        &services.workspace,
        &fake,
        "command-owner",
        FinalizePolicy::NoOp,
    );
    let running = services
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(failed.workspace_session_id.clone()),
            cmd: "sleep 60".to_owned(),
            timeout_ms: None,
            yield_time_ms: Some(0),
        })
        .expect("command is admitted and remains live");
    let command_id = running
        .command_session_id
        .expect("running response carries command id");

    fake.mark_holder_exited(&failed.handle, "signal:9");
    let outcomes = services.workspace.reconcile_holder_exits();

    assert!(matches!(
        outcomes.as_slice(),
        [outcome] if outcome.disposition == HolderExitDisposition::Destroyed
    ));
    assert_eq!(
        launch_driver.launcher().recorded_cancel_request_ids(),
        vec![command_id.0]
    );
    assert!(services.command.active_namespace_executions().is_empty());
    assert_eq!(fake.destroy_calls(), vec![failed.workspace_session_id]);
}

#[test]
fn explicit_destroy_during_holder_command_join_joins_holder_teardown() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(support::FakeLaunchDriver::new());
    let services =
        support::build_services_with_launch_driver(Arc::clone(&fake), Arc::clone(&launch_driver));
    let failed = create(
        &services.workspace,
        &fake,
        "destroy-during-command-join",
        FinalizePolicy::NoOp,
    );
    let running = services
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(failed.workspace_session_id.clone()),
            cmd: "sleep 60".to_owned(),
            timeout_ms: None,
            yield_time_ms: Some(0),
        })
        .expect("command is admitted and remains live");
    assert!(running.command_session_id.is_some());
    let (cancel_entered, release_cancel) =
        launch_driver.launcher().park_next_cancel_after_completion();

    fake.mark_holder_exited(&failed.handle, "signal:9");
    let holder_workspace = Arc::clone(&services.workspace);
    let holder = std::thread::spawn(move || holder_workspace.reconcile_holder_exits());
    cancel_entered
        .recv_timeout(Duration::from_secs(1))
        .expect("holder cleanup publishes command completion before join returns");
    wait_until(Duration::from_secs(1), || {
        services.command.active_namespace_executions().is_empty()
    });

    let explicit_workspace = Arc::clone(&services.workspace);
    let explicit_id = failed.workspace_session_id.clone();
    let explicit =
        std::thread::spawn(move || explicit_workspace.guarded_destroy(explicit_id, None));
    std::thread::sleep(Duration::from_millis(20));
    let explicit_completed_before_holder = explicit.is_finished();
    release_cancel
        .send(())
        .expect("release holder command join");

    let outcomes = holder.join().expect("holder cleanup joins");
    let explicit_result = explicit
        .join()
        .expect("explicit destroy joins")
        .expect("joined teardown succeeds");
    assert!(
        !explicit_completed_before_holder,
        "explicit destroy must join the holder-owned teardown transaction"
    );
    assert!(matches!(
        outcomes.as_slice(),
        [outcome] if outcome.disposition == HolderExitDisposition::Destroyed
    ));
    assert_eq!(
        explicit_result.workspace_session_id,
        failed.workspace_session_id
    );
    assert_eq!(fake.destroy_calls(), vec![failed.workspace_session_id]);
    let lifecycle = services.workspace.holder_lifecycle_snapshot();
    assert_eq!(lifecycle.cleanup_failure_total, 0);
    assert_eq!(lifecycle.cleanup_terminal_total, 1);
}

#[test]
fn explicit_destroy_joins_holder_teardown_failure_then_can_retry_once() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = Arc::new(manager_with(&fake));
    let failed = create(&manager, &fake, "join-holder-failure", FinalizePolicy::NoOp);
    fake.push_destroy_result(Err(sandbox_runtime_workspace::WorkspaceError::Cleanup {
        workspace_session_id: failed.workspace_session_id.0.clone(),
        failures: vec!["Network: injected joined failure".to_owned()],
    }));
    let (destroy_entered, release_destroy) = fake.park_next_destroy();

    fake.mark_holder_exited(&failed.handle, "signal:9");
    let holder_manager = Arc::clone(&manager);
    let holder = std::thread::spawn(move || holder_manager.reconcile_holder_exits());
    destroy_entered
        .recv_timeout(Duration::from_secs(1))
        .expect("holder owns the raw teardown");

    let explicit_manager = Arc::clone(&manager);
    let explicit_id = failed.workspace_session_id.clone();
    let explicit = std::thread::spawn(move || explicit_manager.guarded_destroy(explicit_id, None));
    std::thread::sleep(Duration::from_millis(20));
    assert!(!explicit.is_finished(), "follower waits for holder result");
    release_destroy.send(()).expect("release holder teardown");

    let outcomes = holder.join().expect("holder cleanup joins");
    assert!(matches!(
        outcomes.as_slice(),
        [outcome]
            if matches!(
                &outcome.disposition,
                HolderExitDisposition::RetryableCleanupFailure { diagnostic }
                    if diagnostic.contains("injected joined failure")
            )
    ));
    let explicit_error = explicit
        .join()
        .expect("explicit follower joins")
        .expect_err("follower receives the holder failure");
    assert!(explicit_error
        .to_string()
        .contains("injected joined failure"));
    assert_eq!(
        fake.destroy_calls(),
        vec![failed.workspace_session_id.clone()]
    );

    manager
        .guarded_destroy(failed.workspace_session_id.clone(), None)
        .expect("a later explicit retry leads one fresh transaction");
    assert_eq!(
        fake.destroy_calls(),
        vec![
            failed.workspace_session_id.clone(),
            failed.workspace_session_id,
        ]
    );
}

#[test]
fn concurrent_reconcile_joins_command_timeout_failure_without_stranding_state() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(support::FakeLaunchDriver::new());
    launch_driver
        .launcher()
        .push_script(support::FakeRunnerScript::pending_ignoring_cancel());
    let services =
        support::build_services_with_launch_driver(Arc::clone(&fake), Arc::clone(&launch_driver));
    let failed = create(
        &services.workspace,
        &fake,
        "concurrent-command-timeout",
        FinalizePolicy::NoOp,
    );
    let running = services
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(failed.workspace_session_id.clone()),
            cmd: "sleep 60".to_owned(),
            timeout_ms: None,
            yield_time_ms: Some(0),
        })
        .expect("command is admitted and remains live");
    let command_id = running
        .command_session_id
        .expect("running response carries command id");

    fake.mark_holder_exited(&failed.handle, "signal:9");
    let first_workspace = Arc::clone(&services.workspace);
    let first = std::thread::spawn(move || first_workspace.reconcile_holder_exits());
    wait_until(Duration::from_secs(1), || {
        launch_driver
            .launcher()
            .recorded_cancel_request_ids()
            .contains(&command_id.0)
    });

    let second_workspace = Arc::clone(&services.workspace);
    let second = std::thread::spawn(move || second_workspace.reconcile_holder_exits());

    let first = first.join().expect("first reconcile joins");
    let second = second.join().expect("second reconcile joins");
    let timeout_diagnostic =
        |outcomes: &[sandbox_runtime::workspace_session::HolderExitOutcome]| match outcomes {
            [outcome] => match &outcome.disposition {
                HolderExitDisposition::RetryableCleanupFailure { diagnostic } => diagnostic.clone(),
                disposition => panic!("unexpected disposition: {disposition:?}"),
            },
            outcomes => panic!("expected one joined outcome, got {outcomes:?}"),
        };
    let first_diagnostic = timeout_diagnostic(&first);
    let second_diagnostic = timeout_diagnostic(&second);
    assert!(first_diagnostic.contains("timed out joining command"));
    assert_eq!(second_diagnostic, first_diagnostic);
    assert!(holder_cleanup_failed(
        &services.workspace,
        &failed.workspace_session_id
    ));
    assert!(fake.destroy_calls().is_empty());

    launch_driver.launcher().complete_request(
        &command_id.0,
        sandbox_runtime_namespace_process::runner::protocol::RunResult {
            exit_code: 0,
            payload: serde_json::json!({ "status": "cancelled" }),
        },
    );
    wait_until(Duration::from_secs(1), || {
        services.command.active_namespace_executions().is_empty()
    });
    let retried = services.workspace.reconcile_holder_exits();
    assert!(matches!(
        retried.as_slice(),
        [outcome] if outcome.disposition == HolderExitDisposition::Destroyed
    ));
    assert_eq!(fake.destroy_calls(), vec![failed.workspace_session_id]);
}

#[test]
fn explicit_first_failure_keeps_dispatcher_retry_wake_joinable() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = Arc::new(manager_with(&fake));
    let dispatcher = HolderExitDispatcher::start(&manager)
        .expect("dispatcher starts")
        .expect("fake runtime exposes supervision");
    let failed = create(
        &manager,
        &fake,
        "explicit-first-dispatch-retry",
        FinalizePolicy::NoOp,
    );
    fake.push_destroy_result(Err(sandbox_runtime_workspace::WorkspaceError::Cleanup {
        workspace_session_id: failed.workspace_session_id.0.clone(),
        failures: vec!["Mounts: injected explicit-first failure".to_owned()],
    }));
    let (destroy_entered, release_destroy) = fake.park_next_destroy();

    fake.mark_holder_exited(&failed.handle, "signal:9");
    let explicit_manager = Arc::clone(&manager);
    let explicit_id = failed.workspace_session_id.clone();
    let explicit = std::thread::spawn(move || explicit_manager.guarded_destroy(explicit_id, None));
    destroy_entered
        .recv_timeout(Duration::from_secs(1))
        .expect("explicit destroy owns the first raw teardown");

    fake.notify_holder_exit();
    std::thread::sleep(Duration::from_millis(20));
    assert!(!explicit.is_finished());
    assert_eq!(fake.destroy_calls().len(), 1);
    release_destroy.send(()).expect("release explicit teardown");

    let explicit_error = explicit
        .join()
        .expect("explicit destroy joins")
        .expect_err("first teardown receives the injected failure");
    assert!(explicit_error
        .to_string()
        .contains("injected explicit-first failure"));
    wait_until(Duration::from_secs(1), || {
        fake.destroy_calls().len() == 2
            && session_is_absent(&manager, &failed.workspace_session_id)
            && manager.holder_lifecycle_snapshot().cleanup_terminal_total == 1
    });
    assert_eq!(
        fake.destroy_calls(),
        vec![
            failed.workspace_session_id.clone(),
            failed.workspace_session_id.clone(),
        ]
    );
    let lifecycle = manager.holder_lifecycle_snapshot();
    assert_eq!(lifecycle.holder_exit_total, 1);
    assert_eq!(lifecycle.cleanup_attempt_total, 2);
    assert_eq!(lifecycle.cleanup_failure_total, 1);
    assert_eq!(lifecycle.cleanup_terminal_total, 1);
    dispatcher.shutdown_and_join();
}

#[test]
fn explicit_first_dead_holder_with_active_command_owns_joinable_cleanup() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(support::FakeLaunchDriver::new());
    let services =
        support::build_services_with_launch_driver(Arc::clone(&fake), Arc::clone(&launch_driver));
    let failed = create(
        &services.workspace,
        &fake,
        "explicit-first-active-command",
        FinalizePolicy::NoOp,
    );
    services
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(failed.workspace_session_id.clone()),
            cmd: "sleep 60".to_owned(),
            timeout_ms: None,
            yield_time_ms: Some(0),
        })
        .expect("command is admitted and remains live");
    let (cancel_entered, release_cancel) =
        launch_driver.launcher().park_next_cancel_after_completion();

    fake.mark_holder_exited(&failed.handle, "signal:9");
    let explicit_workspace = Arc::clone(&services.workspace);
    let explicit_id = failed.workspace_session_id.clone();
    let explicit =
        std::thread::spawn(move || explicit_workspace.guarded_destroy(explicit_id, None));
    cancel_entered
        .recv_timeout(Duration::from_secs(1))
        .expect("dead-holder explicit destroy cancels and joins the command");

    let reconcile_workspace = Arc::clone(&services.workspace);
    let reconcile = std::thread::spawn(move || reconcile_workspace.reconcile_holder_exits());
    release_cancel
        .send(())
        .expect("release explicit command join");
    explicit
        .join()
        .expect("explicit thread joins")
        .expect("explicit holder cleanup succeeds");
    let joined = reconcile.join().expect("reconcile follower joins");
    assert!(matches!(
        joined.as_slice(),
        [outcome] if outcome.disposition == HolderExitDisposition::Destroyed
    ));
    assert_eq!(fake.destroy_calls(), vec![failed.workspace_session_id]);
    assert!(services.command.active_namespace_executions().is_empty());
}

#[test]
fn command_join_timeout_preserves_ledger_and_resources_for_retry() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(support::FakeLaunchDriver::new());
    launch_driver
        .launcher()
        .push_script(support::FakeRunnerScript::pending_ignoring_cancel());
    let services =
        support::build_services_with_launch_driver(Arc::clone(&fake), Arc::clone(&launch_driver));
    let failed = create(
        &services.workspace,
        &fake,
        "command-timeout",
        FinalizePolicy::NoOp,
    );
    let running = services
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(failed.workspace_session_id.clone()),
            cmd: "sleep 60".to_owned(),
            timeout_ms: None,
            yield_time_ms: Some(0),
        })
        .expect("command is admitted and remains live");
    let command_id = running
        .command_session_id
        .expect("running response carries command id");

    fake.mark_holder_exited(&failed.handle, "signal:9");
    let started = Instant::now();
    let first = services.workspace.reconcile_holder_exits();

    assert!(matches!(
        first.as_slice(),
        [outcome]
            if matches!(
                &outcome.disposition,
                HolderExitDisposition::RetryableCleanupFailure { diagnostic }
                    if diagnostic.contains("timed out joining command")
            )
    ));
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(holder_cleanup_failed(
        &services.workspace,
        &failed.workspace_session_id
    ));
    assert_eq!(
        services
            .command
            .active_namespace_executions()
            .into_iter()
            .map(|execution| execution.namespace_execution_id)
            .collect::<Vec<_>>(),
        vec![command_id.clone()],
        "the admitted command ledger remains owned after a failed join"
    );
    assert!(
        fake.destroy_calls().is_empty(),
        "resources remain for retry"
    );

    launch_driver.launcher().complete_request(
        &command_id.0,
        sandbox_runtime_namespace_process::runner::protocol::RunResult {
            exit_code: 0,
            payload: serde_json::json!({ "status": "cancelled" }),
        },
    );
    wait_until(Duration::from_secs(1), || {
        services.command.active_namespace_executions().is_empty()
    });
    let second = services.workspace.reconcile_holder_exits();
    assert!(matches!(
        second.as_slice(),
        [outcome] if outcome.disposition == HolderExitDisposition::Destroyed
    ));
    assert_eq!(fake.destroy_calls(), vec![failed.workspace_session_id]);
}

#[test]
fn holder_cleanup_failure_remains_retryable_without_recounting_exit() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = manager_with(&fake);
    let failed = create(&manager, &fake, "retry-cleanup", FinalizePolicy::NoOp);
    fake.push_destroy_result(Err(sandbox_runtime_workspace::WorkspaceError::Cleanup {
        workspace_session_id: failed.workspace_session_id.0.clone(),
        failures: vec!["Network: injected failure".to_owned()],
    }));
    fake.mark_holder_exited(&failed.handle, "signal:9");

    let first = manager.reconcile_holder_exits();
    let second = manager.reconcile_holder_exits();

    assert!(matches!(
        first.as_slice(),
        [outcome]
            if matches!(
                outcome.disposition,
                HolderExitDisposition::RetryableCleanupFailure { .. }
            )
    ));
    assert!(matches!(
        second.as_slice(),
        [outcome] if outcome.disposition == HolderExitDisposition::Destroyed
    ));
    assert_eq!(
        fake.destroy_calls(),
        vec![
            failed.workspace_session_id.clone(),
            failed.workspace_session_id.clone(),
        ]
    );
    let lifecycle = manager.holder_lifecycle_snapshot();
    assert_eq!(lifecycle.holder_exit_total, 1);
    assert_eq!(lifecycle.cleanup_attempt_total, 2);
    assert_eq!(lifecycle.cleanup_failure_total, 1);
    assert_eq!(lifecycle.cleanup_terminal_total, 1);
    assert_eq!(lifecycle.events.len(), 5);
}

#[test]
fn concurrent_explicit_destroy_callers_join_one_transaction() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = Arc::new(manager_with(&fake));
    let handler = create(&manager, &fake, "racing", FinalizePolicy::NoOp);
    let (entered, release) = fake.park_next_destroy();

    let left_manager = Arc::clone(&manager);
    let left_handler = handler.clone();
    let left =
        std::thread::spawn(move || left_manager.destroy_session(left_handler, Default::default()));
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("first destroy enters runtime");

    let right_manager = Arc::clone(&manager);
    let right_handler = handler.clone();
    let right = std::thread::spawn(move || {
        right_manager.destroy_session(right_handler, Default::default())
    });
    std::thread::sleep(Duration::from_millis(20));
    release.send(()).expect("release first destroy");

    let left = left.join().expect("left joins").expect("left succeeds");
    let right = right.join().expect("right joins").expect("right succeeds");
    assert_eq!(left, right);
    assert_eq!(fake.destroy_calls(), vec![handler.workspace_session_id]);
}

#[test]
fn publish_required_holder_exit_commits_bounded_recovery_then_releases_workspace() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let manager = manager_with(&fake);
    let failed = create(
        &manager,
        &fake,
        "publish-required",
        FinalizePolicy::PublishThenDestroy,
    );
    fake.mark_holder_exited(&failed.handle, "signal:9");

    let outcomes = manager.reconcile_holder_exits();

    let artifact = match outcomes.as_slice() {
        [outcome] => match &outcome.disposition {
            HolderExitDisposition::RecoveryRequired { artifact } => artifact,
            disposition => panic!("unexpected disposition: {disposition:?}"),
        },
        outcomes => panic!("unexpected outcomes: {outcomes:?}"),
    };
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(artifact.join("manifest.json")).expect("durable recovery manifest"),
    )
    .expect("recovery manifest json");
    assert_eq!(manifest["workspace_session_id"], "publish-required");
    assert_eq!(manifest["finalization_state"], "finalization_failed");
    assert!(
        manifest["artifact_max_bytes"]
            .as_u64()
            .expect("artifact_max_bytes is a u64")
            <= 1024 * 1024
    );
    assert!(fake.capture_calls().is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![failed.workspace_session_id.clone()]
    );
    assert!(session_is_absent(
        &manager,
        &WorkspaceSessionId("publish-required".to_owned())
    ));
}

#[test]
fn publish_required_last_command_completion_after_holder_exit_routes_to_recovery() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(support::FakeLaunchDriver::new());
    let layerstack =
        support::observed_layerstack_service(sandbox_observability_telemetry::Observer::disabled());
    let services = support::build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        Arc::clone(&launch_driver),
        Arc::clone(&layerstack),
    );
    let failed = create(
        &services.workspace,
        &fake,
        "publish-command-exit-race",
        FinalizePolicy::PublishThenDestroy,
    );
    let upperdir = failed
        .handle
        .entry()
        .expect("holder exposes its recovery source")
        .upperdir;
    std::fs::create_dir_all(&upperdir).expect("create recovery source");
    std::fs::write(upperdir.join("unfinished.txt"), b"preserve me\n")
        .expect("write recovery marker");
    fake.push_capture_result(Ok(CapturedWorkspaceChanges {
        workspace_session_id: failed.workspace_session_id.clone(),
        base_revision: failed.handle.base_revision(),
        base_manifest: failed.handle.snapshot.manifest.clone(),
        changed_path_kinds: Default::default(),
        protected_drops: Vec::new(),
        stats: None,
        metadata_path_count: 0,
        changed_paths: Vec::new(),
        changes: Vec::new(),
    }));
    let running = services
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(failed.workspace_session_id.clone()),
            cmd: "sleep 60".to_owned(),
            timeout_ms: None,
            yield_time_ms: Some(0),
        })
        .expect("publish-required command is admitted");
    let command_id = running
        .command_session_id
        .expect("running response carries command id");

    fake.exit_holder_on_next_liveness_check(&failed.handle, "signal:9");
    launch_driver.launcher().complete_request(
        &command_id.0,
        sandbox_runtime_namespace_process::runner::protocol::RunResult {
            exit_code: 137,
            payload: serde_json::json!({ "status": "holder-exited" }),
        },
    );
    wait_until(Duration::from_secs(1), || {
        services.command.active_namespace_executions().is_empty()
            && services
                .workspace
                .holder_lifecycle_snapshot()
                .cleanup_terminal_total
                == 1
    });

    let recovery_root = layerstack
        .layer_stack_root()
        .parent()
        .unwrap_or(layerstack.layer_stack_root())
        .join("storage")
        .join("workspace_recovery");
    let artifacts = std::fs::read_dir(&recovery_root)
        .expect("completion-owned teardown leaves a recovery artifact")
        .collect::<Result<Vec<_>, _>>()
        .expect("recovery artifact directory is readable");
    assert_eq!(artifacts.len(), 1);
    let artifact = artifacts[0].path();
    assert!(
        fake.capture_calls().is_empty(),
        "normal capture/publish must not steal dead-holder teardown ownership"
    );
    assert_eq!(
        fake.holder_finalization_calls(),
        1,
        "the sole supervisor finalization boundary observes the exit after ledger removal"
    );
    assert!(fake.capture_after_quiesce_calls().is_empty());
    assert_eq!(
        std::fs::read(artifact.join("files/unfinished.txt")).expect("recovery marker is durable"),
        b"preserve me\n"
    );
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(artifact.join("manifest.json")).expect("durable recovery manifest"),
    )
    .expect("recovery manifest json");
    assert_eq!(
        manifest["workspace_session_id"],
        "publish-command-exit-race"
    );
    assert_eq!(manifest["finalization_state"], "finalization_failed");

    let lifecycle = services.workspace.holder_lifecycle_snapshot();
    assert_eq!(lifecycle.holder_exit_total, 1);
    assert_eq!(lifecycle.cleanup_attempt_total, 1);
    assert_eq!(lifecycle.cleanup_failure_total, 0);
    assert_eq!(lifecycle.cleanup_terminal_total, 1);
    assert_eq!(
        lifecycle
            .events
            .iter()
            .map(|event| event.kind)
            .collect::<Vec<_>>(),
        vec![
            HolderLifecycleEventKind::ExitObserved,
            HolderLifecycleEventKind::CleanupAttempt,
            HolderLifecycleEventKind::CleanupTerminal,
        ]
    );
    assert_eq!(
        fake.destroy_calls(),
        vec![failed.workspace_session_id.clone()]
    );
    assert!(session_is_absent(
        &services.workspace,
        &failed.workspace_session_id
    ));
    assert!(services.workspace.reconcile_holder_exits().is_empty());
}

#[test]
fn publish_required_completion_hands_off_to_existing_holder_flight_without_recursive_join() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let launch_driver = Arc::new(support::FakeLaunchDriver::new());
    let layerstack =
        support::observed_layerstack_service(sandbox_observability_telemetry::Observer::disabled());
    let services = Arc::new(support::build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        Arc::clone(&launch_driver),
        Arc::clone(&layerstack),
    ));
    let failed = create(
        &services.workspace,
        &fake,
        "publish-existing-holder-flight",
        FinalizePolicy::PublishThenDestroy,
    );
    let upperdir = failed
        .handle
        .entry()
        .expect("holder exposes its recovery source")
        .upperdir;
    std::fs::create_dir_all(&upperdir).expect("create recovery source");
    std::fs::write(upperdir.join("unfinished.txt"), b"preserve me\n")
        .expect("write recovery marker");
    let running = services
        .command
        .exec_command(ExecCommandInput {
            workspace_session_id: Some(failed.workspace_session_id.clone()),
            cmd: "sleep 60".to_owned(),
            timeout_ms: None,
            yield_time_ms: Some(0),
        })
        .expect("publish-required command is admitted");
    assert!(running.command_session_id.is_some());

    fake.mark_holder_exited(&failed.handle, "signal:9");
    let reconcile_workspace = Arc::clone(&services.workspace);
    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    let reconcile = std::thread::spawn(move || {
        let _ = result_tx.send(reconcile_workspace.reconcile_holder_exits());
    });
    let outcomes = result_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("completion hands off without recursively joining its owning holder flight");
    reconcile.join().expect("holder reconciliation joins");

    let artifact = match outcomes.as_slice() {
        [outcome] => match &outcome.disposition {
            HolderExitDisposition::RecoveryRequired { artifact } => artifact,
            other => panic!("expected recovery-required disposition, got {other:?}"),
        },
        other => panic!("expected one holder outcome, got {other:?}"),
    };
    assert_eq!(
        std::fs::read(artifact.join("files/unfinished.txt")).expect("recovery marker is durable"),
        b"preserve me\n"
    );
    assert!(fake.capture_calls().is_empty());
    assert_eq!(
        fake.destroy_calls(),
        vec![failed.workspace_session_id.clone()]
    );
    assert!(services.command.active_namespace_executions().is_empty());
    assert!(session_is_absent(
        &services.workspace,
        &failed.workspace_session_id
    ));
    assert!(services.workspace.reconcile_holder_exits().is_empty());
    let lifecycle = services.workspace.holder_lifecycle_snapshot();
    assert_eq!(lifecycle.cleanup_failure_total, 0);
    assert_eq!(lifecycle.cleanup_terminal_total, 1);
}

#[test]
fn explicit_destroy_that_observes_dead_publish_holder_preserves_recovery_first() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let layerstack =
        support::observed_layerstack_service(sandbox_observability_telemetry::Observer::disabled());
    let recovery_root = layerstack
        .layer_stack_root()
        .parent()
        .expect("test layer stack has a storage parent")
        .join("storage")
        .join("workspace_recovery");
    let manager = WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        layerstack,
        sandbox_observability_telemetry::Observer::disabled(),
    );
    let failed = create(
        &manager,
        &fake,
        "explicit-dead-publish",
        FinalizePolicy::PublishThenDestroy,
    );
    fake.mark_holder_exited(&failed.handle, "signal:9");

    manager
        .guarded_destroy(failed.workspace_session_id.clone(), None)
        .expect("explicit destroy completes through dead-holder finalization");

    let artifacts = std::fs::read_dir(&recovery_root)
        .expect("dead publish holder leaves a recovery root")
        .collect::<Result<Vec<_>, _>>()
        .expect("recovery artifact directory is readable");
    assert_eq!(artifacts.len(), 1);
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(artifacts[0].path().join("manifest.json"))
            .expect("recovery manifest is durable"),
    )
    .expect("recovery manifest json");
    assert_eq!(manifest["workspace_session_id"], "explicit-dead-publish");
    assert_eq!(manifest["finalization_state"], "finalization_failed");
    assert_eq!(fake.destroy_calls(), vec![failed.workspace_session_id]);
    assert!(manager.reconcile_holder_exits().is_empty());
}

#[test]
fn raw_dead_publish_owner_preserves_recovery_before_teardown_and_is_joinable() {
    let fake = Arc::new(FakeWorkspaceService::new());
    let layerstack =
        support::observed_layerstack_service(sandbox_observability_telemetry::Observer::disabled());
    let recovery_root = layerstack
        .layer_stack_root()
        .parent()
        .expect("test layer stack has a storage parent")
        .join("storage")
        .join("workspace_recovery");
    let manager = Arc::new(WorkspaceSessionService::new(
        support::fake_workspace_runtime(Arc::clone(&fake)),
        layerstack,
        sandbox_observability_telemetry::Observer::disabled(),
    ));
    let failed = create(
        &manager,
        &fake,
        "raw-dead-publish",
        FinalizePolicy::PublishThenDestroy,
    );
    let (destroy_entered, release_destroy) = fake.park_next_destroy();

    fake.mark_holder_exited(&failed.handle, "signal:9");
    let raw_manager = Arc::clone(&manager);
    let raw_handler = failed.clone();
    let raw =
        std::thread::spawn(move || raw_manager.destroy_session(raw_handler, Default::default()));
    destroy_entered
        .recv_timeout(Duration::from_secs(1))
        .expect("raw owner reaches teardown only after dead-holder planning");

    let artifact_before_teardown = std::fs::read_dir(&recovery_root)
        .map(|entries| entries.count() == 1)
        .unwrap_or(false);
    let explicit_manager = Arc::clone(&manager);
    let explicit_id = failed.workspace_session_id.clone();
    let explicit = std::thread::spawn(move || explicit_manager.guarded_destroy(explicit_id, None));
    let join_deadline = Instant::now() + Duration::from_secs(1);
    while manager.destroy_flight_waiter_count(&failed.workspace_session_id) == 0 {
        assert!(
            Instant::now() < join_deadline,
            "explicit follower joins the raw destroy flight"
        );
        std::thread::yield_now();
    }
    release_destroy.send(()).expect("release raw teardown");

    raw.join()
        .expect("raw owner joins")
        .expect("raw owner succeeds");
    explicit
        .join()
        .expect("explicit follower joins")
        .expect("explicit follower shares success");
    assert!(
        artifact_before_teardown,
        "recovery must be durable before the raw teardown hook is entered"
    );
    assert_eq!(fake.destroy_calls(), vec![failed.workspace_session_id]);
}
