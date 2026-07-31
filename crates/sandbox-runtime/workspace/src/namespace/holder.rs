use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
#[cfg(all(target_os = "linux", unix))]
use std::os::unix::process::ExitStatusExt;
#[cfg(target_os = "linux")]
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};

#[cfg(target_os = "linux")]
use nix::fcntl::OFlag;
#[cfg(target_os = "linux")]
use nix::unistd::pipe2;
#[cfg(target_os = "linux")]
use rustix::process::{pidfd_open, pidfd_send_signal, Pid, PidfdFlags, Signal};

use crate::model::{WorkspaceHolderIdentity, WorkspaceSessionId};
use crate::service::{holder_exit_channel, HolderExitNotifier, HolderExitSubscription};
use crate::session::{MountedWorkspace, WorkspaceManagerError};

#[cfg(target_os = "linux")]
use super::fds::{clear_cloexec, expect_line, set_nonblocking};
#[cfg(target_os = "linux")]
use super::setup_error;
use super::{HolderKillReport, NamespacePlan, NamespaceRuntime};

const SUPERVISOR_QUEUE_CAPACITY: usize = 64;
const HOLDER_PROBE_REPLY_TIMEOUT: Duration = Duration::from_millis(250);
const HOLDER_FINALIZATION_REPLY_TIMEOUT: Duration = Duration::from_millis(1_500);
const KILL_REAP_TIMEOUT: Duration = Duration::from_secs(1);
const TERMINATION_POLL_INTERVAL: Duration = Duration::from_millis(5);
const WAIT_REAP_RETRY_BACKOFF: Duration = Duration::from_millis(50);
const WAIT_ERROR_LIMIT: u8 = 3;
#[cfg(target_os = "linux")]
const HOLDER_STDERR_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct HolderIdentity {
    pub(crate) pid: i32,
    pub(crate) parent_pid: i32,
    pub(crate) start_time_ticks: u64,
    pub(crate) executable: PathBuf,
    pub(crate) generation: u64,
    pub(crate) pidfd_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HolderProcessExit {
    pub(crate) exit_status: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) status_raw: Option<i32>,
}

impl HolderProcessExit {
    const fn unknown() -> Self {
        Self {
            exit_status: None,
            signal: None,
            status_raw: None,
        }
    }

    fn report(self, holder_was_alive: bool) -> HolderKillReport {
        HolderKillReport {
            holder_was_alive,
            exit_status: self.exit_status,
            signal: self.signal,
            status_raw: self.status_raw,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HolderSignal {
    Terminate,
    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HolderExitReason {
    Unexpected,
    WaitError,
    Destroy,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderProbe {
    Running,
    Exited,
    Unknown { class: HolderProbeUnknownClass },
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderProbeUnknownClass {
    ObservationFailed,
    NotRegistered,
    Overloaded,
    Unavailable,
    TimedOut,
}

impl HolderProbeUnknownClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ObservationFailed => "holder_probe_observation_failed",
            Self::NotRegistered => "holder_probe_not_registered",
            Self::Overloaded => "holder_probe_overloaded",
            Self::Unavailable => "holder_probe_unavailable",
            Self::TimedOut => "holder_probe_timed_out",
        }
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderFinalization {
    Quiesced {
        proof: HolderFinalizationProof,
    },
    Exited,
    Unknown {
        class: HolderFinalizationUnknownClass,
    },
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderFinalizationProof {
    pub workspace_session_id: WorkspaceSessionId,
    pub holder_identity: WorkspaceHolderIdentity,
    pub exit_sequence: u64,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderFinalizationUnknownClass {
    ObservationFailed,
    NotRegistered,
    IdentityMismatch,
    IdentityValidationFailed,
    TerminationInProgress,
    TerminationFailed,
    Overloaded,
    Unavailable,
    TimedOut,
}

impl HolderFinalizationUnknownClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ObservationFailed => "holder_finalization_observation_failed",
            Self::NotRegistered => "holder_finalization_not_registered",
            Self::IdentityMismatch => "holder_finalization_identity_mismatch",
            Self::IdentityValidationFailed => "holder_finalization_identity_validation_failed",
            Self::TerminationInProgress => "holder_finalization_termination_in_progress",
            Self::TerminationFailed => "holder_finalization_termination_failed",
            Self::Overloaded => "holder_finalization_overloaded",
            Self::Unavailable => "holder_finalization_unavailable",
            Self::TimedOut => "holder_finalization_timed_out",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HolderExitEvent {
    pub(crate) sequence: u64,
    pub(crate) workspace_session_id: WorkspaceSessionId,
    pub(crate) identity: HolderIdentity,
    pub(crate) reason: HolderExitReason,
    pub(crate) exit: HolderProcessExit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HolderSupervisorStats {
    pub(crate) holder_exit_total: u64,
    pub(crate) wait_error_total: u64,
    pub(crate) identity_mismatch_total: u64,
    pub(crate) dropped_event_total: u64,
    pub(crate) retained_event_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HolderSupervisorError {
    #[error("holder supervisor command queue is full")]
    Overloaded,
    #[error("holder supervisor is unavailable")]
    Unavailable,
    #[error("holder supervisor worker terminated unexpectedly")]
    WorkerTerminated,
    #[error("holder {workspace_session_id:?} is not registered for this generation")]
    Unknown {
        workspace_session_id: WorkspaceSessionId,
    },
    #[error(
        "holder pid identity changed for {workspace_session_id:?}: expected pid {expected_pid} generation {generation}"
    )]
    IdentityMismatch {
        workspace_session_id: WorkspaceSessionId,
        expected_pid: i32,
        generation: u64,
    },
    #[error("holder operation failed for {workspace_session_id:?}: {message}")]
    Process {
        workspace_session_id: WorkspaceSessionId,
        message: String,
    },
}

pub(crate) trait HolderProcess: Send {
    /// Nonblocking observation by the sole child owner. Implementations must
    /// not reap through any path outside this trait.
    fn try_wait(&mut self) -> Result<Option<HolderProcessExit>, String>;
    /// Final blocking reap used only after ownership can no longer be handed
    /// back to a live supervisor. The owner retries a bounded number of
    /// transient failures; implementations should turn an already-reaped
    /// kernel child (`ECHILD`) into a successful exit with unknown status.
    fn wait_reap(&mut self) -> Result<HolderProcessExit, String>;
    /// Revalidates fallback PID identity immediately before a raw-PID signal.
    fn identity_matches(&self, expected: &HolderIdentity) -> Result<bool, String>;
    fn send_signal(&mut self, signal: HolderSignal) -> Result<(), String>;
}

type HolderProcessFactory = Box<
    dyn FnOnce(u64) -> Result<(HolderIdentity, Box<dyn HolderProcess>), String> + Send + 'static,
>;

#[derive(Clone)]
pub struct HolderRegistration {
    workspace_session_id: WorkspaceSessionId,
    identity: HolderIdentity,
    exit: Arc<(Mutex<Option<HolderExitEvent>>, Condvar)>,
}

impl fmt::Debug for HolderRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HolderRegistration")
            .field("workspace_session_id", &self.workspace_session_id)
            .field("identity", &self.identity)
            .field("live", &self.is_live())
            .finish()
    }
}

impl PartialEq for HolderRegistration {
    fn eq(&self, other: &Self) -> bool {
        self.workspace_session_id == other.workspace_session_id
            && self.identity == other.identity
            && Arc::ptr_eq(&self.exit, &other.exit)
    }
}

impl Eq for HolderRegistration {}

impl HolderRegistration {
    #[doc(hidden)]
    pub fn unmanaged(workspace_session_id: WorkspaceSessionId, pid: i32) -> Self {
        Self {
            workspace_session_id,
            identity: HolderIdentity {
                pid,
                parent_pid: 0,
                start_time_ticks: 0,
                executable: PathBuf::new(),
                generation: 0,
                pidfd_available: false,
            },
            exit: Arc::new((Mutex::new(None), Condvar::new())),
        }
    }

    pub(crate) fn is_live(&self) -> bool {
        self.exit
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none()
    }

    pub(crate) fn identity_snapshot(&self) -> WorkspaceHolderIdentity {
        WorkspaceHolderIdentity {
            pid: self.identity.pid,
            parent_pid: self.identity.parent_pid,
            start_time_ticks: self.identity.start_time_ticks,
            executable: self.identity.executable.clone(),
            generation: self.identity.generation,
            pidfd_available: self.identity.pidfd_available,
        }
    }

    pub(crate) fn exit_event(&self) -> Option<HolderExitEvent> {
        self.exit
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn matches_finalization_proof(&self, proof: &HolderFinalizationProof) -> bool {
        if proof.workspace_session_id != self.workspace_session_id
            || proof.holder_identity != self.identity_snapshot()
        {
            return false;
        }
        self.exit_event().is_some_and(|event| {
            event.sequence == proof.exit_sequence
                && event.workspace_session_id == self.workspace_session_id
                && event.identity == self.identity
                && event.reason == HolderExitReason::Destroy
        })
    }

    fn finalization_proof(&self) -> Option<HolderFinalizationProof> {
        let event = self.exit_event()?;
        (event.workspace_session_id == self.workspace_session_id
            && event.identity == self.identity
            && event.reason == HolderExitReason::Destroy)
            .then(|| HolderFinalizationProof {
                workspace_session_id: self.workspace_session_id.clone(),
                holder_identity: self.identity_snapshot(),
                exit_sequence: event.sequence,
            })
    }
}

pub(crate) struct HolderSupervisor {
    lifecycle: Mutex<SupervisorLifecycle>,
    lifecycle_changed: Condvar,
    log: Arc<Mutex<SupervisorLog>>,
    next_generation: Arc<AtomicU64>,
}

struct SupervisorLifecycle {
    tx: Option<SyncSender<SupervisorCommand>>,
    worker: SupervisorWorker,
}

enum SupervisorWorker {
    Running(JoinHandle<()>),
    Joining,
    Stopped(Result<(), HolderSupervisorError>),
}

/// Owns the caller side of a pending registration until the caller has
/// actually received and claimed it. A bounded result channel alone is not
/// sufficient: `send` can succeed into its buffer immediately before the
/// receiver is dropped, leaving a live holder with no reachable owner.
pub(crate) struct HolderSpawnReply {
    result: Receiver<Result<HolderRegistration, HolderSupervisorError>>,
    claim: SyncSender<()>,
}

impl HolderSpawnReply {
    fn receive(self) -> Result<HolderRegistration, HolderSupervisorError> {
        let registration = self
            .result
            .recv()
            .map_err(|_| HolderSupervisorError::Unavailable)??;
        self.claim
            .send(())
            .map_err(|_| HolderSupervisorError::Unavailable)?;
        Ok(registration)
    }
}

impl fmt::Debug for HolderSupervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HolderSupervisor")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl HolderSupervisor {
    pub(crate) fn new(poll_interval: Duration, event_capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel(SUPERVISOR_QUEUE_CAPACITY);
        let log = Arc::new(Mutex::new(SupervisorLog::new(event_capacity)));
        let worker_log = Arc::clone(&log);
        let worker = thread::Builder::new()
            .name("eos-holder-supervisor".to_owned())
            .spawn(move || supervisor_loop(rx, worker_log, poll_interval))
            .expect("holder supervisor thread must start");
        Self {
            lifecycle: Mutex::new(SupervisorLifecycle {
                tx: Some(tx),
                worker: SupervisorWorker::Running(worker),
            }),
            lifecycle_changed: Condvar::new(),
            log,
            next_generation: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(crate) fn next_generation(&self) -> u64 {
        self.next_generation.fetch_add(1, Ordering::Relaxed)
    }

    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "Linux holder startup uses the synchronous entry point"
        )
    )]
    pub(crate) fn spawn_process<F>(
        &self,
        workspace_session_id: WorkspaceSessionId,
        factory: F,
    ) -> Result<HolderRegistration, HolderSupervisorError>
    where
        F: FnOnce(u64) -> Result<(HolderIdentity, Box<dyn HolderProcess>), String> + Send + 'static,
    {
        self.enqueue_spawn_process(workspace_session_id, factory)?
            .receive()
    }

    pub(crate) fn enqueue_spawn_process<F>(
        &self,
        workspace_session_id: WorkspaceSessionId,
        factory: F,
    ) -> Result<HolderSpawnReply, HolderSupervisorError>
    where
        F: FnOnce(u64) -> Result<(HolderIdentity, Box<dyn HolderProcess>), String> + Send + 'static,
    {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let (claim_tx, claim_rx) = mpsc::sync_channel(1);
        self.try_command(SupervisorCommand::Spawn {
            workspace_session_id,
            generation: self.next_generation(),
            factory: Box::new(factory),
            reply: reply_tx,
            claim: claim_rx,
        })?;
        Ok(HolderSpawnReply {
            result: reply_rx,
            claim: claim_tx,
        })
    }

    pub(crate) fn terminate(
        &self,
        registration: &HolderRegistration,
        grace: Duration,
    ) -> Result<HolderKillReport, HolderSupervisorError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.try_command(SupervisorCommand::Terminate {
            registration: registration.clone(),
            grace,
            reply: reply_tx,
        })?;
        reply_rx
            .recv()
            .map_err(|_| HolderSupervisorError::Unavailable)?
    }

    pub(crate) fn probe(&self, registration: &HolderRegistration) -> HolderProbe {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if let Err(error) = self.try_command(SupervisorCommand::Probe {
            registration: registration.clone(),
            reply: reply_tx,
        }) {
            return HolderProbe::Unknown {
                class: match error {
                    HolderSupervisorError::Overloaded => HolderProbeUnknownClass::Overloaded,
                    HolderSupervisorError::Unavailable
                    | HolderSupervisorError::WorkerTerminated
                    | HolderSupervisorError::Unknown { .. }
                    | HolderSupervisorError::IdentityMismatch { .. }
                    | HolderSupervisorError::Process { .. } => HolderProbeUnknownClass::Unavailable,
                },
            };
        }
        match reply_rx.recv_timeout(HOLDER_PROBE_REPLY_TIMEOUT) {
            Ok(probe) => probe,
            Err(RecvTimeoutError::Timeout) => HolderProbe::Unknown {
                class: HolderProbeUnknownClass::TimedOut,
            },
            Err(RecvTimeoutError::Disconnected) => HolderProbe::Unknown {
                class: HolderProbeUnknownClass::Unavailable,
            },
        }
    }

    pub(crate) fn quiesce_for_finalization(
        &self,
        registration: &HolderRegistration,
    ) -> HolderFinalization {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if let Err(error) = self.try_command(SupervisorCommand::QuiesceForFinalization {
            registration: registration.clone(),
            reply: reply_tx,
        }) {
            return HolderFinalization::Unknown {
                class: match error {
                    HolderSupervisorError::Overloaded => HolderFinalizationUnknownClass::Overloaded,
                    HolderSupervisorError::Unavailable
                    | HolderSupervisorError::WorkerTerminated
                    | HolderSupervisorError::Unknown { .. }
                    | HolderSupervisorError::IdentityMismatch { .. }
                    | HolderSupervisorError::Process { .. } => {
                        HolderFinalizationUnknownClass::Unavailable
                    }
                },
            };
        }
        match reply_rx.recv_timeout(HOLDER_FINALIZATION_REPLY_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => HolderFinalization::Unknown {
                class: HolderFinalizationUnknownClass::TimedOut,
            },
            Err(RecvTimeoutError::Disconnected) => HolderFinalization::Unknown {
                class: HolderFinalizationUnknownClass::Unavailable,
            },
        }
    }

    pub(crate) fn stats(&self) -> HolderSupervisorStats {
        self.log
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .stats
    }

    pub(crate) fn take_exit_subscription(&self) -> Result<HolderExitSubscription, String> {
        self.log
            .lock()
            .map_err(|_| "holder supervisor event log lock poisoned".to_owned())?
            .take_subscription()
    }

    pub(crate) fn shutdown(&self) -> Result<(), HolderSupervisorError> {
        let worker = {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                match &lifecycle.worker {
                    SupervisorWorker::Running(_) => {
                        lifecycle.tx.take();
                        let SupervisorWorker::Running(worker) =
                            std::mem::replace(&mut lifecycle.worker, SupervisorWorker::Joining)
                        else {
                            unreachable!("running supervisor worker was checked")
                        };
                        break worker;
                    }
                    SupervisorWorker::Joining => {
                        lifecycle = self
                            .lifecycle_changed
                            .wait(lifecycle)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                    }
                    SupervisorWorker::Stopped(result) => return result.clone(),
                }
            }
        };

        let result = worker
            .join()
            .map_err(|_| HolderSupervisorError::WorkerTerminated);
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        lifecycle.worker = SupervisorWorker::Stopped(result.clone());
        self.lifecycle_changed.notify_all();
        result
    }

    fn try_command(&self, command: SupervisorCommand) -> Result<(), HolderSupervisorError> {
        let tx = {
            let lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !matches!(&lifecycle.worker, SupervisorWorker::Running(_)) {
                return Err(HolderSupervisorError::Unavailable);
            }
            lifecycle
                .tx
                .clone()
                .ok_or(HolderSupervisorError::Unavailable)?
        };
        match tx.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(HolderSupervisorError::Overloaded),
            Err(TrySendError::Disconnected(_)) => Err(HolderSupervisorError::Unavailable),
        }
    }
}

impl Drop for HolderSupervisor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

enum SupervisorCommand {
    Spawn {
        workspace_session_id: WorkspaceSessionId,
        generation: u64,
        factory: HolderProcessFactory,
        reply: SyncSender<Result<HolderRegistration, HolderSupervisorError>>,
        claim: Receiver<()>,
    },
    Terminate {
        registration: HolderRegistration,
        grace: Duration,
        reply: SyncSender<Result<HolderKillReport, HolderSupervisorError>>,
    },
    Probe {
        registration: HolderRegistration,
        reply: SyncSender<HolderProbe>,
    },
    QuiesceForFinalization {
        registration: HolderRegistration,
        reply: SyncSender<HolderFinalization>,
    },
}

struct HolderRecord {
    registration: HolderRegistration,
    process: Box<dyn HolderProcess>,
    spawn_claim: Option<Receiver<()>>,
    unclaimed_cleanup_required: bool,
    termination: Option<TerminationAttempt>,
    destroy_requested: bool,
    consecutive_wait_errors: u8,
    wait_error_terminal: bool,
    wait_error_kill_attempted: bool,
}

struct TerminationAttempt {
    phase: TerminationPhase,
    waiters: Vec<TerminationWaiter>,
}

enum TerminationWaiter {
    Destroy(SyncSender<Result<HolderKillReport, HolderSupervisorError>>),
    Finalization(SyncSender<HolderFinalization>),
}

fn notify_termination_success(
    waiters: Vec<TerminationWaiter>,
    report: &HolderKillReport,
    finalization_proof: Option<HolderFinalizationProof>,
) {
    for waiter in waiters {
        match waiter {
            TerminationWaiter::Destroy(reply) => {
                let _ = reply.send(Ok(report.clone()));
            }
            TerminationWaiter::Finalization(reply) => {
                let result = finalization_proof.clone().map_or(
                    HolderFinalization::Unknown {
                        class: HolderFinalizationUnknownClass::TerminationFailed,
                    },
                    |proof| HolderFinalization::Quiesced { proof },
                );
                let _ = reply.send(result);
            }
        }
    }
}

fn notify_termination_failure(waiters: Vec<TerminationWaiter>, error: &HolderSupervisorError) {
    for waiter in waiters {
        match waiter {
            TerminationWaiter::Destroy(reply) => {
                let _ = reply.send(Err(error.clone()));
            }
            TerminationWaiter::Finalization(reply) => {
                let class = match error {
                    HolderSupervisorError::IdentityMismatch { .. } => {
                        HolderFinalizationUnknownClass::IdentityMismatch
                    }
                    HolderSupervisorError::Process { .. } => {
                        HolderFinalizationUnknownClass::TerminationFailed
                    }
                    HolderSupervisorError::Unknown { .. } => {
                        HolderFinalizationUnknownClass::NotRegistered
                    }
                    HolderSupervisorError::Overloaded => HolderFinalizationUnknownClass::Overloaded,
                    HolderSupervisorError::Unavailable
                    | HolderSupervisorError::WorkerTerminated => {
                        HolderFinalizationUnknownClass::Unavailable
                    }
                };
                let _ = reply.send(HolderFinalization::Unknown { class });
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TerminationPhase {
    Grace { deadline: Instant },
    Kill { deadline: Instant },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RegistrationKey {
    workspace_session_id: WorkspaceSessionId,
    identity: HolderIdentity,
}

impl From<&HolderRegistration> for RegistrationKey {
    fn from(registration: &HolderRegistration) -> Self {
        Self {
            workspace_session_id: registration.workspace_session_id.clone(),
            identity: registration.identity.clone(),
        }
    }
}

struct SupervisorLog {
    events: VecDeque<HolderExitEvent>,
    capacity: usize,
    next_sequence: u64,
    reconciliation_exit_total: u64,
    stats: HolderSupervisorStats,
    exit_notifier: Option<HolderExitNotifier>,
}

impl SupervisorLog {
    fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(capacity),
            capacity,
            next_sequence: 1,
            reconciliation_exit_total: 0,
            stats: HolderSupervisorStats::default(),
            exit_notifier: None,
        }
    }

    fn take_subscription(&mut self) -> Result<HolderExitSubscription, String> {
        if self.exit_notifier.is_some() {
            return Err("holder exit subscription already taken".to_owned());
        }
        let (notifier, subscription) = holder_exit_channel();
        if self.reconciliation_exit_total != 0 {
            notifier.notify();
        }
        self.exit_notifier = Some(notifier);
        Ok(subscription)
    }

    fn publish(
        &mut self,
        registration: &HolderRegistration,
        reason: HolderExitReason,
        exit: HolderProcessExit,
    ) -> HolderExitEvent {
        let event = HolderExitEvent {
            sequence: self.next_sequence,
            workspace_session_id: registration.workspace_session_id.clone(),
            identity: registration.identity.clone(),
            reason,
            exit,
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.stats.holder_exit_total = self.stats.holder_exit_total.saturating_add(1);
        if reason != HolderExitReason::Destroy {
            self.reconciliation_exit_total = self.reconciliation_exit_total.saturating_add(1);
            if self.capacity == 0 {
                self.stats.dropped_event_total = self.stats.dropped_event_total.saturating_add(1);
            } else {
                if self.events.len() == self.capacity {
                    self.events.pop_front();
                    self.stats.dropped_event_total =
                        self.stats.dropped_event_total.saturating_add(1);
                }
                self.events.push_back(event.clone());
            }
            self.stats.retained_event_count = self.events.len();
        }
        event
    }
}

fn supervisor_loop(
    rx: Receiver<SupervisorCommand>,
    log: Arc<Mutex<SupervisorLog>>,
    poll_interval: Duration,
) {
    let poll_interval = poll_interval.max(Duration::from_millis(1));
    let mut records: HashMap<RegistrationKey, HolderRecord> = HashMap::with_capacity(1);
    loop {
        if records.is_empty() {
            match rx.recv() {
                Ok(command) => handle_supervisor_command(command, &mut records, &log),
                Err(_) => break,
            }
        } else {
            match rx.recv_timeout(supervisor_poll_interval(&records, poll_interval)) {
                Ok(command) => handle_supervisor_command(command, &mut records, &log),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    shutdown_holder_records(&mut records, &log);
                    break;
                }
            }
        }
        poll_holder_records(&mut records, &log);
    }
}

fn supervisor_poll_interval(
    records: &HashMap<RegistrationKey, HolderRecord>,
    idle_interval: Duration,
) -> Duration {
    if records.values().any(|record| record.termination.is_some()) {
        idle_interval.min(TERMINATION_POLL_INTERVAL)
    } else {
        idle_interval
    }
}

/// The worker owns every accepted child until it has been reaped. If the
/// runtime itself is dropped while workspaces remain registered, disconnect
/// is therefore a teardown request rather than permission to drop `Child`
/// handles and orphan their processes.
fn shutdown_holder_records(
    records: &mut HashMap<RegistrationKey, HolderRecord>,
    log: &Arc<Mutex<SupervisorLog>>,
) {
    let keys = records.keys().cloned().collect::<Vec<_>>();
    for key in &keys {
        let completion = {
            let Some(record) = records.get_mut(key) else {
                continue;
            };
            match record.process.try_wait() {
                Ok(Some(exit)) => Some(finish_record(
                    record,
                    HolderExitReason::Destroy,
                    exit,
                    false,
                    log,
                )),
                Ok(None) => {
                    record.consecutive_wait_errors = 0;
                    None
                }
                Err(message) => {
                    observe_wait_error(record, &message, log);
                    None
                }
            }
        };
        if let Some(report) = completion {
            let (waiters, finalization_proof) = records.remove(key).map_or_else(
                || (Vec::new(), None),
                |mut record| {
                    let waiters = record
                        .termination
                        .take()
                        .map(|attempt| attempt.waiters)
                        .unwrap_or_default();
                    (waiters, record.registration.finalization_proof())
                },
            );
            notify_termination_success(waiters, &report, finalization_proof);
        }
    }

    for key in &keys {
        let Some(record) = records.get_mut(key) else {
            continue;
        };
        record.destroy_requested = true;
        if ensure_identity(record, log).is_ok() {
            let _ = record.process.send_signal(HolderSignal::Kill);
        }
    }

    for key in keys {
        let Some(mut record) = records.remove(&key) else {
            continue;
        };
        let waiters = record
            .termination
            .take()
            .map(|attempt| attempt.waiters)
            .unwrap_or_default();
        match wait_reap_owned(record.process.as_mut()) {
            Ok(exit) => {
                let report = finish_record(&record, HolderExitReason::Destroy, exit, true, log);
                let finalization_proof = record.registration.finalization_proof();
                notify_termination_success(waiters, &report, finalization_proof);
            }
            Err(message) => {
                publish_wait_error_terminal(&mut record, log);
                let error = process_error(&record, message);
                notify_termination_failure(waiters, &error);
            }
        }
    }
}

fn handle_supervisor_command(
    command: SupervisorCommand,
    records: &mut HashMap<RegistrationKey, HolderRecord>,
    log: &Arc<Mutex<SupervisorLog>>,
) {
    match command {
        SupervisorCommand::Spawn {
            workspace_session_id,
            generation,
            factory,
            reply,
            claim,
        } => {
            let factory_result = panic::catch_unwind(AssertUnwindSafe(|| factory(generation)));
            let result = match factory_result {
                Err(_) => Err(HolderSupervisorError::Process {
                    workspace_session_id,
                    message: "holder factory panicked".to_owned(),
                }),
                Ok(Err(message)) => Err(HolderSupervisorError::Process {
                    workspace_session_id,
                    message,
                }),
                Ok(Ok((identity, mut process))) if identity.generation != generation => {
                    abort_unregistered_process(process.as_mut(), &identity);
                    Err(HolderSupervisorError::Process {
                        workspace_session_id,
                        message: format!(
                            "holder factory returned generation {}, expected {generation}",
                            identity.generation
                        ),
                    })
                }
                Ok(Ok((identity, mut process))) => {
                    let registration = HolderRegistration {
                        workspace_session_id: workspace_session_id.clone(),
                        identity: identity.clone(),
                        exit: Arc::new((Mutex::new(None), Condvar::new())),
                    };
                    let key = RegistrationKey::from(&registration);
                    let duplicate_workspace = records.values().any(|record| {
                        record.registration.workspace_session_id == workspace_session_id
                    });
                    if records.contains_key(&key) || duplicate_workspace {
                        abort_unregistered_process(process.as_mut(), &identity);
                        Err(HolderSupervisorError::Process {
                            workspace_session_id,
                            message: "duplicate holder registration".to_owned(),
                        })
                    } else {
                        records.insert(
                            key,
                            HolderRecord {
                                registration: registration.clone(),
                                process,
                                spawn_claim: Some(claim),
                                unclaimed_cleanup_required: false,
                                termination: None,
                                destroy_requested: false,
                                consecutive_wait_errors: 0,
                                wait_error_terminal: false,
                                wait_error_kill_attempted: false,
                            },
                        );
                        Ok(registration)
                    }
                }
            };
            let _ = reply.send(result);
        }
        SupervisorCommand::Terminate {
            registration,
            grace,
            reply,
        } => {
            let key = RegistrationKey::from(&registration);
            let Some(record) = records.get_mut(&key) else {
                let result = registration.exit_event().map_or_else(
                    || {
                        Err(HolderSupervisorError::Unknown {
                            workspace_session_id: registration.workspace_session_id.clone(),
                        })
                    },
                    |event| Ok(event.exit.report(false)),
                );
                let _ = reply.send(result);
                return;
            };
            if let Some(termination) = record.termination.as_mut() {
                termination.waiters.push(TerminationWaiter::Destroy(reply));
                return;
            }
            match start_termination(record, grace, log) {
                Ok(Some(report)) => {
                    records.remove(&key);
                    let _ = reply.send(Ok(report));
                }
                Ok(None) => {
                    record
                        .termination
                        .as_mut()
                        .expect("successful termination start installs state")
                        .waiters
                        .push(TerminationWaiter::Destroy(reply));
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            }
        }
        SupervisorCommand::Probe {
            registration,
            reply,
        } => {
            let key = RegistrationKey::from(&registration);
            if registration.exit_event().is_some() {
                let _ = reply.send(HolderProbe::Exited);
                return;
            }
            let Some(record) = records.get_mut(&key) else {
                let _ = reply.send(HolderProbe::Unknown {
                    class: HolderProbeUnknownClass::NotRegistered,
                });
                return;
            };
            if record.registration != registration {
                let _ = reply.send(HolderProbe::Unknown {
                    class: HolderProbeUnknownClass::NotRegistered,
                });
                return;
            }
            let (probe, completion) = match record.process.try_wait() {
                Ok(None) => {
                    record.consecutive_wait_errors = 0;
                    (HolderProbe::Running, None)
                }
                Ok(Some(exit)) => {
                    let reason = if record.destroy_requested {
                        HolderExitReason::Destroy
                    } else {
                        HolderExitReason::Unexpected
                    };
                    let holder_was_alive = record.destroy_requested;
                    let waiters = record
                        .termination
                        .take()
                        .map(|attempt| attempt.waiters)
                        .unwrap_or_default();
                    let report = finish_record(record, reason, exit, holder_was_alive, log);
                    (HolderProbe::Exited, Some((report, waiters)))
                }
                Err(message) => {
                    observe_wait_error(record, &message, log);
                    if record.wait_error_terminal {
                        (HolderProbe::Exited, None)
                    } else {
                        (
                            HolderProbe::Unknown {
                                class: HolderProbeUnknownClass::ObservationFailed,
                            },
                            None,
                        )
                    }
                }
            };
            if let Some((report, waiters)) = completion {
                records.remove(&key);
                let finalization_proof = registration.finalization_proof();
                notify_termination_success(waiters, &report, finalization_proof);
            }
            let _ = reply.send(probe);
        }
        SupervisorCommand::QuiesceForFinalization {
            registration,
            reply,
        } => {
            let key = RegistrationKey::from(&registration);
            if registration.exit_event().is_some() {
                let _ = reply.send(HolderFinalization::Exited);
                return;
            }
            let Some(record) = records.get_mut(&key) else {
                let _ = reply.send(HolderFinalization::Unknown {
                    class: HolderFinalizationUnknownClass::NotRegistered,
                });
                return;
            };
            if record.registration != registration {
                let _ = reply.send(HolderFinalization::Unknown {
                    class: HolderFinalizationUnknownClass::NotRegistered,
                });
                return;
            }
            if record.termination.is_some() || record.destroy_requested {
                let _ = reply.send(HolderFinalization::Unknown {
                    class: HolderFinalizationUnknownClass::TerminationInProgress,
                });
                return;
            }

            match record.process.try_wait() {
                Ok(Some(exit)) => {
                    let _ = finish_record(record, HolderExitReason::Unexpected, exit, false, log);
                    records.remove(&key);
                    let _ = reply.send(HolderFinalization::Exited);
                }
                Err(message) => {
                    observe_wait_error(record, &message, log);
                    let result = if record.wait_error_terminal {
                        HolderFinalization::Exited
                    } else {
                        HolderFinalization::Unknown {
                            class: HolderFinalizationUnknownClass::ObservationFailed,
                        }
                    };
                    let _ = reply.send(result);
                }
                Ok(None) => {
                    record.consecutive_wait_errors = 0;
                    if let Err(error) = ensure_identity(record, log) {
                        let class = match error {
                            HolderSupervisorError::IdentityMismatch { .. } => {
                                HolderFinalizationUnknownClass::IdentityMismatch
                            }
                            HolderSupervisorError::Process { .. } => {
                                HolderFinalizationUnknownClass::IdentityValidationFailed
                            }
                            HolderSupervisorError::Overloaded => {
                                HolderFinalizationUnknownClass::Overloaded
                            }
                            HolderSupervisorError::Unavailable
                            | HolderSupervisorError::WorkerTerminated => {
                                HolderFinalizationUnknownClass::Unavailable
                            }
                            HolderSupervisorError::Unknown { .. } => {
                                HolderFinalizationUnknownClass::NotRegistered
                            }
                        };
                        let _ = reply.send(HolderFinalization::Unknown { class });
                        return;
                    }

                    record.destroy_requested = true;
                    match record.process.send_signal(HolderSignal::Terminate) {
                        Ok(()) => {
                            record.termination = Some(TerminationAttempt {
                                phase: TerminationPhase::Grace {
                                    deadline: Instant::now(),
                                },
                                waiters: vec![TerminationWaiter::Finalization(reply)],
                            });
                        }
                        Err(_) => match record.process.try_wait() {
                            Ok(Some(exit)) => {
                                let _ = finish_record(
                                    record,
                                    HolderExitReason::Destroy,
                                    exit,
                                    true,
                                    log,
                                );
                                let result = record.registration.finalization_proof().map_or(
                                    HolderFinalization::Unknown {
                                        class: HolderFinalizationUnknownClass::TerminationFailed,
                                    },
                                    |proof| HolderFinalization::Quiesced { proof },
                                );
                                records.remove(&key);
                                let _ = reply.send(result);
                            }
                            Ok(None) => {
                                record.destroy_requested = false;
                                let _ = reply.send(HolderFinalization::Unknown {
                                    class: HolderFinalizationUnknownClass::TerminationFailed,
                                });
                            }
                            Err(message) => {
                                observe_wait_error(record, &message, log);
                                record.destroy_requested = false;
                                let result = if record.wait_error_terminal {
                                    HolderFinalization::Exited
                                } else {
                                    HolderFinalization::Unknown {
                                        class: HolderFinalizationUnknownClass::TerminationFailed,
                                    }
                                };
                                let _ = reply.send(result);
                            }
                        },
                    }
                }
            }
        }
    }
}

fn start_termination(
    record: &mut HolderRecord,
    grace: Duration,
    log: &Arc<Mutex<SupervisorLog>>,
) -> Result<Option<HolderKillReport>, HolderSupervisorError> {
    match record.process.try_wait() {
        Ok(Some(exit)) => {
            return Ok(Some(finish_record(
                record,
                HolderExitReason::Destroy,
                exit,
                false,
                log,
            )));
        }
        Ok(None) => record.consecutive_wait_errors = 0,
        Err(message) => observe_wait_error(record, &message, log),
    }
    ensure_identity(record, log)?;
    record
        .process
        .send_signal(HolderSignal::Terminate)
        .map_err(|message| process_error(record, message))?;
    record.destroy_requested = true;
    record.termination = Some(TerminationAttempt {
        phase: TerminationPhase::Grace {
            deadline: Instant::now() + grace,
        },
        waiters: Vec::new(),
    });
    Ok(None)
}

enum PollCompletion {
    None,
    Exited {
        report: HolderKillReport,
        waiters: Vec<TerminationWaiter>,
        finalization_proof: Option<HolderFinalizationProof>,
    },
    AttemptFailed {
        error: HolderSupervisorError,
        waiters: Vec<TerminationWaiter>,
    },
}

fn poll_holder_records(
    records: &mut HashMap<RegistrationKey, HolderRecord>,
    log: &Arc<Mutex<SupervisorLog>>,
) {
    let keys = records.keys().cloned().collect::<Vec<_>>();
    for key in keys {
        let completion = {
            let Some(record) = records.get_mut(&key) else {
                continue;
            };
            poll_holder_record(record, log)
        };
        match completion {
            PollCompletion::None => {}
            PollCompletion::Exited {
                report,
                waiters,
                finalization_proof,
            } => {
                records.remove(&key);
                notify_termination_success(waiters, &report, finalization_proof);
            }
            PollCompletion::AttemptFailed { error, waiters } => {
                notify_termination_failure(waiters, &error);
            }
        }
    }
}

fn poll_holder_record(
    record: &mut HolderRecord,
    log: &Arc<Mutex<SupervisorLog>>,
) -> PollCompletion {
    observe_spawn_claim(record);
    let mut wait_failed = false;
    match record.process.try_wait() {
        Ok(Some(exit)) => {
            let reason = if record.destroy_requested {
                HolderExitReason::Destroy
            } else {
                HolderExitReason::Unexpected
            };
            let holder_was_alive = record.destroy_requested;
            let waiters = record
                .termination
                .take()
                .map(|attempt| attempt.waiters)
                .unwrap_or_default();
            let report = finish_record(record, reason, exit, holder_was_alive, log);
            let finalization_proof = record.registration.finalization_proof();
            return PollCompletion::Exited {
                report,
                waiters,
                finalization_proof,
            };
        }
        Ok(None) => record.consecutive_wait_errors = 0,
        Err(message) => {
            observe_wait_error(record, &message, log);
            wait_failed = true;
        }
    }

    if record.unclaimed_cleanup_required && record.termination.is_none() {
        match start_termination(record, Duration::ZERO, log) {
            Ok(Some(report)) => {
                record.unclaimed_cleanup_required = false;
                return PollCompletion::Exited {
                    report,
                    waiters: Vec::new(),
                    finalization_proof: record.registration.finalization_proof(),
                };
            }
            Ok(None) => {
                record.unclaimed_cleanup_required = false;
                return PollCompletion::None;
            }
            Err(error) => {
                // Keep ownership and retry on the next bounded poll. Dropping
                // the record after one identity/signal failure could orphan a
                // child that no caller can reach.
                return PollCompletion::AttemptFailed {
                    error,
                    waiters: Vec::new(),
                };
            }
        }
    }

    if wait_failed
        && record.wait_error_terminal
        && !record.wait_error_kill_attempted
        && record.termination.is_none()
    {
        if let Err(error) = ensure_identity(record, log).and_then(|()| {
            record
                .process
                .send_signal(HolderSignal::Kill)
                .map_err(|message| process_error(record, message))
        }) {
            record.wait_error_kill_attempted = true;
            return PollCompletion::AttemptFailed {
                error,
                waiters: Vec::new(),
            };
        }
        record.wait_error_kill_attempted = true;
        match wait_reap_owned(record.process.as_mut()) {
            Ok(exit) => {
                let report = finish_record(record, HolderExitReason::WaitError, exit, true, log);
                return PollCompletion::Exited {
                    report,
                    waiters: Vec::new(),
                    finalization_proof: record.registration.finalization_proof(),
                };
            }
            Err(_) => {
                record.termination = Some(TerminationAttempt {
                    phase: TerminationPhase::Kill {
                        deadline: Instant::now() + WAIT_REAP_RETRY_BACKOFF,
                    },
                    waiters: Vec::new(),
                });
            }
        }
    }

    let Some(phase) = record.termination.as_ref().map(|attempt| attempt.phase) else {
        return PollCompletion::None;
    };
    match phase {
        TerminationPhase::Grace { deadline } if Instant::now() >= deadline => {
            if let Err(error) = ensure_identity(record, log).and_then(|()| {
                record
                    .process
                    .send_signal(HolderSignal::Kill)
                    .map_err(|message| process_error(record, message))
            }) {
                let waiters = record
                    .termination
                    .take()
                    .map(|attempt| attempt.waiters)
                    .unwrap_or_default();
                return PollCompletion::AttemptFailed { error, waiters };
            }
            record
                .termination
                .as_mut()
                .expect("termination attempt still exists")
                .phase = TerminationPhase::Kill {
                deadline: Instant::now() + KILL_REAP_TIMEOUT,
            };
        }
        TerminationPhase::Kill { deadline } if Instant::now() >= deadline => {
            let waiters = record
                .termination
                .take()
                .map(|attempt| attempt.waiters)
                .unwrap_or_default();
            match wait_reap_owned(record.process.as_mut()) {
                Ok(exit) => {
                    let report = finish_record(record, HolderExitReason::Destroy, exit, true, log);
                    return PollCompletion::Exited {
                        report,
                        waiters,
                        finalization_proof: record.registration.finalization_proof(),
                    };
                }
                Err(message) => {
                    record.termination = Some(TerminationAttempt {
                        phase: TerminationPhase::Kill {
                            deadline: Instant::now() + WAIT_REAP_RETRY_BACKOFF,
                        },
                        waiters: Vec::new(),
                    });
                    return PollCompletion::AttemptFailed {
                        error: process_error(record, message),
                        waiters,
                    };
                }
            }
        }
        TerminationPhase::Grace { .. } | TerminationPhase::Kill { .. } => {}
    }
    PollCompletion::None
}

fn observe_spawn_claim(record: &mut HolderRecord) {
    let Some(claim) = record.spawn_claim.as_ref() else {
        return;
    };
    match claim.try_recv() {
        Ok(()) => record.spawn_claim = None,
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => {
            record.spawn_claim = None;
            record.unclaimed_cleanup_required = true;
            record.destroy_requested = true;
        }
    }
}

fn observe_wait_error(record: &mut HolderRecord, _message: &str, log: &Arc<Mutex<SupervisorLog>>) {
    record.consecutive_wait_errors = record.consecutive_wait_errors.saturating_add(1);
    if record.consecutive_wait_errors < WAIT_ERROR_LIMIT || record.wait_error_terminal {
        return;
    }

    publish_wait_error_terminal(record, log);
}

fn publish_wait_error_terminal(record: &mut HolderRecord, log: &Arc<Mutex<SupervisorLog>>) {
    if record.wait_error_terminal {
        return;
    }
    record.wait_error_terminal = true;
    publish_registration_exit(
        &record.registration,
        HolderExitReason::WaitError,
        HolderProcessExit::unknown(),
        log,
    );
}

fn ensure_identity(
    record: &HolderRecord,
    log: &Arc<Mutex<SupervisorLog>>,
) -> Result<(), HolderSupervisorError> {
    let matches = record
        .process
        .identity_matches(&record.registration.identity)
        .map_err(|message| process_error(record, message))?;
    if matches {
        return Ok(());
    }
    let mut log = log.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    log.stats.identity_mismatch_total = log.stats.identity_mismatch_total.saturating_add(1);
    Err(HolderSupervisorError::IdentityMismatch {
        workspace_session_id: record.registration.workspace_session_id.clone(),
        expected_pid: record.registration.identity.pid,
        generation: record.registration.identity.generation,
    })
}

fn process_error(record: &HolderRecord, message: String) -> HolderSupervisorError {
    HolderSupervisorError::Process {
        workspace_session_id: record.registration.workspace_session_id.clone(),
        message,
    }
}

fn finish_record(
    record: &HolderRecord,
    reason: HolderExitReason,
    exit: HolderProcessExit,
    holder_was_alive: bool,
    log: &Arc<Mutex<SupervisorLog>>,
) -> HolderKillReport {
    publish_registration_exit(&record.registration, reason, exit, log);
    exit.report(holder_was_alive)
}

fn publish_registration_exit(
    registration: &HolderRegistration,
    reason: HolderExitReason,
    exit: HolderProcessExit,
    log: &Arc<Mutex<SupervisorLog>>,
) {
    let notifier = {
        let mut slot = registration
            .exit
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_some() {
            return;
        }
        let mut log = log.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if reason == HolderExitReason::WaitError {
            log.stats.wait_error_total = log.stats.wait_error_total.saturating_add(1);
        }
        *slot = Some(log.publish(registration, reason, exit));
        (reason != HolderExitReason::Destroy)
            .then(|| log.exit_notifier.clone())
            .flatten()
    };
    registration.exit.1.notify_all();
    if let Some(notifier) = notifier {
        notifier.notify();
    }
}

/// A factory result that cannot be registered remains owned by the stable
/// supervisor thread until this bounded cleanup attempt has completed.
fn abort_unregistered_process(process: &mut dyn HolderProcess, identity: &HolderIdentity) {
    if matches!(process.try_wait(), Ok(Some(_))) {
        return;
    }
    if process.identity_matches(identity).unwrap_or(false) {
        let _ = process.send_signal(HolderSignal::Kill);
    }
    let _ = wait_reap_owned(process);
}

pub(super) fn wait_reap_owned(
    process: &mut dyn HolderProcess,
) -> Result<HolderProcessExit, String> {
    let mut last_error = String::new();
    for attempt in 1..=WAIT_ERROR_LIMIT {
        match process.wait_reap() {
            Ok(exit) => return Ok(exit),
            Err(error) => {
                last_error = error;
                if attempt < WAIT_ERROR_LIMIT {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
    Err(format!(
        "blocking holder reap failed after {WAIT_ERROR_LIMIT} attempts: {last_error}"
    ))
}

impl NamespaceRuntime {
    pub(crate) fn spawn_ns_holder(
        &self,
        handle: &mut MountedWorkspace,
        setup_timeout_s: f64,
        plan: NamespacePlan,
    ) -> Result<i32, WorkspaceManagerError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (setup_timeout_s, plan);
            handle.holder_registration =
                HolderRegistration::unmanaged(handle.workspace_id.clone(), 0);
            Ok(0)
        }
        #[cfg(target_os = "linux")]
        {
            let (readiness_read, readiness_write) = pipe2(OFlag::O_CLOEXEC).map_err(setup_error)?;
            let (control_read, control_write) = pipe2(OFlag::O_CLOEXEC).map_err(setup_error)?;
            let readiness_child_fd = readiness_write.as_raw_fd();
            let control_child_fd = control_read.as_raw_fd();
            clear_cloexec(readiness_child_fd)?;
            clear_cloexec(control_child_fd)?;
            let executable = std::env::current_exe().map_err(setup_error)?;
            let stderr = Arc::new(Mutex::new(None));
            let process_stderr = Arc::clone(&stderr);
            let network_arg = plan.network.holder_arg();
            let registration = self
                .holder_supervisor
                .spawn_process(handle.workspace_id.clone(), move |generation| {
                    spawn_linux_holder_process(
                        executable,
                        readiness_write,
                        control_read,
                        network_arg,
                        generation,
                        process_stderr,
                    )
                })
                .map_err(|error| ns_holder_spawn_error(error, &stderr))?;
            let holder_pid = registration.identity.pid;
            let readiness_fd = readiness_read.as_raw_fd();
            if let Err(error) = set_nonblocking(readiness_fd)
                .and_then(|()| expect_line(readiness_fd, b"ns-up", setup_timeout_s))
            {
                return Err(ns_holder_startup_error(
                    error,
                    &registration,
                    &self.holder_supervisor,
                    &stderr,
                ));
            }
            handle.readiness_fd = readiness_read.into_raw_fd();
            handle.control_fd = control_write.into_raw_fd();
            handle.holder_pid = holder_pid;
            handle.holder_registration = registration;
            Ok(holder_pid)
        }
    }

    pub(crate) fn kill_holder(
        &self,
        registration: &HolderRegistration,
        grace_s: f64,
    ) -> Result<HolderKillReport, WorkspaceManagerError> {
        if registration.identity.pid <= 0 {
            return Ok(HolderKillReport::default());
        }
        self.holder_supervisor
            .terminate(registration, Duration::from_secs_f64(grace_s.max(0.0)))
            .map_err(|error| WorkspaceManagerError::SetupFailed {
                step: error.to_string(),
            })
    }
}

#[cfg(target_os = "linux")]
struct LinuxHolderProcess {
    child: Child,
    identity: HolderIdentity,
    pidfd: Option<OwnedFd>,
    _stderr: Arc<Mutex<Option<ChildStderr>>>,
}

#[cfg(target_os = "linux")]
fn spawn_linux_holder_process(
    executable: PathBuf,
    readiness_write: OwnedFd,
    control_read: OwnedFd,
    network_arg: &'static str,
    generation: u64,
    stderr: Arc<Mutex<Option<ChildStderr>>>,
) -> Result<(HolderIdentity, Box<dyn HolderProcess>), String> {
    let readiness_child_fd = readiness_write.as_raw_fd();
    let control_child_fd = control_read.as_raw_fd();
    let mut child = Command::new(executable)
        .arg("ns-holder")
        .arg(readiness_child_fd.to_string())
        .arg(control_child_fd.to_string())
        .arg(network_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    *stderr
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = child.stderr.take();
    let holder_pid = match i32::try_from(child.id()) {
        Ok(holder_pid) => holder_pid,
        Err(_) => {
            let message = format!("ns-holder pid does not fit i32: {}", child.id());
            return Err(abort_spawned_linux_holder(message, &mut child));
        }
    };
    let (identity, pidfd) = match inspect_linux_holder(holder_pid, generation) {
        Ok(identity) => identity,
        Err(error) => return Err(abort_spawned_linux_holder(error.to_string(), &mut child)),
    };
    let process = LinuxHolderProcess {
        child,
        identity: identity.clone(),
        pidfd,
        _stderr: stderr,
    };
    Ok((identity, Box::new(process)))
}

#[cfg(target_os = "linux")]
fn abort_spawned_linux_holder(message: String, child: &mut Child) -> String {
    let _ = child.kill();
    let status = child.wait().ok();
    format!(
        "{message}; ns-holder {}",
        format_exit_status(status.as_ref())
    )
}

#[cfg(target_os = "linux")]
fn inspect_linux_holder(
    pid: i32,
    generation: u64,
) -> Result<(HolderIdentity, Option<OwnedFd>), WorkspaceManagerError> {
    let (parent_pid, start_time_ticks) = read_proc_stat(pid).map_err(setup_error)?;
    let executable = std::fs::read_link(format!("/proc/{pid}/exe")).map_err(setup_error)?;
    let pidfd = Pid::from_raw(pid).and_then(|pid| pidfd_open(pid, PidfdFlags::empty()).ok());
    let identity = HolderIdentity {
        pid,
        parent_pid,
        start_time_ticks,
        executable,
        generation,
        pidfd_available: pidfd.is_some(),
    };
    Ok((identity, pidfd))
}

#[cfg(target_os = "linux")]
impl HolderProcess for LinuxHolderProcess {
    fn try_wait(&mut self) -> Result<Option<HolderProcessExit>, String> {
        self.child
            .try_wait()
            .map(|status| status.map(process_exit))
            .map_err(|error| error.to_string())
    }

    fn wait_reap(&mut self) -> Result<HolderProcessExit, String> {
        match self.child.wait() {
            Ok(status) => Ok(process_exit(status)),
            Err(error) if error.raw_os_error() == Some(nix::libc::ECHILD) => {
                Ok(HolderProcessExit::unknown())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn identity_matches(&self, expected: &HolderIdentity) -> Result<bool, String> {
        if self.pidfd.is_some() {
            return Ok(self.identity == *expected);
        }
        let (parent_pid, start_time_ticks) = read_proc_stat(expected.pid)?;
        let executable = std::fs::read_link(format!("/proc/{}/exe", expected.pid))
            .map_err(|error| error.to_string())?;
        Ok(parent_pid == expected.parent_pid
            && start_time_ticks == expected.start_time_ticks
            && executable == expected.executable)
    }

    fn send_signal(&mut self, signal: HolderSignal) -> Result<(), String> {
        let signal = match signal {
            HolderSignal::Terminate => Signal::Term,
            HolderSignal::Kill => Signal::Kill,
        };
        if let Some(pidfd) = &self.pidfd {
            return pidfd_send_signal(pidfd, signal).map_err(|error| error.to_string());
        }
        let pid = Pid::from_raw(self.identity.pid)
            .ok_or_else(|| format!("invalid holder pid {}", self.identity.pid))?;
        rustix::process::kill_process(pid, signal).map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "linux")]
fn read_proc_stat(pid: i32) -> Result<(i32, u64), String> {
    let stat =
        std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|error| error.to_string())?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| format!("malformed /proc/{pid}/stat"))?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let parent_pid = fields
        .get(1)
        .ok_or_else(|| format!("missing ppid in /proc/{pid}/stat"))?
        .parse::<i32>()
        .map_err(|error| error.to_string())?;
    let start_time_ticks = fields
        .get(19)
        .ok_or_else(|| format!("missing starttime in /proc/{pid}/stat"))?
        .parse::<u64>()
        .map_err(|error| error.to_string())?;
    Ok((parent_pid, start_time_ticks))
}

#[cfg(all(target_os = "linux", unix))]
fn process_exit(status: std::process::ExitStatus) -> HolderProcessExit {
    HolderProcessExit {
        exit_status: status.code(),
        signal: status.signal(),
        status_raw: Some(status.into_raw()),
    }
}

#[cfg(target_os = "linux")]
fn ns_holder_spawn_error(
    error: HolderSupervisorError,
    stderr: &Arc<Mutex<Option<ChildStderr>>>,
) -> WorkspaceManagerError {
    let stderr = take_child_stderr(stderr);
    WorkspaceManagerError::SetupFailed {
        step: format!(
            "{error}; ns-holder stderr: {}",
            stderr_summary(&read_child_stderr(stderr))
        ),
    }
}

#[cfg(target_os = "linux")]
fn ns_holder_startup_error(
    error: WorkspaceManagerError,
    registration: &HolderRegistration,
    supervisor: &HolderSupervisor,
    stderr: &Arc<Mutex<Option<ChildStderr>>>,
) -> WorkspaceManagerError {
    let original_step = match error {
        WorkspaceManagerError::SetupFailed { step } => step,
        other => other.to_string(),
    };
    let cleanup = supervisor.terminate(registration, Duration::ZERO);
    let stderr = take_child_stderr(stderr);
    let stderr = match &cleanup {
        Ok(_) => stderr_summary(&read_child_stderr(stderr)),
        Err(_) => {
            drop(stderr);
            "<not read because holder cleanup failed>".to_owned()
        }
    };
    WorkspaceManagerError::SetupFailed {
        step: format!(
            "{original_step}; ns-holder cleanup: {}; stderr: {stderr}",
            cleanup
                .map(|report| format!("{report:?}"))
                .unwrap_or_else(|cleanup_error| format!("failed: {cleanup_error}")),
        ),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn ns_holder_runtime_error(
    error: WorkspaceManagerError,
    registration: &HolderRegistration,
    supervisor: &HolderSupervisor,
) -> Result<WorkspaceManagerError, WorkspaceManagerError> {
    let original_step = match error {
        WorkspaceManagerError::SetupFailed { step } => step,
        other => other.to_string(),
    };
    let cleanup = supervisor
        .terminate(registration, Duration::ZERO)
        .map(|report| format!("holder cleanup: {report:?}"))
        .unwrap_or_else(|cleanup_error| format!("holder cleanup failed: {cleanup_error}"));
    Ok(WorkspaceManagerError::SetupFailed {
        step: format!("{original_step}; {cleanup}"),
    })
}

#[cfg(target_os = "linux")]
fn take_child_stderr(stderr: &Arc<Mutex<Option<ChildStderr>>>) -> Option<ChildStderr> {
    stderr
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

#[cfg(target_os = "linux")]
fn read_child_stderr(stderr: Option<ChildStderr>) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut output = Vec::with_capacity(HOLDER_STDERR_LIMIT.min(4096));
    let _ = stderr
        .by_ref()
        .take((HOLDER_STDERR_LIMIT + 1) as u64)
        .read_to_end(&mut output);
    let truncated = output.len() > HOLDER_STDERR_LIMIT;
    output.truncate(HOLDER_STDERR_LIMIT);
    let mut output = String::from_utf8_lossy(&output).into_owned();
    if truncated {
        output.push_str("\n<truncated>");
    }
    output
}

#[cfg(target_os = "linux")]
fn stderr_summary(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        "<empty>".to_owned()
    } else {
        trimmed.replace('\n', " | ")
    }
}

#[cfg(target_os = "linux")]
fn format_exit_status(status: Option<&ExitStatus>) -> String {
    let Some(status) = status else {
        return "exit status unavailable".to_owned();
    };
    if let Some(code) = status.code() {
        return format!("exited with status {code}");
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return format!("terminated by signal {signal}");
    }
    status.to_string()
}
