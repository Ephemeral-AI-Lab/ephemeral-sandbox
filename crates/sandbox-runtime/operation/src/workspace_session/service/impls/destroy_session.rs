use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use sandbox_observability_telemetry::record::names;
use serde_json::json;

use crate::workspace_crate::{
    DestroyWorkspaceRequest, DestroyWorkspaceResult, WorkspaceError, WorkspaceSessionId,
};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::cgroup::cleanup_workspace_cgroup;
use super::super::core::{DestroyFlight, DestroyFlightTerminal, HolderDestroyPlan};
use super::super::model::{
    FinalizationState, HolderExitDisposition, MplaStoragePhase, MplaWorkspaceBinding,
    WorkspaceSessionHandler,
};

/// The state a destroy needs, snapshotted under a brief `sessions` lock so the
/// lock is never held across the workspace teardown I/O (§2.3 hard rule).
pub(crate) struct DestroySnapshot {
    pub(crate) handler: WorkspaceSessionHandler,
    pub(crate) workspace_destroy_result: Option<DestroyWorkspaceResult>,
    pub(crate) cgroup_cleanup_complete: bool,
    pub(crate) mpla_binding: Option<MplaWorkspaceBinding>,
}

impl WorkspaceSessionService {
    /// Destroy the exact session generation represented by `handler`.
    ///
    /// Public callers enter the admission gate before claiming ownership. A
    /// dead holder is always routed through the shared holder-finalization
    /// transaction so commands are cancelled/joined and the creation-time
    /// recovery policy is preserved. Live holders retain the raw destroy
    /// behavior.
    pub fn destroy_session(
        &self,
        handler: WorkspaceSessionHandler,
        request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        if let Some(flight) = self.existing_destroy_flight_for_handler(&handler)? {
            return Self::wait_destroy_flight(&flight);
        }

        let gate = self.session_gate(&handler.workspace_session_id);
        let admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(flight) = self.existing_destroy_flight_for_handler(&handler)? {
            drop(admission);
            return Self::wait_destroy_flight(&flight);
        }

        let holder_is_live = self.validate_destroy_handler(&handler)?;
        if !holder_is_live {
            let (flight, leader) = self.claim_holder_destroy_flight(&handler)?;
            drop(admission);
            return if leader {
                self.run_holder_destroy(flight, request)
            } else {
                Self::wait_destroy_flight(&flight)
            };
        }

        let (flight, leader) = self.claim_destroy_flight(&handler)?;
        if !leader {
            drop(admission);
            return Self::wait_destroy_flight(&flight);
        }
        self.destroy_claimed_session(handler, request, flight)
    }

    /// Raw destroy for paths that already own the exact session's admission
    /// gate (normal publish/finalize and faulty-remount cleanup). Never wait on
    /// a follower while holding that gate: such a wait could block a holder
    /// owner that must reacquire it after joining commands.
    pub(in crate::workspace_session::service::impls) fn destroy_session_under_gate(
        &self,
        handler: WorkspaceSessionHandler,
        request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        let (flight, leader) = self.claim_destroy_flight(&handler)?;
        if !leader {
            return Err(WorkspaceSessionError::FinalizationFailed {
                workspace_session_id: handler.workspace_session_id,
                error: "destroy transaction already owned while admission gate is held".to_owned(),
            });
        }
        self.destroy_claimed_session(handler, request, flight)
    }

    fn validate_destroy_handler(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Result<bool, WorkspaceSessionError> {
        let sessions = self.lock_sessions()?;
        let Some(session) = sessions.get(&handler.workspace_session_id) else {
            return Err(WorkspaceSessionError::not_found(
                &handler.workspace_session_id,
            ));
        };
        if session.handler() != *handler {
            return Err(WorkspaceSessionError::not_found(
                &handler.workspace_session_id,
            ));
        }
        Ok(self.workspace().holder_is_live(&session.handle))
    }

    /// Reserve or join the one raw teardown transaction for this exact
    /// generation. The sessions map and flight map are acquired in the same
    /// order used by create (`sessions -> destroy_flights` after create's own
    /// reservation lock), so validation and insertion are one atomic boundary.
    pub(crate) fn claim_destroy_flight(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Result<(Arc<DestroyFlight>, bool), WorkspaceSessionError> {
        let sessions = self.lock_sessions()?;
        let mut flights = self
            .destroy_flights
            .lock()
            .map_err(|_| WorkspaceSessionError::LockPoisoned)?;
        if let Some(flight) = flights.get(&handler.workspace_session_id) {
            if flight.handler == *handler {
                return Ok((Arc::clone(flight), false));
            }
            return Err(WorkspaceSessionError::not_found(
                &handler.workspace_session_id,
            ));
        }
        let current = sessions
            .get(&handler.workspace_session_id)
            .ok_or_else(|| WorkspaceSessionError::not_found(&handler.workspace_session_id))?;
        if current.handler() != *handler {
            return Err(WorkspaceSessionError::not_found(
                &handler.workspace_session_id,
            ));
        }
        let flight = Arc::new(DestroyFlight::new(handler.clone(), None));
        flights.insert(handler.workspace_session_id.clone(), Arc::clone(&flight));
        Ok((flight, true))
    }

    /// Claim the immutable dead-holder teardown plan. Only the successful
    /// claimant mutates lifecycle state; followers observe the plan and wait
    /// for the same terminal result.
    pub(crate) fn claim_holder_destroy_flight(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Result<(Arc<DestroyFlight>, bool), WorkspaceSessionError> {
        let mut sessions = self.lock_sessions()?;
        let mut flights = self
            .destroy_flights
            .lock()
            .map_err(|_| WorkspaceSessionError::LockPoisoned)?;
        if let Some(flight) = flights.get(&handler.workspace_session_id) {
            if flight.handler == *handler {
                return Ok((Arc::clone(flight), false));
            }
            return Err(WorkspaceSessionError::not_found(
                &handler.workspace_session_id,
            ));
        }
        let session = sessions
            .get_mut(&handler.workspace_session_id)
            .ok_or_else(|| WorkspaceSessionError::not_found(&handler.workspace_session_id))?;
        if session.handler() != *handler || self.workspace().holder_is_live(&session.handle) {
            return Err(WorkspaceSessionError::not_found(
                &handler.workspace_session_id,
            ));
        }

        let attempt = session.holder_cleanup_attempts.saturating_add(1);
        let plan = HolderDestroyPlan {
            handler: handler.clone(),
            policy: session.finalize_policy,
            command_ids: session.active_commands.iter().cloned().collect(),
            reason: self
                .workspace()
                .holder_exit_reason(&session.handle)
                .unwrap_or_else(|| "exit-status:unknown".to_owned()),
            newly_observed: !session.holder_exit_recorded,
            attempt,
        };
        session.holder_exit_recorded = true;
        session.finalization_state = FinalizationState::Finalizing;
        session.holder_cleanup_attempts = attempt;

        let flight = Arc::new(DestroyFlight::new(handler.clone(), Some(plan)));
        flights.insert(handler.workspace_session_id.clone(), Arc::clone(&flight));
        Ok((flight, true))
    }

    pub(crate) fn existing_destroy_flight(
        &self,
        workspace_session_id: &WorkspaceSessionId,
    ) -> Result<Option<Arc<DestroyFlight>>, WorkspaceSessionError> {
        Ok(self
            .destroy_flights
            .lock()
            .map_err(|_| WorkspaceSessionError::LockPoisoned)?
            .get(workspace_session_id)
            .map(Arc::clone))
    }

    pub(crate) fn existing_destroy_flight_for_handler(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Result<Option<Arc<DestroyFlight>>, WorkspaceSessionError> {
        Ok(self
            .existing_destroy_flight(&handler.workspace_session_id)?
            .filter(|flight| flight.handler == *handler))
    }

    pub(crate) fn wait_destroy_terminal(
        flight: &Arc<DestroyFlight>,
    ) -> Result<DestroyFlightTerminal, WorkspaceSessionError> {
        flight.waiters.fetch_add(1, Ordering::AcqRel);
        let result = (|| {
            let terminal = flight
                .terminal
                .lock()
                .map_err(|_| WorkspaceSessionError::LockPoisoned)?;
            let terminal = flight
                .ready
                .wait_while(terminal, |terminal| terminal.is_none())
                .map_err(|_| WorkspaceSessionError::LockPoisoned)?;
            Ok(terminal
                .as_ref()
                .expect("destroy leader always publishes a terminal result")
                .clone())
        })();
        flight.waiters.fetch_sub(1, Ordering::AcqRel);
        result
    }

    pub(crate) fn wait_destroy_flight(
        flight: &Arc<DestroyFlight>,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        Self::wait_destroy_terminal(flight)?.result
    }

    pub(crate) fn destroy_claimed_session(
        &self,
        handler: WorkspaceSessionHandler,
        request: DestroyWorkspaceRequest,
        flight: Arc<DestroyFlight>,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        let result = self.execute_destroy_transaction(&handler, request);
        if result.is_err() {
            self.mark_destroy_failed(&handler);
        }
        self.publish_destroy_flight(&handler, &flight, result, None)
    }

    /// Execute teardown without publishing the flight. Holder finalization
    /// uses this so recovery/command failures and raw failures all converge on
    /// exactly one terminal publication. A panic is converted to a retryable
    /// error, ensuring joiners are never stranded on the condition variable.
    pub(crate) fn execute_destroy_transaction(
        &self,
        handler: &WorkspaceSessionHandler,
        request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        catch_unwind(AssertUnwindSafe(|| {
            self.obs().scope(names::WORKSPACE_SESSION_DESTROY, |_span| {
                let snapshot = self.snapshot_for_destroy(handler)?;
                self.destroy_snapshot(snapshot, request)
            })
        }))
        .unwrap_or_else(|_| {
            Err(WorkspaceSessionError::FinalizationFailed {
                workspace_session_id: handler.workspace_session_id.clone(),
                error: "destroy transaction panicked".to_owned(),
            })
        })
    }

    pub(crate) fn mark_destroy_failed(&self, handler: &WorkspaceSessionHandler) {
        if let Ok(mut sessions) = self.lock_sessions() {
            if let Some(session) = sessions.get_mut(&handler.workspace_session_id) {
                if session.handler() == *handler {
                    session.finalization_state = FinalizationState::FinalizeFailed;
                }
            }
        }
    }

    pub(crate) fn publish_destroy_flight(
        &self,
        handler: &WorkspaceSessionHandler,
        flight: &Arc<DestroyFlight>,
        result: Result<DestroyWorkspaceResult, WorkspaceSessionError>,
        holder_disposition: Option<HolderExitDisposition>,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        let terminal = DestroyFlightTerminal {
            result: result.clone(),
            holder_disposition,
        };
        {
            let mut slot = flight
                .terminal
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if slot.is_none() {
                *slot = Some(terminal);
            }
            flight.ready.notify_all();
        }
        let mut flights = self
            .destroy_flights
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if flights
            .get(&handler.workspace_session_id)
            .is_some_and(|current| Arc::ptr_eq(current, flight) && current.handler == *handler)
        {
            flights.remove(&handler.workspace_session_id);
        }
        result
    }

    pub(crate) fn snapshot_for_destroy(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Result<DestroySnapshot, WorkspaceSessionError> {
        let mut sessions = self.lock_sessions()?;
        let session = sessions
            .get_mut(&handler.workspace_session_id)
            .ok_or_else(|| WorkspaceSessionError::not_found(&handler.workspace_session_id))?;
        if session.handler() != *handler {
            return Err(WorkspaceSessionError::not_found(
                &handler.workspace_session_id,
            ));
        }
        if let Some(binding) = &session.mpla_binding {
            if binding.phase != MplaStoragePhase::Cleaned {
                return Err(WorkspaceSessionError::MplaLifecycle {
                    workspace_session_id: handler.workspace_session_id.clone(),
                    reason: format!(
                        "raw teardown is forbidden before MPLA cleanup (current phase: {})",
                        binding.phase.as_str()
                    ),
                });
            }
        }
        session.finalization_state = FinalizationState::Finalizing;
        Ok(DestroySnapshot {
            handler: handler.clone(),
            workspace_destroy_result: session.workspace_destroy_result.clone(),
            cgroup_cleanup_complete: session.cgroup_cleanup_complete,
            mpla_binding: session.mpla_binding.clone(),
        })
    }

    pub(crate) fn destroy_snapshot(
        &self,
        snapshot: DestroySnapshot,
        request: DestroyWorkspaceRequest,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        let DestroySnapshot {
            handler,
            workspace_destroy_result,
            cgroup_cleanup_complete,
            mpla_binding,
        } = snapshot;
        let workspace_session_id = handler.workspace_session_id.clone();
        let revision = handler.handle.base_revision().version;
        let command_release = self
            .release_terminal_commands(&workspace_session_id)
            .map_err(|error| WorkspaceSessionError::TeardownIncomplete {
                workspace_session_id: workspace_session_id.clone(),
                failures: vec![format!("command-scratch: {error}")],
            })?;
        debug_assert!(command_release.verifies(&workspace_session_id));
        let _released_terminal_records = command_release.released_terminal_records();
        let mut workspace_error: Option<WorkspaceError> = None;
        let workspace_result = match workspace_destroy_result {
            Some(result) => Some(result),
            None => match self
                .workspace()
                .destroy_workspace(handler.handle.clone(), request)
            {
                Ok(result) => Some(result),
                Err(error) => {
                    workspace_error = Some(error);
                    None
                }
            },
        };
        let mut cgroup_error = None;
        let cgroup_complete = if cgroup_cleanup_complete {
            true
        } else if let Some(cgroup_path) = &handler.cgroup_path {
            match cleanup_workspace_cgroup(cgroup_path) {
                Ok(()) => true,
                Err(error) => {
                    cgroup_error = Some(error);
                    false
                }
            }
        } else {
            true
        };

        {
            let mut sessions = self.lock_sessions()?;
            let session = sessions
                .get_mut(&workspace_session_id)
                .ok_or_else(|| WorkspaceSessionError::not_found(&workspace_session_id))?;
            if session.handler() != handler {
                return Err(WorkspaceSessionError::not_found(&workspace_session_id));
            }
            if let Some(result) = &workspace_result {
                session.workspace_destroy_result = Some(result.clone());
            }
            session.cgroup_cleanup_complete = cgroup_complete;
        }

        if let Some(error) = workspace_error {
            if cgroup_error.is_none() {
                return Err(WorkspaceSessionError::Workspace(error));
            }
            return Err(WorkspaceSessionError::TeardownIncomplete {
                workspace_session_id,
                failures: vec![
                    format!("workspace: {error}"),
                    format!(
                        "workload-cgroup: {}",
                        cgroup_error.expect("checked present")
                    ),
                ],
            });
        }
        if let Some(error) = cgroup_error {
            return Err(WorkspaceSessionError::TeardownIncomplete {
                workspace_session_id,
                failures: vec![format!("workload-cgroup: {error}")],
            });
        }

        if let Some(binding) = &mpla_binding {
            sandbox_runtime_mpla_poc::allocation::destroy_workspace_allocation(
                &binding.payload_root.join("allocations"),
                &binding.allocation.descriptor.allocation_id,
                &binding.lease.deleter,
            )
            .map_err(|error| WorkspaceSessionError::MplaLifecycle {
                workspace_session_id: workspace_session_id.clone(),
                reason: format!("delete cleaned MPLA allocation: {error}"),
            })?;
        }

        let result = workspace_result.expect("no failures requires workspace teardown success");
        {
            let mut sessions = self.lock_sessions()?;
            let matches = sessions
                .get(&workspace_session_id)
                .is_some_and(|session| session.handler() == handler);
            if !matches {
                return Err(WorkspaceSessionError::not_found(&workspace_session_id));
            }
            sessions.remove(&workspace_session_id);
        }
        self.workspace().commit_workspace_destroy(&handler.handle);
        self.drop_session_gate(&workspace_session_id);
        self.obs()
            .event(names::LEASE_RELEASED, json!({ "revision": revision }));
        Ok(result)
    }
}
