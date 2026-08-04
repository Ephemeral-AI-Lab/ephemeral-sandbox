//! Focused lifecycle coverage for the single namespace-runner completion owner.

#![allow(dead_code)]

include!("support/namespace_execution_src.rs");

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use launcher::RunnerChild;
use sandbox_runtime_namespace_process::runner::protocol::RunResult;
use supervisor::CompletionSupervisor;

struct ControlledChild {
    completes_after_terminate: bool,
    terminated: Arc<AtomicBool>,
    terminate_calls: Arc<AtomicUsize>,
    wait_polls: Arc<AtomicUsize>,
}

impl ControlledChild {
    fn completed(terminate_calls: Arc<AtomicUsize>, wait_polls: Arc<AtomicUsize>) -> Self {
        Self {
            completes_after_terminate: true,
            terminated: Arc::new(AtomicBool::new(true)),
            terminate_calls,
            wait_polls,
        }
    }

    fn responsive(terminate_calls: Arc<AtomicUsize>, wait_polls: Arc<AtomicUsize>) -> Self {
        Self {
            completes_after_terminate: true,
            terminated: Arc::new(AtomicBool::new(false)),
            terminate_calls,
            wait_polls,
        }
    }

    fn stuck(terminate_calls: Arc<AtomicUsize>, wait_polls: Arc<AtomicUsize>) -> Self {
        Self {
            completes_after_terminate: false,
            terminated: Arc::new(AtomicBool::new(false)),
            terminate_calls,
            wait_polls,
        }
    }
}

#[test]
fn completion_progress_does_not_depend_on_tokio_blocking_pool_capacity() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_time()
        .build()
        .expect("build multithread runtime");
    runtime.block_on(async {
        let (blocking_started_tx, blocking_started_rx) = mpsc::sync_channel(0);
        let (blocking_release_tx, blocking_release_rx) = mpsc::channel();
        let blocking_worker = tokio::task::spawn_blocking(move || {
            blocking_started_tx
                .send(())
                .expect("test observes occupied blocking worker");
            blocking_release_rx
                .recv()
                .expect("test releases occupied blocking worker");
        });
        blocking_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocking pool worker is occupied");

        let supervisor = CompletionSupervisor::new();
        assert_eq!(supervisor.worker_threads(), 1);

        let terminate_calls = Arc::new(AtomicUsize::new(0));
        let wait_polls = Arc::new(AtomicUsize::new(0));
        let (completion_tx, completion_rx) = mpsc::channel();
        supervisor
            .submit(
                Box::new(ControlledChild::completed(
                    Arc::clone(&terminate_calls),
                    Arc::clone(&wait_polls),
                )),
                move |result| completion_tx.send(result).expect("receive completion"),
            )
            .expect("child admitted");

        let completion = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match completion_rx.try_recv() {
                    Ok(result) => return result,
                    Err(mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        panic!("completion channel disconnected")
                    }
                }
            }
        })
        .await;

        blocking_release_tx
            .send(())
            .expect("release occupied blocking worker");
        blocking_worker
            .await
            .expect("blocking worker exits after release");

        let result = completion
            .expect("completion callback remains responsive")
            .expect("completed child succeeds");
        assert_eq!(result.exit_code, 130);
        assert_eq!(terminate_calls.load(Ordering::SeqCst), 0);
        assert!(wait_polls.load(Ordering::SeqCst) >= 1);
        assert_eq!(supervisor.active(), 0);
        assert_eq!(supervisor.worker_threads(), 1);

        supervisor
            .shutdown_and_join()
            .expect("dedicated completion worker shuts down cleanly");
        assert_eq!(supervisor.worker_threads(), 0);
    });
}

impl RunnerChild for ControlledChild {
    fn wait_completion(&mut self) -> Result<RunResult, NamespaceExecutionError> {
        loop {
            if let Some(result) = self.try_wait_completion()? {
                return Ok(result);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn try_wait_completion(&mut self) -> Result<Option<RunResult>, NamespaceExecutionError> {
        self.wait_polls.fetch_add(1, Ordering::SeqCst);
        if self.completes_after_terminate && self.terminated.load(Ordering::SeqCst) {
            Ok(Some(RunResult {
                exit_code: 130,
                payload: serde_json::json!({ "status": "cancelled" }),
            }))
        } else {
            Ok(None)
        }
    }

    fn terminate(&mut self) {
        self.terminate_calls.fetch_add(1, Ordering::SeqCst);
        self.terminated.store(true, Ordering::SeqCst);
    }
}

#[test]
fn completion_worker_remains_alive_until_explicit_shutdown() {
    let supervisor = CompletionSupervisor::new();
    assert_eq!(supervisor.worker_threads(), 1);

    thread::sleep(Duration::from_millis(300));
    assert_eq!(supervisor.worker_threads(), 1);

    supervisor
        .shutdown_and_join()
        .expect("idle completion worker shuts down cleanly");
    assert_eq!(supervisor.worker_threads(), 0);
}

#[test]
fn concurrent_shutdown_is_idempotent_and_joins_active_child_once() {
    let supervisor = Arc::new(CompletionSupervisor::new());
    let terminate_calls = Arc::new(AtomicUsize::new(0));
    let wait_polls = Arc::new(AtomicUsize::new(0));
    let (completion_tx, completion_rx) = mpsc::channel();
    supervisor
        .submit(
            Box::new(ControlledChild::responsive(
                Arc::clone(&terminate_calls),
                Arc::clone(&wait_polls),
            )),
            move |result| completion_tx.send(result).expect("receive completion"),
        )
        .expect("child admitted before shutdown");

    let barrier = Arc::new(Barrier::new(3));
    let mut shutdowns = Vec::new();
    for _ in 0..2 {
        let supervisor = Arc::clone(&supervisor);
        let barrier = Arc::clone(&barrier);
        shutdowns.push(thread::spawn(move || {
            barrier.wait();
            supervisor.shutdown_and_join()
        }));
    }
    barrier.wait();
    for shutdown in shutdowns {
        shutdown
            .join()
            .expect("shutdown caller does not panic")
            .expect("shutdown succeeds");
    }

    let result = completion_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("completion callback runs exactly once")
        .expect("responsive child completes");
    assert_eq!(result.exit_code, 130);
    assert_eq!(terminate_calls.load(Ordering::SeqCst), 1);
    assert!(wait_polls.load(Ordering::SeqCst) >= 1);
    assert_eq!(supervisor.active(), 0);
    assert_eq!(supervisor.worker_threads(), 0);
    supervisor
        .shutdown_and_join()
        .expect("later shutdown remains idempotent");
}

#[test]
fn submission_after_shutdown_is_rejected_reaped_and_never_completed() {
    let supervisor = CompletionSupervisor::new();
    supervisor
        .shutdown_and_join()
        .expect("initial shutdown succeeds");
    let terminate_calls = Arc::new(AtomicUsize::new(0));
    let wait_polls = Arc::new(AtomicUsize::new(0));
    let callback_calls = Arc::new(AtomicUsize::new(0));
    let callback_counter = Arc::clone(&callback_calls);

    let error = supervisor
        .submit(
            Box::new(ControlledChild::responsive(
                Arc::clone(&terminate_calls),
                Arc::clone(&wait_polls),
            )),
            move |_| {
                callback_counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect_err("late child is rejected");

    assert!(matches!(error, NamespaceExecutionError::Shutdown));
    assert_eq!(terminate_calls.load(Ordering::SeqCst), 1);
    assert_eq!(wait_polls.load(Ordering::SeqCst), 1);
    assert_eq!(callback_calls.load(Ordering::SeqCst), 0);
    assert_eq!(supervisor.active(), 0);
    assert_eq!(supervisor.worker_threads(), 0);
    supervisor
        .shutdown_and_join()
        .expect("repeated shutdown remains successful");
}

#[test]
fn stuck_child_reports_failure_without_unbounded_shutdown() {
    let supervisor = CompletionSupervisor::new();
    let terminate_calls = Arc::new(AtomicUsize::new(0));
    let wait_polls = Arc::new(AtomicUsize::new(0));
    let (completion_tx, completion_rx) = mpsc::channel();
    supervisor
        .submit(
            Box::new(ControlledChild::stuck(
                Arc::clone(&terminate_calls),
                Arc::clone(&wait_polls),
            )),
            move |result| completion_tx.send(result).expect("receive completion"),
        )
        .expect("child admitted before shutdown");

    let started = Instant::now();
    let error = supervisor
        .shutdown_and_join()
        .expect_err("unreaped child is surfaced");

    assert!(started.elapsed() < Duration::from_millis(1_500));
    assert!(matches!(
        error,
        NamespaceExecutionError::Completion(detail)
            if detail.contains("1 namespace runner(s)")
                && detail.contains("shutdown deadline")
    ));
    let completion_error = completion_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("timeout reaches completion callback")
        .expect_err("callback receives the join failure");
    assert!(matches!(
        completion_error,
        NamespaceExecutionError::Completion(detail)
            if detail.contains("timed out joining namespace runner")
    ));
    assert_eq!(terminate_calls.load(Ordering::SeqCst), 1);
    assert!(wait_polls.load(Ordering::SeqCst) >= 1);
    assert_eq!(supervisor.active(), 0);
    assert_eq!(supervisor.worker_threads(), 0);
}
