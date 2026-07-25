use std::collections::{BTreeSet, HashMap};
use std::io::ErrorKind;
use std::path::Path;
use std::time::Instant;

use serde_json::{json, Value};

use crate::model::{NetworkProfile, WorkspaceSessionId};
use crate::namespace::HolderKillReport;
use crate::overlay::tree::TreeResourceStats;
use crate::session::manager::{
    WorkspaceManagerError, WorkspaceManagerShutdownReport, WorkspaceShutdownFailure,
};
use crate::session::{MountedWorkspace, WorkspaceManager};

use super::{monotonic_seconds, record_phase_ms};

/// Ordered, individually retryable teardown resources. A completed step is
/// never executed again, while failures do not prevent independent later
/// resources from being released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TeardownStep {
    Holder,
    Commands,
    NamespaceFds,
    Network,
    Mounts,
    Scratch,
    Leases,
    LeaseAccounting,
    Persistence,
}

impl TeardownStep {
    pub(crate) const ORDER: [Self; 9] = [
        Self::Holder,
        Self::Commands,
        Self::NamespaceFds,
        Self::Network,
        Self::Mounts,
        Self::Scratch,
        Self::Leases,
        Self::LeaseAccounting,
        Self::Persistence,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeardownFailure {
    pub(crate) step: TeardownStep,
    pub(crate) message: String,
}

pub(crate) trait TeardownStepExecutor {
    fn execute(&mut self, step: TeardownStep) -> Result<(), String>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TeardownLedger {
    completed: BTreeSet<TeardownStep>,
}

impl TeardownLedger {
    pub(crate) fn run(
        &mut self,
        executor: &mut impl TeardownStepExecutor,
    ) -> Result<(), Vec<TeardownFailure>> {
        let mut failures = Vec::new();
        for step in TeardownStep::ORDER {
            if self.completed.contains(&step) {
                continue;
            }
            match executor.execute(step) {
                Ok(()) => {
                    self.completed.insert(step);
                }
                Err(message) => failures.push(TeardownFailure { step, message }),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.completed.len() == TeardownStep::ORDER.len()
    }

    pub(crate) fn is_completed(&self, step: TeardownStep) -> bool {
        self.completed.contains(&step)
    }
}

/// A removed handle remains here until every teardown resource has reached a
/// terminal state. Retrying `close` resumes only failed steps; it never
/// re-signals a reaped holder, re-closes an fd, or repeats a successful
/// network/scratch release.
pub(crate) struct TeardownTransaction {
    handle: MountedWorkspace,
    ledger: TeardownLedger,
    started_at: Instant,
    grace_s: f64,
    upperdir_bytes: u64,
    holder_report: HolderKillReport,
    holder_error: Option<String>,
    holder_terminal: bool,
    mounts_released: bool,
    scratch_released: bool,
    candidate_lease_released: bool,
    lease_released: bool,
    parked_lease_released: bool,
    active_leases_after: Option<usize>,
    phases_ms: HashMap<String, f64>,
    ns_fd_count_before: usize,
    readiness_fd_was_open: bool,
    control_fd_was_open: bool,
    veth_host_name: Option<String>,
    veth_ns_name: Option<String>,
}

impl TeardownTransaction {
    fn new(handle: MountedWorkspace, grace_s: f64) -> Self {
        let upperdir_bytes = TreeResourceStats::collect(&handle.dirs.upperdir).bytes;
        Self {
            ns_fd_count_before: handle.ns_fds.len(),
            readiness_fd_was_open: handle.readiness_fd >= 0,
            control_fd_was_open: handle.control_fd >= 0,
            veth_host_name: handle.veth.as_ref().map(|veth| veth.host_name.clone()),
            veth_ns_name: handle.veth.as_ref().map(|veth| veth.ns_name.clone()),
            handle,
            ledger: TeardownLedger::default(),
            started_at: Instant::now(),
            grace_s,
            upperdir_bytes,
            holder_report: HolderKillReport::default(),
            holder_error: None,
            holder_terminal: false,
            mounts_released: false,
            scratch_released: false,
            candidate_lease_released: false,
            lease_released: false,
            parked_lease_released: false,
            active_leases_after: None,
            phases_ms: HashMap::new(),
        }
    }

    fn outcome(self, manager: &WorkspaceManager) -> ExitOutcome {
        debug_assert!(self.ledger.is_complete());
        let lifetime_s = (monotonic_seconds() - self.handle.created_at).max(0.0);
        let inspection = json!({
            "handle_registered_after": manager.handles.contains_key(&self.handle.workspace_id),
            "teardown_registered_after": manager.teardowns.contains_key(&self.handle.workspace_id),
            "open_handle_count_after": manager.handles.len(),
            "holder_pid": self.handle.holder_pid,
            "holder_was_alive": self.holder_report.holder_was_alive,
            "holder_exit_status": self.holder_report.exit_status,
            "holder_signal": self.holder_report.signal,
            "holder_status_raw": self.holder_report.status_raw,
            "holder_kill_error": self.holder_error,
            "ns_fd_count": self.ns_fd_count_before,
            "ns_fd_count_after": self.handle.ns_fds.len(),
            "readiness_fd_was_open": self.readiness_fd_was_open,
            "readiness_fd_is_open_after": self.handle.readiness_fd >= 0,
            "control_fd_was_open": self.control_fd_was_open,
            "control_fd_is_open_after": self.handle.control_fd >= 0,
            "veth_host_name": self.veth_host_name,
            "veth_ns_name": self.veth_ns_name,
            "veth_registered_after": self.handle.veth.is_some(),
            "scratch_dir": self.handle.dirs.run_dir.to_string_lossy(),
            "scratch_exists_after": self.handle.dirs.run_dir.exists(),
            "upperdir_exists_after": self.handle.dirs.upperdir.exists(),
            "workdir_exists_after": self.handle.dirs.workdir.exists(),
            "mountinfo_reference_count_after": mountinfo_reference_count(&[
                &self.handle.dirs.run_dir,
                &self.handle.dirs.upperdir,
                &self.handle.dirs.workdir,
            ]),
            "candidate_materialization_id": self.handle.candidate_admission.as_ref()
                .map(|admission| admission.selection.materialization_id.as_str()),
            "candidate_generation": self.handle.candidate_admission.as_ref()
                .map(|admission| admission.selection.generation),
            "candidate_fence": self.handle.candidate_admission.as_ref()
                .map(|admission| admission.selection.fence),
        });
        ExitOutcome {
            workspace_id: self.handle.workspace_id,
            lease_id: self.handle.snapshot.lease_id.0,
            parked_lease_id: self.handle.parked_lease_id,
            active_leases_after: self
                .active_leases_after
                .expect("completed teardown accounted active leases"),
            evicted_upperdir_bytes: self.upperdir_bytes,
            lifetime_s,
            total_ms: self.started_at.elapsed().as_secs_f64() * 1000.0,
            phases_ms: self.phases_ms,
            inspection,
        }
    }

    pub(crate) fn owned_handle(&self) -> &MountedWorkspace {
        &self.handle
    }

    pub(crate) fn has_persisted_handle(&self) -> bool {
        !self.ledger.is_completed(TeardownStep::Persistence)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExitOutcome {
    pub workspace_id: WorkspaceSessionId,
    pub lease_id: String,
    pub parked_lease_id: Option<String>,
    pub active_leases_after: usize,
    pub evicted_upperdir_bytes: u64,
    pub lifetime_s: f64,
    pub total_ms: f64,
    pub phases_ms: HashMap<String, f64>,
    pub inspection: Value,
}

impl WorkspaceManager {
    #[must_use]
    pub fn shutdown_all(&mut self) -> WorkspaceManagerShutdownReport {
        let mut workspace_ids = self
            .handles
            .keys()
            .chain(self.teardowns.keys())
            .cloned()
            .collect::<Vec<_>>();
        workspace_ids.sort_by(|left, right| left.0.cmp(&right.0));
        workspace_ids.dedup();

        let mut report = WorkspaceManagerShutdownReport {
            attempted_workspace_ids: workspace_ids.clone(),
            ..WorkspaceManagerShutdownReport::default()
        };
        for workspace_session_id in workspace_ids {
            match self.close(&workspace_session_id, None) {
                Ok(_) => report.closed_workspace_ids.push(workspace_session_id),
                Err(WorkspaceManagerError::TeardownFailed {
                    workspace_session_id,
                    failures,
                }) => report.retryable_failures.push(WorkspaceShutdownFailure {
                    workspace_session_id,
                    failures,
                }),
                Err(error) => report.retryable_failures.push(WorkspaceShutdownFailure {
                    workspace_session_id,
                    failures: vec![error.to_string()],
                }),
            }
        }
        report.remaining_workspace_ids = self
            .handles
            .keys()
            .chain(self.teardowns.keys())
            .cloned()
            .collect();
        report
            .remaining_workspace_ids
            .sort_by(|left, right| left.0.cmp(&right.0));
        report.remaining_workspace_ids.dedup();
        report
    }

    pub fn close(
        &mut self,
        workspace_id: &WorkspaceSessionId,
        grace_s: Option<f64>,
    ) -> Result<ExitOutcome, WorkspaceManagerError> {
        let transaction = if let Some(transaction) = self.teardowns.remove(workspace_id) {
            transaction
        } else {
            let Some(handle) = self.handles.remove(workspace_id) else {
                return Err(WorkspaceManagerError::NotOpen);
            };
            TeardownTransaction::new(handle, grace_s.unwrap_or(self.caps.exit_grace_s))
        };

        self.run_teardown_transaction(workspace_id.clone(), transaction)
    }

    /// Roll back a handle that was never published in the manager's active
    /// map. Failure retains the exact same transaction under its generated
    /// workspace id, making the cleanup visible and joinable even though
    /// `open` never returned a handle to the caller.
    pub(crate) fn rollback_unpublished(
        &mut self,
        handle: &MountedWorkspace,
    ) -> Result<ExitOutcome, WorkspaceManagerError> {
        self.run_teardown_transaction(
            handle.workspace_id.clone(),
            TeardownTransaction::new(handle.clone(), 1.0),
        )
    }

    fn run_teardown_transaction(
        &mut self,
        workspace_id: WorkspaceSessionId,
        mut transaction: TeardownTransaction,
    ) -> Result<ExitOutcome, WorkspaceManagerError> {
        let mut ledger = std::mem::take(&mut transaction.ledger);
        let result = {
            let mut executor = ManagerTeardownExecutor {
                manager: self,
                transaction: &mut transaction,
                resource_failed: false,
            };
            ledger.run(&mut executor)
        };
        transaction.ledger = ledger;

        if let Err(failures) = result {
            let mut failures = failures
                .into_iter()
                .map(|failure| format!("{:?}: {}", failure.step, failure.message))
                .collect::<Vec<_>>();
            self.teardowns.insert(workspace_id.clone(), transaction);
            if let Err(error) = self.persist_handles() {
                failures.push(format!("RetainPersistence: {error}"));
            }
            return Err(WorkspaceManagerError::TeardownFailed {
                workspace_session_id: workspace_id,
                failures,
            });
        }

        let holder_registration = transaction.handle.holder_registration.clone();
        let outcome = transaction.outcome(self);
        self.record_completed_teardown(workspace_id, holder_registration, outcome.clone());
        Ok(outcome)
    }
}

struct ManagerTeardownExecutor<'a> {
    manager: &'a mut WorkspaceManager,
    transaction: &'a mut TeardownTransaction,
    resource_failed: bool,
}

impl TeardownStepExecutor for ManagerTeardownExecutor<'_> {
    fn execute(&mut self, step: TeardownStep) -> Result<(), String> {
        let started_at = Instant::now();
        let result = match step {
            TeardownStep::Holder => self.terminate_holder(),
            TeardownStep::Commands => Ok(()),
            TeardownStep::NamespaceFds => close_handle_fds(&mut self.transaction.handle),
            TeardownStep::Network => self.teardown_network(),
            TeardownStep::Mounts => self.release_mounts(),
            TeardownStep::Scratch => self.remove_scratch(),
            TeardownStep::Leases => self.release_leases(),
            TeardownStep::LeaseAccounting => self.account_leases(),
            TeardownStep::Persistence => self.persist_terminal_state(),
        };
        record_phase_ms(
            &mut self.transaction.phases_ms,
            teardown_phase_name(step),
            started_at,
        );
        if result.is_err() && step != TeardownStep::Persistence {
            self.resource_failed = true;
        }
        result
    }
}

impl ManagerTeardownExecutor<'_> {
    fn terminate_holder(&mut self) -> Result<(), String> {
        if self.transaction.handle.holder_pid <= 0 {
            self.transaction.holder_terminal = true;
            self.transaction.holder_error = None;
            return Ok(());
        }
        match self.manager.runtime.kill_holder(
            &self.transaction.handle.holder_registration,
            self.transaction.grace_s,
        ) {
            Ok(report) => {
                self.transaction.holder_report = report;
                self.transaction.holder_error = None;
                self.transaction.holder_terminal = true;
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                self.transaction.holder_error = Some(message.clone());
                Err(message)
            }
        }
    }

    fn teardown_network(&mut self) -> Result<(), String> {
        if self.transaction.handle.network != NetworkProfile::Isolated {
            return Ok(());
        }
        let Some(veth) = self.transaction.handle.veth.as_ref() else {
            return Ok(());
        };
        self.manager
            .network
            .teardown_veth(veth)
            .map_err(|error| error.to_string())?;
        self.transaction.handle.veth = None;
        Ok(())
    }

    fn release_mounts(&mut self) -> Result<(), String> {
        if !self.transaction.holder_terminal {
            return Err("deferred until the namespace holder is reaped".to_owned());
        }
        if !self.transaction.handle.ns_fds.is_empty()
            || self.transaction.handle.readiness_fd >= 0
            || self.transaction.handle.control_fd >= 0
        {
            return Err("deferred until namespace fds are closed".to_owned());
        }
        self.transaction.mounts_released = true;
        Ok(())
    }

    fn remove_scratch(&mut self) -> Result<(), String> {
        if !self.transaction.mounts_released {
            return Err("deferred until namespace mounts are released".to_owned());
        }
        let run_dir = &self.transaction.handle.dirs.run_dir;
        if !run_dir.starts_with(&self.manager.scratch_root) {
            return Err(format!(
                "refusing to remove scratch outside manager root: {}",
                run_dir.display()
            ));
        }
        match std::fs::remove_dir_all(run_dir) {
            Ok(()) => {
                self.transaction.scratch_released = true;
                Ok(())
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                self.transaction.scratch_released = true;
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn release_leases(&mut self) -> Result<(), String> {
        if !self.transaction.scratch_released {
            return Err("deferred until workspace scratch is released".to_owned());
        }
        let Some(layer_stack_root) = self.manager.layer_stack_root.as_deref() else {
            return Err("layer stack root is not bound to workspace manager".to_owned());
        };

        let mut failures = Vec::new();
        match self.transaction.handle.candidate_admission.as_ref() {
            None => self.transaction.candidate_lease_released = true,
            Some(admission) if !self.transaction.candidate_lease_released => {
                let lease = &admission.lease;
                match sandbox_runtime_layerstack::service::release_candidate_generation_lease(
                    layer_stack_root,
                    lease,
                ) {
                    Ok(_) => self.transaction.candidate_lease_released = true,
                    Err(error) => failures.push(format!(
                        "release candidate generation lease {}: {error}",
                        lease.lease_id
                    )),
                }
            }
            Some(_) => {}
        }

        if !self.transaction.lease_released {
            match sandbox_runtime_layerstack::service::release_lease(
                layer_stack_root,
                &self.transaction.handle.snapshot.lease_id.0,
            ) {
                Ok(()) => self.transaction.lease_released = true,
                Err(error) => failures.push(format!(
                    "release lease {}: {error}",
                    self.transaction.handle.snapshot.lease_id.0
                )),
            }
        }

        if self.transaction.handle.parked_lease_id.is_none() {
            self.transaction.parked_lease_released = true;
        } else if !self.transaction.parked_lease_released {
            let parked = self
                .transaction
                .handle
                .parked_lease_id
                .as_deref()
                .expect("parked lease checked above");
            match sandbox_runtime_layerstack::service::release_lease(layer_stack_root, parked) {
                Ok(()) => self.transaction.parked_lease_released = true,
                Err(error) => {
                    failures.push(format!("release parked lease {parked}: {error}"));
                }
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn account_leases(&mut self) -> Result<(), String> {
        if !self.transaction.candidate_lease_released
            || !self.transaction.lease_released
            || !self.transaction.parked_lease_released
        {
            return Err("deferred until every workspace lease is released".to_owned());
        }
        let Some(layer_stack_root) = self.manager.layer_stack_root.as_deref() else {
            return Err("layer stack root is not bound to workspace manager".to_owned());
        };
        let stack = sandbox_runtime_layerstack::LayerStack::open(layer_stack_root.to_path_buf())
            .map_err(|error| format!("count active leases after destroy: {error}"))?;
        self.transaction.active_leases_after = Some(stack.active_lease_count());
        Ok(())
    }

    fn persist_terminal_state(&self) -> Result<(), String> {
        if self.resource_failed {
            return Err("deferred until resource teardown succeeds".to_owned());
        }
        self.manager
            .persist_handles()
            .map_err(|error| error.to_string())
    }
}

fn teardown_phase_name(step: TeardownStep) -> &'static str {
    match step {
        TeardownStep::Holder => "kill_holder",
        TeardownStep::Commands => "release_commands",
        TeardownStep::NamespaceFds => "close_namespace_fds",
        TeardownStep::Network => "teardown_veth",
        TeardownStep::Mounts => "release_mounts",
        TeardownStep::Scratch => "rmtree_scratch",
        TeardownStep::Leases => "release_leases",
        TeardownStep::LeaseAccounting => "count_active_leases",
        TeardownStep::Persistence => "persist_handles",
    }
}

fn close_handle_fds(handle: &mut MountedWorkspace) -> Result<(), String> {
    let mut failures = Vec::new();
    close_optional_fd_once("user namespace", &mut handle.ns_fds.user, &mut failures);
    close_optional_fd_once("mount namespace", &mut handle.ns_fds.mnt, &mut failures);
    close_optional_fd_once("pid namespace", &mut handle.ns_fds.pid, &mut failures);
    close_optional_fd_once("network namespace", &mut handle.ns_fds.net, &mut failures);
    close_required_fd_once("holder readiness", &mut handle.readiness_fd, &mut failures);
    close_required_fd_once("holder control", &mut handle.control_fd, &mut failures);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn close_optional_fd_once(name: &str, slot: &mut Option<i32>, failures: &mut Vec<String>) {
    if let Some(fd) = slot.take() {
        close_owned_fd(name, fd, failures);
    }
}

fn close_required_fd_once(name: &str, slot: &mut i32, failures: &mut Vec<String>) {
    let fd = std::mem::replace(slot, -1);
    close_owned_fd(name, fd, failures);
}

fn close_owned_fd(name: &str, fd: i32, failures: &mut Vec<String>) {
    if fd >= 0 {
        if let Err(error) = nix::unistd::close(fd) {
            failures.push(format!("close {name} fd {fd}: {error}"));
        }
    }
}

fn mountinfo_reference_count(paths: &[&Path]) -> Option<usize> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let needles = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    Some(
        mountinfo
            .lines()
            .filter(|line| needles.iter().any(|needle| line.contains(needle)))
            .count(),
    )
}
