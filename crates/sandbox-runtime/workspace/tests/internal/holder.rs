use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use crate::model::WorkspaceSessionId;
use crate::namespace::holder::{
    HolderExitEvent, HolderExitReason, HolderFinalization, HolderFinalizationUnknownClass,
    HolderIdentity, HolderProcess, HolderProcessExit, HolderRegistration, HolderSignal,
    HolderSupervisor, HolderSupervisorError,
};
use sandbox_runtime_workspace::HolderExitWait;

#[derive(Default)]
struct FakeProcessState {
    exited: AtomicBool,
    normal_exit: AtomicBool,
    ignore_signals: AtomicBool,
    wait_errors_remaining: AtomicUsize,
    blocking_wait_errors_remaining: AtomicUsize,
    identity_errors_remaining: AtomicUsize,
    signal_errors_remaining: AtomicUsize,
    try_wait_calls: AtomicUsize,
    blocking_wait_calls: AtomicUsize,
    reaps: AtomicUsize,
    signals: AtomicUsize,
    identity_checks: AtomicUsize,
    sequence: AtomicUsize,
    last_identity_check_sequence: AtomicUsize,
    signal_followed_identity_check: AtomicBool,
    observed_identity: Mutex<Option<HolderIdentity>>,
    delayed_exit_after_signal: Mutex<Option<Duration>>,
    signaled_at: Mutex<Option<Instant>>,
    dropped_unreaped: AtomicUsize,
}

struct FakeProcess {
    state: Arc<FakeProcessState>,
}

impl HolderProcess for FakeProcess {
    fn try_wait(&mut self) -> Result<Option<HolderProcessExit>, String> {
        self.state.try_wait_calls.fetch_add(1, Ordering::SeqCst);
        self.state.sequence.fetch_add(1, Ordering::SeqCst);
        if decrement_if_positive(&self.state.wait_errors_remaining) {
            return Err("injected try_wait failure".to_owned());
        }
        self.observe_delayed_exit();
        if !self.state.exited.load(Ordering::SeqCst) {
            return Ok(None);
        }
        Ok(Some(self.reap()))
    }

    fn wait_reap(&mut self) -> Result<HolderProcessExit, String> {
        self.state
            .blocking_wait_calls
            .fetch_add(1, Ordering::SeqCst);
        self.state.sequence.fetch_add(1, Ordering::SeqCst);
        if decrement_if_positive(&self.state.blocking_wait_errors_remaining) {
            return Err("injected blocking wait failure".to_owned());
        }
        loop {
            self.observe_delayed_exit();
            if self.state.exited.load(Ordering::SeqCst) {
                return Ok(self.reap());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn identity_matches(&self, expected: &HolderIdentity) -> Result<bool, String> {
        self.state.identity_checks.fetch_add(1, Ordering::SeqCst);
        let sequence = self.state.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        if decrement_if_positive(&self.state.identity_errors_remaining) {
            return Err("injected identity check failure".to_owned());
        }
        self.state
            .last_identity_check_sequence
            .store(sequence, Ordering::SeqCst);
        Ok(self
            .state
            .observed_identity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|observed| observed == expected))
    }

    fn send_signal(&mut self, _signal: HolderSignal) -> Result<(), String> {
        let sequence = self.state.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.signal_followed_identity_check.store(
            self.state
                .last_identity_check_sequence
                .load(Ordering::SeqCst)
                .saturating_add(1)
                == sequence,
            Ordering::SeqCst,
        );
        if decrement_if_positive(&self.state.signal_errors_remaining) {
            return Err("injected signal failure".to_owned());
        }
        self.state.signals.fetch_add(1, Ordering::SeqCst);
        if !self.state.ignore_signals.load(Ordering::SeqCst) {
            if self
                .state
                .delayed_exit_after_signal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some()
            {
                *self
                    .state
                    .signaled_at
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
            } else {
                self.state.exited.store(true, Ordering::SeqCst);
            }
        }
        Ok(())
    }
}

impl FakeProcess {
    fn observe_delayed_exit(&self) {
        let delayed_exit = *self
            .state
            .delayed_exit_after_signal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let signaled_at = *self
            .state
            .signaled_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if delayed_exit
            .zip(signaled_at)
            .is_some_and(|(delay, signaled_at)| signaled_at.elapsed() >= delay)
        {
            self.state.exited.store(true, Ordering::SeqCst);
        }
    }

    fn reap(&self) -> HolderProcessExit {
        assert_eq!(self.state.reaps.fetch_add(1, Ordering::SeqCst), 0);
        if self.state.normal_exit.load(Ordering::SeqCst) {
            HolderProcessExit {
                exit_status: Some(0),
                signal: None,
                status_raw: Some(0),
            }
        } else {
            HolderProcessExit {
                exit_status: None,
                signal: Some(9),
                status_raw: Some(9),
            }
        }
    }
}

impl Drop for FakeProcess {
    fn drop(&mut self) {
        if self.state.reaps.load(Ordering::SeqCst) == 0 {
            self.state.dropped_unreaped.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[test]
fn holder_process_factory_runs_on_stable_supervisor_thread() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    let created_on = Arc::new(Mutex::new(None));
    let caller_thread_id = std::thread::current().id();
    let factory_process = Arc::clone(&process);
    let factory_created_on = Arc::clone(&created_on);

    let registration = supervisor
        .spawn_process(
            WorkspaceSessionId("workspace-supervisor-spawn".to_owned()),
            move |generation: u64| -> Result<_, String> {
                let current = std::thread::current();
                *factory_created_on
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some((current.id(), current.name().map(str::to_owned)));
                let identity = identity(41, generation);
                factory_process.set_identity(identity.clone());
                Ok((
                    identity,
                    Box::new(FakeProcess {
                        state: factory_process,
                    }) as Box<dyn HolderProcess>,
                ))
            },
        )
        .expect("fake holder is spawned and registered");

    let (creator_thread_id, creator_thread_name) = created_on
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("holder process factory ran");
    assert_ne!(creator_thread_id, caller_thread_id);
    assert_eq!(
        creator_thread_name.as_deref(),
        Some("eos-holder-supervisor")
    );

    process.exited.store(true, Ordering::SeqCst);
    wait_for_exit(&registration, Duration::from_secs(1));
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn spawn_reply_drop_terminates_and_reaps_once() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    let factory_process = Arc::clone(&process);
    let reply = supervisor
        .enqueue_spawn_process(
            WorkspaceSessionId("workspace-dropped-spawn-reply".to_owned()),
            move |generation| {
                let identity = identity(42, generation);
                factory_process.set_identity(identity.clone());
                Ok((
                    identity,
                    Box::new(FakeProcess {
                        state: factory_process,
                    }) as Box<dyn HolderProcess>,
                ))
            },
        )
        .expect("spawn command is queued");

    // Wait until the successful result has been buffered. This exercises the
    // race where `reply.send(Ok(..))` succeeds immediately before the caller
    // drops its receiver.
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));
    process.identity_errors_remaining.store(1, Ordering::SeqCst);
    drop(reply);
    wait_for_count(&process.reaps, 1, Duration::from_secs(1));

    assert_eq!(process.signals.load(Ordering::SeqCst), 1);
    assert!(process.identity_checks.load(Ordering::SeqCst) >= 2);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(process.dropped_unreaped.load(Ordering::SeqCst), 0);

    let reused = Arc::new(FakeProcessState::default());
    let reused_registration = registered(&supervisor, "workspace-dropped-spawn-reply", &reused);
    reused.exited.store(true, Ordering::SeqCst);
    wait_for_exit(&reused_registration, Duration::from_secs(1));
    assert_eq!(reused.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn spawn_factory_panic_does_not_abandon_existing_holders() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let existing = Arc::new(FakeProcessState::default());
    let existing_registration =
        registered(&supervisor, "workspace-existing-before-panic", &existing);

    let panic_error = supervisor
        .spawn_process(
            WorkspaceSessionId("workspace-panicking-factory".to_owned()),
            |_generation| -> Result<(HolderIdentity, Box<dyn HolderProcess>), String> {
                panic!("injected holder factory panic")
            },
        )
        .expect_err("factory panic is contained as a spawn error");
    assert_eq!(
        panic_error,
        HolderSupervisorError::Process {
            workspace_session_id: WorkspaceSessionId("workspace-panicking-factory".to_owned()),
            message: "holder factory panicked".to_owned(),
        }
    );
    assert_eq!(existing.signals.load(Ordering::SeqCst), 0);
    assert_eq!(existing.reaps.load(Ordering::SeqCst), 0);

    existing.exited.store(true, Ordering::SeqCst);
    wait_for_exit(&existing_registration, Duration::from_secs(1));
    assert_eq!(existing.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(existing.dropped_unreaped.load(Ordering::SeqCst), 0);

    let peer = Arc::new(FakeProcessState::default());
    let peer_registration = registered(&supervisor, "workspace-after-panic", &peer);
    peer.exited.store(true, Ordering::SeqCst);
    wait_for_exit(&peer_registration, Duration::from_secs(1));
    assert_eq!(peer.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn normal_exit_is_detected_under_one_second_and_reaped_once() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    process.normal_exit.store(true, Ordering::SeqCst);
    let registration = registered(&supervisor, "workspace-normal", &process);

    process.exited.store(true, Ordering::SeqCst);
    let event = wait_for_exit(&registration, Duration::from_secs(1));

    assert_eq!(event.reason, HolderExitReason::Unexpected);
    assert_eq!(event.exit.exit_status, Some(0));
    assert_eq!(event.exit.signal, None);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
}

#[test]
fn unexpected_signal_exit_reports_sigkill_and_reaps_once() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-signal-exit", &process);

    process.exited.store(true, Ordering::SeqCst);
    let event = wait_for_exit(&registration, Duration::from_secs(1));

    assert_eq!(event.reason, HolderExitReason::Unexpected);
    assert_eq!(event.exit.exit_status, None);
    assert_eq!(event.exit.signal, Some(9));
    assert_eq!(event.exit.status_raw, Some(9));
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
}

#[test]
fn finalization_quiesces_exact_generation_and_returns_exact_proof() {
    let supervisor = HolderSupervisor::new(Duration::from_secs(60), 8);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-finalize-running", &process);
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));

    let first = supervisor.quiesce_for_finalization(&registration);
    let proof = match first {
        HolderFinalization::Quiesced { proof } => proof,
        other => panic!("expected a quiesced holder, got {other:?}"),
    };
    assert!(registration.matches_finalization_proof(&proof));
    let mut wrong_session = proof.clone();
    wrong_session.workspace_session_id = WorkspaceSessionId("workspace-other".to_owned());
    assert!(!registration.matches_finalization_proof(&wrong_session));
    let mut wrong_identity = proof.clone();
    wrong_identity.holder_identity.generation += 1;
    assert!(!registration.matches_finalization_proof(&wrong_identity));
    let mut wrong_sequence = proof.clone();
    wrong_sequence.exit_sequence += 1;
    assert!(!registration.matches_finalization_proof(&wrong_sequence));
    assert_eq!(
        registration.exit_event().map(|event| event.reason),
        Some(HolderExitReason::Destroy)
    );
    assert_eq!(process.signals.load(Ordering::SeqCst), 1);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Exited
    );
    assert_eq!(process.signals.load(Ordering::SeqCst), 1);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn planned_finalization_is_proven_without_reconciliation_history_or_wake() {
    let supervisor = HolderSupervisor::new(Duration::from_secs(60), 8);
    let subscription = supervisor
        .take_exit_subscription()
        .expect("one subscription is available");
    let (listener, shutdown) = subscription.into_parts();
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-planned-finalization", &process);
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));

    let proof = match supervisor.quiesce_for_finalization(&registration) {
        HolderFinalization::Quiesced { proof } => proof,
        other => panic!("expected a quiesced holder, got {other:?}"),
    };

    assert!(registration.matches_finalization_proof(&proof));
    assert_eq!(supervisor.stats().holder_exit_total, 1);
    assert_eq!(supervisor.stats().retained_event_count, 0);
    assert_eq!(supervisor.stats().dropped_event_total, 0);
    assert_eq!(
        listener.wait_for_retry(Duration::ZERO),
        HolderExitWait::RetryDeadline
    );
    shutdown.stop();
}

#[test]
fn finalization_observes_preexisting_exit_without_issuing_proof() {
    let supervisor = HolderSupervisor::new(Duration::from_secs(60), 8);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-finalize-exited", &process);
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));

    process.exited.store(true, Ordering::SeqCst);
    assert_eq!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Exited
    );

    let event = registration
        .exit_event()
        .expect("an exited finalization reply follows registration publication");
    assert_eq!(event.reason, HolderExitReason::Unexpected);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Exited
    );
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
}

#[test]
fn transient_finalization_wait_error_is_unknown_then_quiesced() {
    let supervisor = HolderSupervisor::new(Duration::from_secs(60), 8);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-finalize-transient", &process);
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));
    process.wait_errors_remaining.store(1, Ordering::SeqCst);

    assert_eq!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Unknown {
            class: HolderFinalizationUnknownClass::ObservationFailed,
        }
    );
    let retry = supervisor.quiesce_for_finalization(&registration);
    assert!(matches!(retry, HolderFinalization::Quiesced { .. }));
    assert_eq!(process.signals.load(Ordering::SeqCst), 1);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn forged_generation_finalization_is_rejected_without_affecting_peer() {
    let supervisor = HolderSupervisor::new(Duration::from_secs(60), 8);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-finalize-generation", &process);
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));

    let foreign_supervisor = HolderSupervisor::new(Duration::from_secs(60), 8);
    assert_eq!(foreign_supervisor.next_generation(), 1);
    let foreign_process = Arc::new(FakeProcessState::default());
    let forged = registered(
        &foreign_supervisor,
        "workspace-finalize-generation",
        &foreign_process,
    );
    wait_for_count(&foreign_process.try_wait_calls, 1, Duration::from_secs(1));

    assert_eq!(
        supervisor.quiesce_for_finalization(&forged),
        HolderFinalization::Unknown {
            class: HolderFinalizationUnknownClass::NotRegistered,
        }
    );
    assert_eq!(process.signals.load(Ordering::SeqCst), 0);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 0);

    assert!(matches!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Quiesced { .. }
    ));
    assert!(matches!(
        foreign_supervisor.quiesce_for_finalization(&forged),
        HolderFinalization::Quiesced { .. }
    ));
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(foreign_process.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn finalization_signal_failure_is_retryable_without_false_proof() {
    let supervisor = HolderSupervisor::new(Duration::from_secs(60), 8);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-finalize-signal-failure", &process);
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));
    process.signal_errors_remaining.store(1, Ordering::SeqCst);

    assert_eq!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Unknown {
            class: HolderFinalizationUnknownClass::TerminationFailed,
        }
    );
    assert!(registration.is_live());
    assert!(registration.exit_event().is_none());
    assert_eq!(process.reaps.load(Ordering::SeqCst), 0);

    let retry = supervisor.quiesce_for_finalization(&registration);
    assert!(matches!(retry, HolderFinalization::Quiesced { .. }));
    assert_eq!(process.signals.load(Ordering::SeqCst), 1);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn finalization_identity_mismatch_refuses_signal_and_proof() {
    let supervisor = HolderSupervisor::new(Duration::from_secs(60), 8);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-finalize-identity", &process);
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));
    process.mutate_identity(|identity| identity.start_time_ticks += 1);

    assert_eq!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Unknown {
            class: HolderFinalizationUnknownClass::IdentityMismatch,
        }
    );
    assert!(registration.is_live());
    assert!(registration.exit_event().is_none());
    assert_eq!(process.signals.load(Ordering::SeqCst), 0);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 0);

    process.set_identity(identity(41, 1));
    assert!(matches!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Quiesced { .. }
    ));
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn finalization_wait_error_terminal_is_exited_without_false_proof() {
    let supervisor = HolderSupervisor::new(Duration::from_secs(60), 8);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-finalize-wait-error", &process);
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));
    process
        .wait_errors_remaining
        .store(usize::MAX, Ordering::SeqCst);

    assert!(matches!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Unknown {
            class: HolderFinalizationUnknownClass::ObservationFailed,
        }
    ));
    assert_eq!(
        supervisor.quiesce_for_finalization(&registration),
        HolderFinalization::Exited
    );
    assert_eq!(
        registration.exit_event().map(|event| event.reason),
        Some(HolderExitReason::WaitError)
    );
    wait_for_count(&process.reaps, 1, Duration::from_secs(1));
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn concurrent_finalization_and_destroy_share_one_reap() {
    let supervisor = Arc::new(HolderSupervisor::new(Duration::from_millis(2), 8));
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-finalize-destroy-race", &process);
    let start = Arc::new(Barrier::new(3));

    let finalize_supervisor = Arc::clone(&supervisor);
    let finalize_registration = registration.clone();
    let finalize_start = Arc::clone(&start);
    let finalize = std::thread::spawn(move || {
        finalize_start.wait();
        finalize_supervisor.quiesce_for_finalization(&finalize_registration)
    });
    let destroy_supervisor = Arc::clone(&supervisor);
    let destroy_registration = registration.clone();
    let destroy_start = Arc::clone(&start);
    let destroy = std::thread::spawn(move || {
        destroy_start.wait();
        destroy_supervisor.terminate(&destroy_registration, Duration::ZERO)
    });
    start.wait();

    let finalization = finalize.join().expect("finalization joins");
    let destroy = destroy
        .join()
        .expect("destroy joins")
        .expect("destroy succeeds");
    assert!(matches!(
        &finalization,
        HolderFinalization::Quiesced { .. }
            | HolderFinalization::Exited
            | HolderFinalization::Unknown {
                class: HolderFinalizationUnknownClass::TerminationInProgress,
            }
    ));
    assert!(
        destroy.holder_was_alive || matches!(&finalization, HolderFinalization::Quiesced { .. })
    );
    assert_eq!(destroy.signal, Some(9));
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
}

#[test]
fn repeated_join_after_exit_does_not_publish_duplicate_notification() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let subscription = supervisor
        .take_exit_subscription()
        .expect("one subscription is available");
    let (listener, shutdown) = subscription.into_parts();
    let process = Arc::new(FakeProcessState::default());
    process.normal_exit.store(true, Ordering::SeqCst);
    let registration = registered(&supervisor, "workspace-duplicate-exit", &process);

    process.exited.store(true, Ordering::SeqCst);
    wait_for_exit(&registration, Duration::from_secs(1));
    let first = supervisor
        .terminate(&registration, Duration::ZERO)
        .expect("first join returns the recorded exit");
    let repeated = supervisor
        .terminate(&registration, Duration::ZERO)
        .expect("repeated join returns the recorded exit");

    assert_eq!(first, repeated);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
    assert_eq!(
        listener.wait_for_retry(Duration::ZERO),
        HolderExitWait::Wake
    );
    assert_eq!(
        listener.wait_for_retry(Duration::ZERO),
        HolderExitWait::RetryDeadline
    );
    shutdown.stop();
}

#[test]
fn exit_notification_never_precedes_registration_state() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let subscription = supervisor
        .take_exit_subscription()
        .expect("one subscription is available");
    let (listener, shutdown) = subscription.into_parts();
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-notification-order", &process);

    process.exited.store(true, Ordering::SeqCst);
    assert_eq!(
        listener.wait_for_retry(Duration::from_secs(1)),
        HolderExitWait::Wake
    );
    let event = registration
        .exit_event()
        .expect("a wake always observes the published exit state");
    assert_eq!(event.reason, HolderExitReason::Unexpected);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
    shutdown.stop();
}

#[test]
fn idle_subscription_has_no_synthetic_activity() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let subscription = supervisor
        .take_exit_subscription()
        .expect("one subscription is available");
    let (listener, shutdown) = subscription.into_parts();

    assert_eq!(
        listener.wait_for_retry(Duration::ZERO),
        HolderExitWait::RetryDeadline
    );
    shutdown.stop();
}

#[test]
fn exit_before_subscription_queues_one_reconciliation_wake() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    process.normal_exit.store(true, Ordering::SeqCst);
    let registration = registered(&supervisor, "workspace-before-subscription", &process);

    process.exited.store(true, Ordering::SeqCst);
    let event = wait_for_exit(&registration, Duration::from_secs(1));
    assert_eq!(event.reason, HolderExitReason::Unexpected);

    let subscription = supervisor
        .take_exit_subscription()
        .expect("one subscription is available");
    let (listener, shutdown) = subscription.into_parts();
    assert_eq!(
        listener.wait_for_retry(Duration::ZERO),
        HolderExitWait::Wake
    );
    assert_eq!(
        listener.wait_for_retry(Duration::ZERO),
        HolderExitWait::RetryDeadline
    );
    shutdown.stop();
}

#[test]
fn dropped_exit_history_still_queues_late_reconciliation_wake() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 0);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-dropped-history", &process);

    process.exited.store(true, Ordering::SeqCst);
    wait_for_exit(&registration, Duration::from_secs(1));
    assert_eq!(supervisor.stats().dropped_event_total, 1);

    let subscription = supervisor
        .take_exit_subscription()
        .expect("one late subscription is available");
    let (listener, shutdown) = subscription.into_parts();
    assert_eq!(
        listener.wait_for_retry(Duration::ZERO),
        HolderExitWait::Wake
    );
    assert_eq!(
        listener.wait_for_retry(Duration::ZERO),
        HolderExitWait::RetryDeadline
    );
    shutdown.stop();
}

#[test]
fn transient_try_wait_error_is_retried_by_one_owner_and_reaped_once() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    process.wait_errors_remaining.store(1, Ordering::SeqCst);
    process.exited.store(true, Ordering::SeqCst);
    let registration = registered(&supervisor, "workspace-transient-wait", &process);

    let event = wait_for_exit(&registration, Duration::from_secs(1));

    assert_eq!(event.reason, HolderExitReason::Unexpected);
    assert_eq!(process.wait_errors_remaining.load(Ordering::SeqCst), 0);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert!(process.try_wait_calls.load(Ordering::SeqCst) >= 2);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
}

#[test]
fn permanent_try_wait_error_fails_closed_and_uses_one_blocking_reap_owner() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    process
        .wait_errors_remaining
        .store(usize::MAX, Ordering::SeqCst);
    let registration = registered(&supervisor, "workspace-persistent-wait", &process);

    let event = wait_for_exit(&registration, Duration::from_secs(1));
    assert_eq!(event.reason, HolderExitReason::WaitError);
    assert!(!registration.is_live());
    assert_eq!(supervisor.stats().wait_error_total, 1);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
    wait_for_count(&process.reaps, 1, Duration::from_secs(1));
    assert_eq!(process.signals.load(Ordering::SeqCst), 1);
    assert_eq!(process.blocking_wait_calls.load(Ordering::SeqCst), 1);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(process.dropped_unreaped.load(Ordering::SeqCst), 0);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
}

#[test]
fn transient_blocking_wait_errors_are_bounded_and_eventually_reap_once() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    process
        .wait_errors_remaining
        .store(usize::MAX, Ordering::SeqCst);
    process
        .blocking_wait_errors_remaining
        .store(2, Ordering::SeqCst);
    let registration = registered(&supervisor, "workspace-blocking-retry", &process);

    drop(supervisor);

    assert_eq!(process.blocking_wait_calls.load(Ordering::SeqCst), 3);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(process.dropped_unreaped.load(Ordering::SeqCst), 0);
    assert_eq!(
        registration
            .exit_event()
            .expect("shutdown finalizes the registration")
            .reason,
        HolderExitReason::Destroy
    );
}

#[test]
fn blocking_reap_failure_retains_owner_and_retries_without_duplicate_event() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    process
        .wait_errors_remaining
        .store(usize::MAX, Ordering::SeqCst);
    process
        .blocking_wait_errors_remaining
        .store(3, Ordering::SeqCst);
    let registration = registered(&supervisor, "workspace-blocking-failure", &process);

    let started = Instant::now();
    let event = wait_for_exit(&registration, Duration::from_secs(1));
    wait_for_count(&process.reaps, 1, Duration::from_secs(1));

    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(event.reason, HolderExitReason::WaitError);
    assert_eq!(process.blocking_wait_calls.load(Ordering::SeqCst), 4);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(process.dropped_unreaped.load(Ordering::SeqCst), 0);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
}

#[test]
fn repeated_and_concurrent_joins_share_one_reap_and_one_notification() {
    let supervisor = Arc::new(HolderSupervisor::new(Duration::from_millis(2), 8));
    let process = Arc::new(FakeProcessState::default());
    process.normal_exit.store(true, Ordering::SeqCst);
    let registration = registered(&supervisor, "workspace-join", &process);
    process.exited.store(true, Ordering::SeqCst);

    let left_supervisor = Arc::clone(&supervisor);
    let left_registration = registration.clone();
    let left = std::thread::spawn(move || {
        left_supervisor.terminate(&left_registration, Duration::from_millis(5))
    });
    let right_supervisor = Arc::clone(&supervisor);
    let right_registration = registration.clone();
    let right = std::thread::spawn(move || {
        right_supervisor.terminate(&right_registration, Duration::from_millis(5))
    });

    let left = left.join().expect("left destroy joins");
    let right = right.join().expect("right destroy joins");
    assert_eq!(left, right);
    assert_eq!(supervisor.terminate(&registration, Duration::ZERO), left);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.stats().holder_exit_total, 1);
}

#[test]
fn explicit_supervisor_shutdown_is_joinable_and_rejects_new_holders() {
    let supervisor = Arc::new(HolderSupervisor::new(Duration::from_millis(2), 8));
    let process = Arc::new(FakeProcessState::default());
    *process
        .delayed_exit_after_signal
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Duration::from_millis(25));
    let registration = registered(&supervisor, "workspace-supervisor-shutdown", &process);
    let start = Arc::new(Barrier::new(3));

    let left_supervisor = Arc::clone(&supervisor);
    let left_start = Arc::clone(&start);
    let left = std::thread::spawn(move || {
        left_start.wait();
        left_supervisor.shutdown()
    });
    let right_supervisor = Arc::clone(&supervisor);
    let right_start = Arc::clone(&start);
    let right = std::thread::spawn(move || {
        right_start.wait();
        right_supervisor.shutdown()
    });
    start.wait();

    let left = left.join().expect("left shutdown joins");
    let right = right.join().expect("right shutdown joins");

    assert_eq!(left, Ok(()));
    assert_eq!(right, left);
    assert_eq!(supervisor.shutdown(), left);
    assert_eq!(process.signals.load(Ordering::SeqCst), 1);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(process.dropped_unreaped.load(Ordering::SeqCst), 0);
    assert_eq!(
        registration
            .exit_event()
            .expect("shutdown publishes the exact holder exit")
            .reason,
        HolderExitReason::Destroy
    );
    assert_eq!(
        supervisor.spawn_process(
            WorkspaceSessionId("workspace-after-supervisor-shutdown".to_owned()),
            |_generation| Err("must not run".to_owned()),
        ),
        Err(HolderSupervisorError::Unavailable)
    );
}

#[test]
fn long_destroy_grace_does_not_delay_peer_exit_detection() {
    let supervisor = Arc::new(HolderSupervisor::new(Duration::from_millis(2), 8));
    let stubborn = Arc::new(FakeProcessState::default());
    stubborn.ignore_signals.store(true, Ordering::SeqCst);
    let stubborn_registration = registered(&supervisor, "workspace-stubborn", &stubborn);
    let peer = Arc::new(FakeProcessState::default());
    let peer_registration = registered(&supervisor, "workspace-peer", &peer);

    let destroy_supervisor = Arc::clone(&supervisor);
    let destroy_registration = stubborn_registration.clone();
    let destroy = std::thread::spawn(move || {
        destroy_supervisor.terminate(&destroy_registration, Duration::from_secs(5))
    });
    wait_for_count(&stubborn.signals, 1, Duration::from_secs(1));

    peer.exited.store(true, Ordering::SeqCst);
    let peer_exit = wait_for_exit(&peer_registration, Duration::from_secs(1));
    assert_eq!(peer_exit.reason, HolderExitReason::Unexpected);

    stubborn.exited.store(true, Ordering::SeqCst);
    destroy
        .join()
        .expect("destroy thread joins")
        .expect("destroy observes the eventual exit");
    assert_eq!(stubborn.reaps.load(Ordering::SeqCst), 1);
    assert_eq!(peer.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn active_termination_accelerates_polling_without_changing_idle_cadence() {
    let idle_interval = Duration::from_millis(200);
    let supervisor = HolderSupervisor::new(idle_interval, 8);
    let process = Arc::new(FakeProcessState::default());
    *process
        .delayed_exit_after_signal
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Duration::from_millis(10));
    let registration = registered(&supervisor, "workspace-termination-aware-poll", &process);
    wait_for_count(&process.try_wait_calls, 1, Duration::from_secs(1));
    let idle_poll_count = process.try_wait_calls.load(Ordering::SeqCst);

    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        process.try_wait_calls.load(Ordering::SeqCst),
        idle_poll_count
    );

    let started = Instant::now();
    supervisor
        .terminate(&registration, Duration::from_secs(1))
        .expect("termination observes the delayed exit");

    assert!(started.elapsed() < Duration::from_millis(150));
    assert_eq!(process.signals.load(Ordering::SeqCst), 1);
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn pid_parent_start_time_and_executable_mismatches_refuse_signal() {
    let mutations: [fn(&mut HolderIdentity); 4] = [
        |identity| identity.pid += 1,
        |identity| identity.parent_pid += 1,
        |identity| identity.start_time_ticks += 1,
        |identity| identity.executable = PathBuf::from("/different/executable"),
    ];
    for (index, mutate) in mutations.into_iter().enumerate() {
        let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
        let process = Arc::new(FakeProcessState::default());
        let registration = registered(
            &supervisor,
            &format!("workspace-identity-{index}"),
            &process,
        );
        process.mutate_identity(mutate);

        let error = supervisor
            .terminate(&registration, Duration::from_millis(5))
            .expect_err("mismatched identity is rejected");

        assert!(matches!(
            error,
            HolderSupervisorError::IdentityMismatch { .. }
        ));
        assert_eq!(process.identity_checks.load(Ordering::SeqCst), 1);
        assert_eq!(process.signals.load(Ordering::SeqCst), 0);
        assert_eq!(process.reaps.load(Ordering::SeqCst), 0);
        assert_eq!(supervisor.stats().identity_mismatch_total, 1);

        process.set_identity(identity(41, 1));
        process.exited.store(true, Ordering::SeqCst);
        wait_for_exit(&registration, Duration::from_secs(1));
        assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn signal_is_preceded_immediately_by_exact_identity_validation() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 8);
    let process = Arc::new(FakeProcessState::default());
    let registration = registered(&supervisor, "workspace-signal-validation", &process);

    supervisor
        .terminate(&registration, Duration::from_millis(5))
        .expect("validated holder terminates");

    assert_eq!(process.identity_checks.load(Ordering::SeqCst), 1);
    assert_eq!(process.signals.load(Ordering::SeqCst), 1);
    assert!(process
        .signal_followed_identity_check
        .load(Ordering::SeqCst));
    assert_eq!(process.reaps.load(Ordering::SeqCst), 1);
}

#[test]
fn holder_event_history_is_bounded_and_counters_are_monotonic() {
    let supervisor = HolderSupervisor::new(Duration::from_millis(2), 2);
    for index in 0..3 {
        let process = Arc::new(FakeProcessState::default());
        let registration = registered(&supervisor, &format!("workspace-{index}"), &process);
        process.exited.store(true, Ordering::SeqCst);
        wait_for_exit(&registration, Duration::from_secs(1));
    }

    assert_eq!(supervisor.stats().holder_exit_total, 3);
    assert_eq!(supervisor.stats().dropped_event_total, 1);
    assert_eq!(supervisor.stats().retained_event_count, 2);
}

fn decrement_if_positive(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
}

fn identity(pid: i32, generation: u64) -> HolderIdentity {
    HolderIdentity {
        pid,
        parent_pid: 1,
        start_time_ticks: 1234,
        executable: PathBuf::from("/proc/self/exe"),
        generation,
        pidfd_available: true,
    }
}

impl FakeProcessState {
    fn set_identity(&self, identity: HolderIdentity) {
        *self
            .observed_identity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(identity);
    }

    fn mutate_identity(&self, mutate: impl FnOnce(&mut HolderIdentity)) {
        let mut identity = self
            .observed_identity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        mutate(identity.as_mut().expect("registered fake has an identity"));
    }
}

fn registered(
    supervisor: &HolderSupervisor,
    workspace: &str,
    process: &Arc<FakeProcessState>,
) -> HolderRegistration {
    let factory_process = Arc::clone(process);
    supervisor
        .spawn_process(
            WorkspaceSessionId(workspace.to_owned()),
            move |generation| {
                let identity = identity(41, generation);
                factory_process.set_identity(identity.clone());
                Ok((
                    identity,
                    Box::new(FakeProcess {
                        state: factory_process,
                    }) as Box<dyn HolderProcess>,
                ))
            },
        )
        .expect("fake holder spawns and registers")
}

fn wait_for_exit(registration: &HolderRegistration, timeout: Duration) -> HolderExitEvent {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(event) = registration.exit_event() {
            return event;
        }
        assert!(
            Instant::now() < deadline,
            "holder exit exceeded {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn wait_for_count(counter: &AtomicUsize, expected: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while counter.load(Ordering::SeqCst) < expected && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(counter.load(Ordering::SeqCst), expected);
}
