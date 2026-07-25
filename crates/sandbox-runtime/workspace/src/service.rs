use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard};
use std::time::Duration;

use crate::error::WorkspaceError;
use crate::model::WorkspaceOwnershipSnapshot;
use crate::namespace::holder::{
    HolderFinalization, HolderFinalizationUnknownClass, HolderProbe, HolderProbeUnknownClass,
};
use crate::session::{WorkspaceManager, WorkspaceManagerShutdownReport};

mod hooks;
mod impls;
mod support;

pub use hooks::WorkspaceRuntimeHooks;

/// Coalesced, bounded notification that at least one holder exit needs
/// operation-layer reconciliation. The notification carries no resource
/// ownership: the holder supervisor remains the sole `Child`/wait owner and
/// the session dispatcher merely enters the normal joinable teardown path.
#[doc(hidden)]
#[derive(Clone)]
pub struct HolderExitNotifier {
    tx: SyncSender<HolderExitDispatchSignal>,
}

/// The receiving half of the one daemon-wide holder-exit subscription.
#[doc(hidden)]
pub struct HolderExitListener {
    rx: Receiver<HolderExitDispatchSignal>,
}

/// Explicit shutdown control for the holder-exit dispatcher.
#[doc(hidden)]
pub struct HolderExitShutdown {
    tx: SyncSender<HolderExitDispatchSignal>,
}

/// A subscription is split exactly once between the blocking dispatcher and
/// its owning shutdown/join handle.
#[doc(hidden)]
pub struct HolderExitSubscription {
    listener: HolderExitListener,
    shutdown: HolderExitShutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HolderExitDispatchSignal {
    Wake,
    Shutdown,
}

/// Result of waiting on the holder-exit channel while an event-driven cleanup
/// retry is pending. A timeout is a retry deadline, not an idle poll.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderExitWait {
    Wake,
    RetryDeadline,
    Shutdown,
}

impl HolderExitNotifier {
    /// Queue one wake without blocking the reap owner. A full queue already
    /// contains a wake, so coalescing cannot lose reconciliation work.
    pub fn notify(&self) {
        match self.tx.try_send(HolderExitDispatchSignal::Wake) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

impl HolderExitListener {
    /// Blocks without polling. `false` is terminal shutdown/disconnection.
    #[must_use]
    pub fn wait(&self) -> bool {
        matches!(self.rx.recv(), Ok(HolderExitDispatchSignal::Wake))
    }

    /// Blocks until another holder event, shutdown, or the caller's bounded
    /// cleanup-retry deadline. This is used only after a teardown failure;
    /// the normal idle path remains an un-timed blocking receive.
    #[must_use]
    pub fn wait_for_retry(&self, timeout: Duration) -> HolderExitWait {
        match self.rx.recv_timeout(timeout) {
            Ok(HolderExitDispatchSignal::Wake) => HolderExitWait::Wake,
            Ok(HolderExitDispatchSignal::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                HolderExitWait::Shutdown
            }
            Err(RecvTimeoutError::Timeout) => HolderExitWait::RetryDeadline,
        }
    }
}

impl HolderExitShutdown {
    /// Stop is join-friendly even when a coalesced wake already occupies the
    /// one-slot queue: the bounded send waits only for the dispatcher to take
    /// that pending wake.
    pub fn stop(&self) {
        let _ = self.tx.send(HolderExitDispatchSignal::Shutdown);
    }
}

impl HolderExitSubscription {
    #[must_use]
    pub fn into_parts(self) -> (HolderExitListener, HolderExitShutdown) {
        (self.listener, self.shutdown)
    }
}

#[doc(hidden)]
#[must_use]
pub fn holder_exit_channel() -> (HolderExitNotifier, HolderExitSubscription) {
    let (tx, rx) = mpsc::sync_channel(1);
    (
        HolderExitNotifier { tx: tx.clone() },
        HolderExitSubscription {
            listener: HolderExitListener { rx },
            shutdown: HolderExitShutdown { tx },
        },
    )
}

pub struct WorkspaceRuntimeService {
    backend: WorkspaceRuntimeBackend,
    admission: RwLock<()>,
    shutdown_control: ShutdownControl,
}

/// Private LayerStack activation route used for new workspace admissions.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceStorageMode {
    Legacy,
    StrictCandidate {
        admission_lease_ttl: Duration,
        session_lease_ttl: Duration,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceRuntimeShutdownReport {
    pub workspaces: WorkspaceManagerShutdownReport,
    pub namespace_stopped: bool,
    pub namespace_error: Option<String>,
}

impl WorkspaceRuntimeShutdownReport {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.workspaces.is_complete() && self.namespace_stopped && self.namespace_error.is_none()
    }
}

#[derive(Default)]
struct ShutdownControl {
    state: Mutex<ShutdownState>,
}

#[derive(Default)]
struct ShutdownState {
    closing: bool,
    in_flight: Option<Arc<ShutdownRun>>,
    completed: Option<WorkspaceRuntimeShutdownReport>,
}

#[derive(Default)]
struct ShutdownRun {
    report: Mutex<Option<WorkspaceRuntimeShutdownReport>>,
    completed: Condvar,
}

enum ShutdownTurn {
    Complete(WorkspaceRuntimeShutdownReport),
    Join(Arc<ShutdownRun>),
    Lead(Arc<ShutdownRun>),
}

pub(crate) struct WorkspaceRuntimeState {
    pub(crate) manager: WorkspaceManager,
    pub(crate) layer_stack_root: PathBuf,
    pub(crate) storage_mode: WorkspaceStorageMode,
}

enum WorkspaceRuntimeBackend {
    Runtime(Box<Mutex<WorkspaceRuntimeState>>),
    Hooks(Box<WorkspaceRuntimeHooks>),
}

impl WorkspaceRuntimeService {
    #[must_use]
    pub fn new(manager: WorkspaceManager, layer_stack_root: PathBuf) -> Self {
        Self::new_with_storage_mode(manager, layer_stack_root, WorkspaceStorageMode::Legacy)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn new_with_storage_mode(
        mut manager: WorkspaceManager,
        layer_stack_root: PathBuf,
        storage_mode: WorkspaceStorageMode,
    ) -> Self {
        manager.bind_layer_stack_root(layer_stack_root.clone());
        Self {
            backend: WorkspaceRuntimeBackend::Runtime(Box::new(Mutex::new(
                WorkspaceRuntimeState {
                    manager,
                    layer_stack_root,
                    storage_mode,
                },
            ))),
            admission: RwLock::new(()),
            shutdown_control: ShutdownControl::default(),
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn from_hooks_for_test(hooks: WorkspaceRuntimeHooks) -> Self {
        Self {
            backend: WorkspaceRuntimeBackend::Hooks(Box::new(hooks)),
            admission: RwLock::new(()),
            shutdown_control: ShutdownControl::default(),
        }
    }

    pub(crate) fn hooks(&self) -> Option<&WorkspaceRuntimeHooks> {
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(_) => None,
            WorkspaceRuntimeBackend::Hooks(hooks) => Some(hooks.as_ref()),
        }
    }

    /// The isolated-network IP of a mounted workspace session, when it has one.
    /// Reads live session state; shared or veth-less workspaces yield `None`.
    ///
    /// # Errors
    /// Returns an error when the runtime state lock cannot be taken.
    pub fn isolated_ip(
        &self,
        workspace_id: &crate::model::WorkspaceSessionId,
    ) -> Result<Option<std::net::Ipv4Addr>, WorkspaceError> {
        let _admission = self.admit_work()?;
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(_) => {
                Ok(self.lock_state()?.manager.isolated_ip(workspace_id))
            }
            WorkspaceRuntimeBackend::Hooks(hooks) => (hooks.isolated_ip)(workspace_id),
        }
    }

    /// Returns bounded in-memory ownership accounting for open and retryable
    /// workspace teardown state.
    ///
    /// # Errors
    /// Returns an error when the concrete runtime state lock cannot be taken.
    pub fn ownership_snapshot(&self) -> Result<WorkspaceOwnershipSnapshot, WorkspaceError> {
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(_) => {
                Ok(self.lock_state()?.manager.ownership_snapshot())
            }
            WorkspaceRuntimeBackend::Hooks(_) => Ok(WorkspaceOwnershipSnapshot::default()),
        }
    }

    #[doc(hidden)]
    pub fn commit_workspace_destroy(&self, handle: &crate::model::WorkspaceHandle) {
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(state) => state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .manager
                .forget_completed_teardown(handle),
            WorkspaceRuntimeBackend::Hooks(hooks) => (hooks.commit_workspace_destroy)(handle),
        }
    }

    /// Returns whether the exact holder generation associated with `handle`
    /// remains live.
    #[must_use]
    pub fn holder_is_live(&self, handle: &crate::model::WorkspaceHandle) -> bool {
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(_) => handle.holder_is_live(),
            WorkspaceRuntimeBackend::Hooks(hooks) => (hooks.holder_is_live)(handle),
        }
    }

    /// Ask the stable holder supervisor to observe the exact generation at a
    /// finalization boundary.
    #[must_use]
    pub fn probe_holder(&self, handle: &crate::model::WorkspaceHandle) -> HolderProbe {
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(state) => {
                let runtime = match state.lock() {
                    Ok(state) => Arc::clone(&state.manager.runtime),
                    Err(_) => {
                        return HolderProbe::Unknown {
                            class: HolderProbeUnknownClass::Unavailable,
                        }
                    }
                };
                runtime.probe_holder(handle.holder_registration())
            }
            WorkspaceRuntimeBackend::Hooks(hooks) => (hooks.holder_probe)(handle),
        }
    }

    /// Ask the stable holder supervisor to linearize finalization against the
    /// exact holder generation and return only after planned teardown reaps it.
    #[must_use]
    pub fn quiesce_holder_for_finalization(
        &self,
        handle: &crate::model::WorkspaceHandle,
    ) -> HolderFinalization {
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(state) => {
                let runtime = match state.lock() {
                    Ok(state) => Arc::clone(&state.manager.runtime),
                    Err(_) => {
                        return HolderFinalization::Unknown {
                            class: HolderFinalizationUnknownClass::Unavailable,
                        }
                    }
                };
                runtime.quiesce_holder_for_finalization(handle.holder_registration())
            }
            WorkspaceRuntimeBackend::Hooks(hooks) => (hooks.holder_finalization)(handle),
        }
    }

    /// Returns the bounded exit reason for the exact holder generation, when
    /// that generation has exited.
    #[must_use]
    pub fn holder_exit_reason(&self, handle: &crate::model::WorkspaceHandle) -> Option<String> {
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(_) => handle.holder_exit_reason(),
            WorkspaceRuntimeBackend::Hooks(hooks) => (hooks.holder_exit_reason)(handle),
        }
    }

    /// Takes the sole process-wide holder-exit subscription. Concrete
    /// runtimes always provide one; hook runtimes may omit it when a test does
    /// not exercise live supervision.
    #[doc(hidden)]
    pub fn take_holder_exit_subscription(
        &self,
    ) -> Result<Option<HolderExitSubscription>, WorkspaceError> {
        let _admission = self.admit_work()?;
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(_) => self
                .lock_state()?
                .manager
                .take_holder_exit_subscription()
                .map(Some)
                .map_err(|message| WorkspaceError::Setup { step: message }),
            WorkspaceRuntimeBackend::Hooks(hooks) => (hooks.take_holder_exit_subscription)(),
        }
    }

    pub(crate) fn lock_state(
        &self,
    ) -> Result<MutexGuard<'_, WorkspaceRuntimeState>, WorkspaceError> {
        match &self.backend {
            WorkspaceRuntimeBackend::Runtime(state) => {
                state.lock().map_err(|_| WorkspaceError::Setup {
                    step: "workspace runtime state lock poisoned".to_owned(),
                })
            }
            WorkspaceRuntimeBackend::Hooks(_) => Err(WorkspaceError::Setup {
                step: "workspace runtime hooks do not expose concrete state".to_owned(),
            }),
        }
    }

    pub(crate) fn admit_work(&self) -> Result<RwLockReadGuard<'_, ()>, WorkspaceError> {
        let admission = self.admission.read().map_err(|_| WorkspaceError::Setup {
            step: "workspace admission lock poisoned".to_owned(),
        })?;
        if self.shutdown_control.is_closing() {
            return Err(WorkspaceError::Closing);
        }
        Ok(admission)
    }

    #[must_use]
    pub fn shutdown(&self) -> WorkspaceRuntimeShutdownReport {
        match self.shutdown_control.begin() {
            ShutdownTurn::Complete(report) => report,
            ShutdownTurn::Join(run) => ShutdownControl::wait(&run),
            ShutdownTurn::Lead(run) => {
                let _admission = self
                    .admission
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let report = catch_unwind(AssertUnwindSafe(|| self.perform_shutdown()))
                    .unwrap_or_else(|_| WorkspaceRuntimeShutdownReport {
                        workspaces: WorkspaceManagerShutdownReport::default(),
                        namespace_stopped: false,
                        namespace_error: Some(
                            "workspace shutdown panicked before namespace convergence".to_owned(),
                        ),
                    });
                self.shutdown_control.finish(&run, report.clone());
                report
            }
        }
    }

    fn perform_shutdown(&self) -> WorkspaceRuntimeShutdownReport {
        match &self.backend {
            WorkspaceRuntimeBackend::Hooks(_) => WorkspaceRuntimeShutdownReport {
                workspaces: WorkspaceManagerShutdownReport::default(),
                namespace_stopped: true,
                namespace_error: None,
            },
            WorkspaceRuntimeBackend::Runtime(state) => {
                let (workspaces, runtime) = {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let runtime = Arc::clone(&state.manager.runtime);
                    (state.manager.shutdown_all(), runtime)
                };
                if !workspaces.is_complete() {
                    return WorkspaceRuntimeShutdownReport {
                        workspaces,
                        namespace_stopped: false,
                        namespace_error: None,
                    };
                }
                match runtime.shutdown() {
                    Ok(()) => WorkspaceRuntimeShutdownReport {
                        workspaces,
                        namespace_stopped: true,
                        namespace_error: None,
                    },
                    Err(namespace_error) => WorkspaceRuntimeShutdownReport {
                        workspaces,
                        namespace_stopped: false,
                        namespace_error: Some(namespace_error),
                    },
                }
            }
        }
    }
}

impl ShutdownControl {
    fn begin(&self) -> ShutdownTurn {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.closing = true;
        if let Some(report) = &state.completed {
            return ShutdownTurn::Complete(report.clone());
        }
        if let Some(run) = &state.in_flight {
            return ShutdownTurn::Join(Arc::clone(run));
        }
        let run = Arc::new(ShutdownRun::default());
        state.in_flight = Some(Arc::clone(&run));
        ShutdownTurn::Lead(run)
    }

    fn is_closing(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .closing
    }

    fn wait(run: &ShutdownRun) -> WorkspaceRuntimeShutdownReport {
        let mut report = run
            .report
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(report) = &*report {
                return report.clone();
            }
            report = run
                .completed
                .wait(report)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn finish(&self, run: &ShutdownRun, report: WorkspaceRuntimeShutdownReport) {
        *run.report
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(report.clone());
        run.completed.notify_all();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if report.is_complete() {
            state.completed = Some(report);
        }
        state.in_flight = None;
    }
}

impl Drop for WorkspaceRuntimeService {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
