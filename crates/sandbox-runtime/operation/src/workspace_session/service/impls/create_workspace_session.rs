use std::collections::hash_map::Entry;
use std::sync::{Arc, PoisonError};

use sandbox_observability_telemetry::record::names;
use serde_json::json;

use crate::workspace_crate::{
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, ExecutionScratchRoute,
    WorkspaceHandle,
};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::cgroup::cleanup_workspace_cgroup;
use super::super::core::DestroyFlight;
use super::super::model::{
    CreateSessionRequest, FinalizationState, FinalizePolicy, HolderExitDisposition,
    WorkspaceSession, WorkspaceSessionHandler,
};

impl WorkspaceSessionService {
    pub fn create_workspace_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        self.create_workspace_session_routed(request, ExecutionScratchRoute::WorkspaceScoped)
    }

    #[doc(hidden)]
    pub(crate) fn create_workspace_session_legacy_scratch_adapter(
        &self,
        request: CreateSessionRequest,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        self.create_workspace_session_routed(request, ExecutionScratchRoute::LegacyCompat)
    }

    fn create_workspace_session_routed(
        &self,
        request: CreateSessionRequest,
        execution_scratch_route: ExecutionScratchRoute,
    ) -> Result<WorkspaceSessionHandler, WorkspaceSessionError> {
        self.obs().scope(names::WORKSPACE_SESSION_CREATE, |span| {
            span.attr("finalize_policy", request.finalize_policy.as_str());
            span.attr("scratch_route", execution_scratch_route.as_str());
            let workspace_session_id = self
                .workspace()
                .allocate_workspace_session_id(request.network)?;
            let _reservation = self.reserve_workspace_session_id(workspace_session_id.clone())?;
            let handle = self.workspace().create_workspace(CreateWorkspaceRequest {
                workspace_session_id: workspace_session_id.clone(),
                network: request.network,
            })?;
            if handle.id != workspace_session_id {
                return Err(WorkspaceSessionError::WorkspaceIdentityMismatch {
                    reserved_workspace_session_id: workspace_session_id,
                    returned_workspace_session_id: handle.id,
                });
            }
            let cgroup_path = match self.prepare_workspace_cgroup(&workspace_session_id) {
                Ok(path) => path,
                Err(error) => {
                    let raw_rollback = self
                        .workspace()
                        .destroy_workspace(handle.clone(), DestroyWorkspaceRequest::default());
                    let cgroup_path = self.workspace_cgroup_path(&workspace_session_id);
                    let cgroup_retry = setup_rollback_failed(&error)
                        && cgroup_path.as_ref().is_some_and(|path| path.exists());
                    if raw_rollback.is_err() || cgroup_retry {
                        return Err(self.retain_failed_create_cleanup(
                            handle,
                            cgroup_path.filter(|_| cgroup_retry),
                            request.finalize_policy,
                            execution_scratch_route,
                            raw_rollback,
                            error,
                        ));
                    }
                    self.workspace().commit_workspace_destroy(&handle);
                    return Err(error);
                }
            };
            let session = WorkspaceSession::from_handle(
                handle.clone(),
                cgroup_path.clone(),
                request.finalize_policy,
                execution_scratch_route,
            );
            let handler = session.handler();

            let insert_result = self.lock_sessions().and_then(|mut sessions| {
                match sessions.entry(workspace_session_id.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(session);
                        Ok(())
                    }
                    Entry::Occupied(_) => Err(WorkspaceSessionError::DuplicateWorkspaceSessionId {
                        workspace_session_id: workspace_session_id.clone(),
                    }),
                }
            });

            if let Err(insert_error) = insert_result {
                if let Err(rollback_error) = self
                    .workspace()
                    .destroy_workspace(handle.clone(), DestroyWorkspaceRequest::default())
                {
                    return Err(WorkspaceSessionError::CreateRollbackFailed {
                        workspace_session_id,
                        insert_error: Box::new(insert_error),
                        rollback_error,
                    });
                }
                self.workspace().commit_workspace_destroy(&handle);
                if let Some(cgroup_path) = &cgroup_path {
                    let _ = cleanup_workspace_cgroup(cgroup_path);
                }
                return Err(insert_error);
            }

            self.obs().event(
                names::LEASE_ACQUIRED,
                json!({ "revision": handler.handle.base_revision().version }),
            );
            // The session is externally discoverable after insertion, so an
            // autonomous holder-exit teardown or explicit destroy can win
            // before create reaches its commit point. Join the exact
            // generation's flight (or claim its dead-holder teardown) instead
            // of launching a global reconciliation that could retry a retained
            // failure or miss a teardown that already removed the session.
            self.commit_created_session(&handler)?;
            Ok(handler)
        })
    }

    fn commit_created_session(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Result<(), WorkspaceSessionError> {
        if let Some(flight) = self.existing_destroy_flight_for_handler(handler)? {
            return self.join_created_session_flight(handler, &flight);
        }

        let gate = self.session_gate(&handler.workspace_session_id);
        let admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(flight) = self.existing_destroy_flight_for_handler(handler)? {
            drop(admission);
            return self.join_created_session_flight(handler, &flight);
        }

        let (finalization_state, holder_is_live) = {
            let sessions = self.lock_sessions()?;
            let Some(session) = sessions.get(&handler.workspace_session_id) else {
                drop(sessions);
                drop(admission);
                return self.reject_disappeared_created_session(handler);
            };
            if session.handler() != *handler {
                drop(sessions);
                drop(admission);
                return Err(WorkspaceSessionError::not_found(
                    &handler.workspace_session_id,
                ));
            }
            (
                session.finalization_state,
                self.workspace().holder_is_live(&session.handle),
            )
        };

        if holder_is_live {
            drop(admission);
            return match finalization_state {
                FinalizationState::Active => Ok(()),
                FinalizationState::Finalizing | FinalizationState::FinalizeFailed => {
                    Err(WorkspaceSessionError::FinalizationFailed {
                        workspace_session_id: handler.workspace_session_id.clone(),
                        error: format!(
                            "created session was concurrently finalized before commit ({})",
                            finalization_state.as_str()
                        ),
                    })
                }
            };
        }

        if finalization_state == FinalizationState::FinalizeFailed {
            drop(admission);
            return Err(self.created_holder_exit_error(handler, finalization_state, None));
        }
        if finalization_state == FinalizationState::Finalizing {
            drop(admission);
            return Err(self.created_holder_exit_error(handler, finalization_state, None));
        }

        let (flight, leader) = self.claim_holder_destroy_flight(handler)?;
        drop(admission);
        if leader {
            let _ =
                self.run_holder_destroy(Arc::clone(&flight), DestroyWorkspaceRequest::default());
        }
        self.join_created_session_flight(handler, &flight)
    }

    fn join_created_session_flight(
        &self,
        handler: &WorkspaceSessionHandler,
        flight: &Arc<DestroyFlight>,
    ) -> Result<(), WorkspaceSessionError> {
        if flight.holder_plan.is_none() {
            return match Self::wait_destroy_flight(flight) {
                Ok(_) => Err(WorkspaceSessionError::not_found(
                    &handler.workspace_session_id,
                )),
                Err(error) => Err(error),
            };
        }

        let outcome = Self::wait_holder_destroy_flight(flight);
        let fallback_state = match outcome.disposition {
            HolderExitDisposition::RetryableCleanupFailure { .. } => {
                FinalizationState::FinalizeFailed
            }
            HolderExitDisposition::Destroyed | HolderExitDisposition::RecoveryRequired { .. } => {
                FinalizationState::Active
            }
        };
        let cleanup_state = self
            .created_session_state(handler)
            .unwrap_or(fallback_state);
        Err(WorkspaceSessionError::HolderExited {
            workspace_session_id: handler.workspace_session_id.clone(),
            reason: outcome.reason,
            cleanup_state,
        })
    }

    fn reject_disappeared_created_session(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Result<(), WorkspaceSessionError> {
        if self.workspace().holder_is_live(&handler.handle) {
            return Err(WorkspaceSessionError::not_found(
                &handler.workspace_session_id,
            ));
        }
        Err(self.created_holder_exit_error(handler, FinalizationState::Active, None))
    }

    fn created_session_state(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Option<FinalizationState> {
        self.lock_sessions().ok().and_then(|sessions| {
            sessions
                .get(&handler.workspace_session_id)
                .filter(|session| session.handler() == *handler)
                .map(|session| session.finalization_state)
        })
    }

    fn created_holder_exit_error(
        &self,
        handler: &WorkspaceSessionHandler,
        cleanup_state: FinalizationState,
        reason: Option<String>,
    ) -> WorkspaceSessionError {
        WorkspaceSessionError::HolderExited {
            workspace_session_id: handler.workspace_session_id.clone(),
            reason: reason
                .or_else(|| self.workspace().holder_exit_reason(&handler.handle))
                .unwrap_or_else(|| "exit-status:unknown".to_owned()),
            cleanup_state,
        }
    }
}

impl WorkspaceSessionService {
    fn retain_failed_create_cleanup(
        &self,
        handle: WorkspaceHandle,
        cgroup_path: Option<std::path::PathBuf>,
        finalize_policy: FinalizePolicy,
        execution_scratch_route: ExecutionScratchRoute,
        raw_rollback: Result<DestroyWorkspaceResult, crate::workspace_crate::WorkspaceError>,
        setup_error: WorkspaceSessionError,
    ) -> WorkspaceSessionError {
        let workspace_session_id = handle.id.clone();
        let mut session = WorkspaceSession::from_handle(
            handle,
            cgroup_path,
            finalize_policy,
            execution_scratch_route,
        );
        session.finalization_state = FinalizationState::FinalizeFailed;
        let mut failures = vec![format!("workspace setup: {setup_error}")];
        match raw_rollback {
            Ok(result) => session.workspace_destroy_result = Some(result),
            Err(error) => failures.push(format!("workspace rollback: {error}")),
        }
        if !session.cgroup_cleanup_complete {
            failures.push("workload-cgroup rollback remains retryable".to_owned());
        }

        match self.lock_sessions() {
            Ok(mut sessions) => match sessions.entry(workspace_session_id.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(session);
                }
                Entry::Occupied(_) => {
                    failures.push(
                        "cleanup handle could not be retained because the identity is active"
                            .to_owned(),
                    );
                }
            },
            Err(error) => failures.push(format!("retain cleanup handle: {error}")),
        }
        WorkspaceSessionError::TeardownIncomplete {
            workspace_session_id,
            failures,
        }
    }
}

fn setup_rollback_failed(error: &WorkspaceSessionError) -> bool {
    matches!(
        error,
        WorkspaceSessionError::WorkloadCgroupSetupFailed {
            rollback_diagnostic: Some(_),
            ..
        }
    )
}
