use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use sandbox_observability_telemetry::record::names;
use sandbox_runtime_mpla_poc::{
    StorageAdminAction, StorageAdminOutcome, StorageAdminReceipt, StorageAdminScope,
};
use sandbox_runtime_namespace_execution::NamespaceExecutionId;
use serde_json::json;

use crate::workspace_crate::{DestroyWorkspaceRequest, HolderFinalization, WorkspaceSessionId};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::core::DestroyFlight;
use super::super::model::{
    FinalizationState, FinalizeOutcome, FinalizePolicy, MplaStoragePhase, MplaWorkspaceBinding,
    WorkspaceSessionHandler,
};

enum PostGateCompletion {
    None,
    /// Only a newly claimed flight whose immutable plan has an empty command
    /// ledger may run from command completion. An existing owner may be
    /// waiting for this very completion callback and must never be joined
    /// here.
    HolderDestroyLeader(Arc<DestroyFlight>),
}

/// Take-once slot shared between `exec_command`'s failure path and the engine
/// `on_complete` closure; exactly one side takes the token and completes it
/// (§2.3).
pub type TokenSlot = Arc<Mutex<Option<SessionExecutionToken>>>;

/// What admission hands back to `exec_command` while the caller still holds
/// the session admission gate (caller-locks realization of §2.3).
pub struct AdmittedCommand {
    pub handler: WorkspaceSessionHandler,
    pub finalize_policy: FinalizePolicy,
    pub token_slot: TokenSlot,
    pub finalize_outcome: Arc<OnceLock<FinalizeOutcome>>,
}

/// RAII completion for one admitted command: dropping the token removes the
/// command's ledger entry and runs the finalize policy when the ledger
/// drained. Completion against a missing session or ledger entry is a silent
/// no-op; the drop is panic-contained and never poisons the watcher.
pub struct SessionExecutionToken {
    service: Arc<WorkspaceSessionService>,
    workspace_session_id: WorkspaceSessionId,
    command_session_id: NamespaceExecutionId,
    finalize_outcome: Arc<OnceLock<FinalizeOutcome>>,
    completed: bool,
}

impl Drop for SessionExecutionToken {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        self.completed = true;
        let completion = catch_unwind(AssertUnwindSafe(|| {
            self.service.complete_activity(
                &self.workspace_session_id,
                &self.command_session_id,
                &self.finalize_outcome,
            );
        }));
        if completion.is_err() {
            self.service.obs().event(
                names::WORKSPACE_SESSION_FINALIZE_FAILED,
                json!({
                    "workspace_session_id": self.workspace_session_id.0,
                    "command_session_id": self.command_session_id.0,
                    "panicked": true,
                }),
            );
        }
    }
}

impl WorkspaceSessionService {
    /// Admit one command into the session's ledger. The caller must already
    /// hold `gate`'s guard and keep holding it through transcript prep, launch,
    /// and attach (§2.3); `admission` is the proof-of-lock. A destroyed or
    /// finalizing session fails `not_found` and cleans the gates-map entry the
    /// lookup may have resurrected.
    ///
    /// # Errors
    /// Returns [`WorkspaceSessionError::NotFound`] when the session is absent
    /// or no longer active, or a lock error when session state is unreadable.
    pub fn admit_command_locked(
        self: &Arc<Self>,
        gate: &Arc<Mutex<()>>,
        admission: &MutexGuard<'_, ()>,
        workspace_session_id: &WorkspaceSessionId,
        command_session_id: NamespaceExecutionId,
    ) -> Result<AdmittedCommand, WorkspaceSessionError> {
        let _ = admission;
        let (handler, finalize_policy) = {
            let mut sessions = self.lock_sessions()?;
            let Some(session) = sessions.get_mut(workspace_session_id) else {
                drop(sessions);
                self.discard_resurrected_gate(workspace_session_id, gate);
                return Err(WorkspaceSessionError::not_found(workspace_session_id));
            };
            if !self.workspace().holder_is_live(&session.handle) {
                return Err(WorkspaceSessionError::HolderExited {
                    workspace_session_id: workspace_session_id.clone(),
                    reason: self
                        .workspace()
                        .holder_exit_reason(&session.handle)
                        .unwrap_or_else(|| "exit-status:unknown".to_owned()),
                    cleanup_state: session.finalization_state,
                });
            }
            if session.finalization_state != FinalizationState::Active {
                return Err(WorkspaceSessionError::not_found(workspace_session_id));
            }
            if let Some(binding) = &session.mpla_binding {
                if binding.phase != MplaStoragePhase::Mounted {
                    return Err(WorkspaceSessionError::MplaLifecycle {
                        workspace_session_id: workspace_session_id.clone(),
                        reason: format!(
                            "ordinary command admission requires mounted MPLA storage (current phase: {})",
                            binding.phase.as_str()
                        ),
                    });
                }
            }
            session.active_commands.insert(command_session_id.clone());
            (session.handler(), session.finalize_policy)
        };
        let finalize_outcome = Arc::new(OnceLock::new());
        let token = SessionExecutionToken {
            service: Arc::clone(self),
            workspace_session_id: workspace_session_id.clone(),
            command_session_id,
            finalize_outcome: Arc::clone(&finalize_outcome),
            completed: false,
        };
        Ok(AdmittedCommand {
            handler,
            finalize_policy,
            token_slot: Arc::new(Mutex::new(Some(token))),
            finalize_outcome,
        })
    }

    /// Resolve the session inside its admission gate and run `f` on the fresh
    /// handler while the gate is held. Synchronous file ops and remounts route
    /// through here: no ledger mutation, no finalization (§2.3 / F1), and no
    /// pre-gate stale handler.
    ///
    /// # Errors
    /// Returns [`WorkspaceSessionError::NotFound`] when the session is absent
    /// or no longer active, or a lock error when session state is unreadable.
    pub fn with_gated_session<R>(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        f: impl FnOnce(&WorkspaceSessionHandler) -> R,
    ) -> Result<R, WorkspaceSessionError> {
        let gate = self.session_gate(workspace_session_id);
        let _admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
        let handler = {
            let sessions = self.lock_sessions()?;
            match sessions.get(workspace_session_id) {
                Some(session) if !self.workspace().holder_is_live(&session.handle) => {
                    return Err(WorkspaceSessionError::HolderExited {
                        workspace_session_id: workspace_session_id.clone(),
                        reason: self
                            .workspace()
                            .holder_exit_reason(&session.handle)
                            .unwrap_or_else(|| "exit-status:unknown".to_owned()),
                        cleanup_state: session.finalization_state,
                    });
                }
                Some(session) if session.finalization_state == FinalizationState::Active => {
                    if let Some(binding) = &session.mpla_binding {
                        if binding.phase != MplaStoragePhase::Mounted {
                            return Err(WorkspaceSessionError::MplaLifecycle {
                                workspace_session_id: workspace_session_id.clone(),
                                reason: format!(
                                    "ordinary workspace access requires mounted MPLA storage (current phase: {})",
                                    binding.phase.as_str()
                                ),
                            });
                        }
                    }
                    session.handler()
                }
                _ => {
                    drop(sessions);
                    self.discard_resurrected_gate(workspace_session_id, &gate);
                    return Err(WorkspaceSessionError::not_found(workspace_session_id));
                }
            }
        };
        Ok(f(&handler))
    }

    /// Run one storage-admin action under the exact same gate as commands and
    /// destruction.  The helper is invoked with a copy of server-owned state;
    /// the state transition happens only after a matching successful receipt.
    pub(crate) fn with_gated_mpla_storage_action<R>(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        action: StorageAdminAction,
        f: impl FnOnce(
            &WorkspaceSessionHandler,
            &MplaWorkspaceBinding,
        ) -> Result<(R, StorageAdminReceipt), String>,
    ) -> Result<R, WorkspaceSessionError> {
        let gate = self.session_gate(workspace_session_id);
        let _admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
        let (handler, binding) = {
            let sessions = self.lock_sessions()?;
            let Some(session) = sessions.get(workspace_session_id) else {
                drop(sessions);
                self.discard_resurrected_gate(workspace_session_id, &gate);
                return Err(WorkspaceSessionError::not_found(workspace_session_id));
            };
            if !self.workspace().holder_is_live(&session.handle) {
                return Err(WorkspaceSessionError::HolderExited {
                    workspace_session_id: workspace_session_id.clone(),
                    reason: self
                        .workspace()
                        .holder_exit_reason(&session.handle)
                        .unwrap_or_else(|| "exit-status:unknown".to_owned()),
                    cleanup_state: session.finalization_state,
                });
            }
            if session.finalization_state != FinalizationState::Active {
                return Err(WorkspaceSessionError::not_found(workspace_session_id));
            }
            let binding = session.mpla_binding.clone().ok_or_else(|| {
                WorkspaceSessionError::MplaLifecycle {
                    workspace_session_id: workspace_session_id.clone(),
                    reason: "storage-admin is available only for dedicated MPLA sessions"
                        .to_owned(),
                }
            })?;
            if !mpla_action_allowed(binding.phase, action) {
                return Err(WorkspaceSessionError::MplaLifecycle {
                    workspace_session_id: workspace_session_id.clone(),
                    reason: format!(
                        "storage action {action:?} is not allowed from MPLA phase {}",
                        binding.phase.as_str()
                    ),
                });
            }
            (session.handler(), binding)
        };
        let (result, receipt) =
            f(&handler, &binding).map_err(|reason| WorkspaceSessionError::MplaLifecycle {
                workspace_session_id: workspace_session_id.clone(),
                reason,
            })?;
        if receipt.action != action || receipt.outcome != StorageAdminOutcome::Succeeded {
            return Err(WorkspaceSessionError::MplaLifecycle {
                workspace_session_id: workspace_session_id.clone(),
                reason: "storage-admin did not return a matching successful receipt".to_owned(),
            });
        }
        let mut sessions = self.lock_sessions()?;
        let session = sessions
            .get_mut(workspace_session_id)
            .ok_or_else(|| WorkspaceSessionError::not_found(workspace_session_id))?;
        let current =
            session
                .mpla_binding
                .as_mut()
                .ok_or_else(|| WorkspaceSessionError::MplaLifecycle {
                    workspace_session_id: workspace_session_id.clone(),
                    reason: "dedicated MPLA binding disappeared during storage action".to_owned(),
                })?;
        if current.phase != binding.phase {
            return Err(WorkspaceSessionError::MplaLifecycle {
                workspace_session_id: workspace_session_id.clone(),
                reason: "MPLA lifecycle phase changed during storage action".to_owned(),
            });
        }
        current.phase = mpla_phase_after(action);
        Ok(result)
    }

    /// Return the exact server-derived scope a caller must echo in each typed
    /// storage-admin request.  This is deliberately available before Mount so
    /// creation can hand off a usable, but non-authoritative, request template.
    pub(crate) fn mpla_storage_scope(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        sandbox_id: &str,
    ) -> Result<StorageAdminScope, WorkspaceSessionError> {
        let gate = self.session_gate(workspace_session_id);
        let _admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
        let (handler, binding) = {
            let sessions = self.lock_sessions()?;
            let session = sessions
                .get(workspace_session_id)
                .ok_or_else(|| WorkspaceSessionError::not_found(workspace_session_id))?;
            if !self.workspace().holder_is_live(&session.handle) {
                return Err(WorkspaceSessionError::HolderExited {
                    workspace_session_id: workspace_session_id.clone(),
                    reason: self
                        .workspace()
                        .holder_exit_reason(&session.handle)
                        .unwrap_or_else(|| "exit-status:unknown".to_owned()),
                    cleanup_state: session.finalization_state,
                });
            }
            let binding = session.mpla_binding.clone().ok_or_else(|| {
                WorkspaceSessionError::MplaLifecycle {
                    workspace_session_id: workspace_session_id.clone(),
                    reason: "storage scope is available only for dedicated MPLA sessions"
                        .to_owned(),
                }
            })?;
            (session.handler(), binding)
        };
        let holder_pid = u32::try_from(handler.handle.holder_pid).map_err(|_| {
            WorkspaceSessionError::MplaLifecycle {
                workspace_session_id: workspace_session_id.clone(),
                reason: "live MPLA holder pid is invalid".to_owned(),
            }
        })?;
        if holder_pid == 0 {
            return Err(WorkspaceSessionError::MplaLifecycle {
                workspace_session_id: workspace_session_id.clone(),
                reason: "live MPLA holder pid is invalid".to_owned(),
            });
        }
        let mount_namespace_id = std::fs::read_link(format!("/proc/{holder_pid}/ns/mnt"))
            .map_err(|error| WorkspaceSessionError::MplaLifecycle {
                workspace_session_id: workspace_session_id.clone(),
                reason: format!("read live holder mount namespace: {error}"),
            })?
            .to_string_lossy()
            .into_owned();
        let entry =
            handler
                .handle
                .entry()
                .map_err(|error| WorkspaceSessionError::MplaLifecycle {
                    workspace_session_id: workspace_session_id.clone(),
                    reason: format!("read live MPLA workspace launch material: {error}"),
                })?;
        Ok(StorageAdminScope {
            run_id: binding.run_id,
            sandbox_id: sandbox_id.to_owned(),
            workspace_session_id: handler.workspace_session_id.0,
            session_id: binding.lease.session_id,
            allocation_id: binding.allocation.descriptor.allocation_id,
            lease_id: binding.lease_operation_id.as_str().to_owned(),
            lease_epoch: binding.lease.lease_epoch,
            mount_namespace_id,
            payload_root: binding.payload_root,
            control_root: binding.control_root,
            lower_dirs_newest_first: entry.layer_paths,
            allocation_root: binding.allocation.allocation_root,
            workspace_root: binding.prepared.workspace_root().to_path_buf(),
        })
    }

    /// Command-completion edge (token drop): locks the gate itself on the
    /// calling thread, removes the ledger entry, and runs the finalize policy
    /// when the ledger drained. A missing session or ledger entry is a silent
    /// no-op (§2.3 / F5).
    pub(crate) fn complete_activity(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_session_id: &NamespaceExecutionId,
        finalize_outcome: &Arc<OnceLock<FinalizeOutcome>>,
    ) {
        let gate = self.session_gate(workspace_session_id);
        let admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
        let post_gate = self.complete_under_gate(
            workspace_session_id,
            command_session_id,
            finalize_outcome,
            Some(&gate),
        );
        drop(admission);
        self.complete_after_gate(post_gate);
    }

    /// Completion for failure paths that already hold the admission guard
    /// (§2.3): consumes the token without re-locking the gate. The token's own
    /// drop is defused first, so unwinding cannot double-complete.
    pub(crate) fn complete_admitted_locked(
        &self,
        mut token: SessionExecutionToken,
        admission: MutexGuard<'_, ()>,
    ) {
        token.completed = true;
        let workspace_session_id = token.workspace_session_id.clone();
        let command_session_id = token.command_session_id.clone();
        let finalize_outcome = Arc::clone(&token.finalize_outcome);
        drop(token);
        let post_gate = self.complete_under_gate(
            &workspace_session_id,
            &command_session_id,
            &finalize_outcome,
            None,
        );
        drop(admission);
        self.complete_after_gate(post_gate);
    }

    fn complete_under_gate(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_session_id: &NamespaceExecutionId,
        finalize_outcome: &Arc<OnceLock<FinalizeOutcome>>,
        resurrected_gate: Option<&Arc<Mutex<()>>>,
    ) -> PostGateCompletion {
        let candidate = {
            let Ok(mut sessions) = self.lock_sessions() else {
                self.obs().event(
                    names::WORKSPACE_SESSION_FINALIZE_FAILED,
                    json!({
                        "workspace_session_id": workspace_session_id.0,
                        "command_session_id": command_session_id.0,
                        "error": "sessions lock poisoned during completion",
                    }),
                );
                return PostGateCompletion::None;
            };
            let Some(session) = sessions.get_mut(workspace_session_id) else {
                drop(sessions);
                if let Some(gate) = resurrected_gate {
                    self.discard_resurrected_gate(workspace_session_id, gate);
                }
                return PostGateCompletion::None;
            };
            if !session.active_commands.remove(command_session_id) {
                return PostGateCompletion::None;
            }
            if !session.active_commands.is_empty()
                || session.finalize_policy != FinalizePolicy::PublishThenDestroy
                || session.finalization_state != FinalizationState::Active
            {
                return PostGateCompletion::None;
            }
            // Claim normal finalization before releasing the sessions lock.
            // A holder exit after this point cannot leave an empty Active
            // session or let another path steal the teardown boundary.
            session.finalization_state = FinalizationState::Finalizing;
            session.handler()
        };

        let (finalization, attempts) = self.quiesce_session_holder(&candidate);
        match finalization {
            HolderFinalization::Quiesced { proof } => {
                if let Err(error) = self.mark_holder_quiesced_for_finalization(&candidate) {
                    self.fail_completion_finalization(
                        &candidate,
                        command_session_id,
                        finalize_outcome,
                        "holder_finalization_state_changed",
                        attempts,
                    );
                    self.obs().event(
                        names::WORKSPACE_SESSION_FINALIZE_FAILED,
                        json!({
                            "workspace_session_id": candidate.workspace_session_id.0,
                            "command_session_id": command_session_id.0,
                            "stage": "holder_finalization_state",
                            "error_present": !error.to_string().is_empty(),
                        }),
                    );
                    return PostGateCompletion::None;
                }
                self.finalize_session_snapshot(candidate, &proof, attempts, finalize_outcome);
                PostGateCompletion::None
            }
            HolderFinalization::Exited => match self.claim_holder_destroy_flight(&candidate) {
                Ok((flight, true)) => {
                    debug_assert!(
                        flight
                            .holder_plan
                            .as_ref()
                            .is_some_and(|plan| plan.command_ids.is_empty()),
                        "last-command completion may lead only an empty-ledger holder flight"
                    );
                    PostGateCompletion::HolderDestroyLeader(flight)
                }
                Ok((_flight, false)) => PostGateCompletion::None,
                Err(_) => {
                    self.fail_completion_finalization(
                        &candidate,
                        command_session_id,
                        finalize_outcome,
                        "holder_finalization_cleanup_claim_failed",
                        attempts,
                    );
                    PostGateCompletion::None
                }
            },
            HolderFinalization::Unknown { class } => {
                self.fail_completion_finalization(
                    &candidate,
                    command_session_id,
                    finalize_outcome,
                    class.as_str(),
                    attempts,
                );
                PostGateCompletion::None
            }
        }
    }

    fn complete_after_gate(&self, completion: PostGateCompletion) {
        if let PostGateCompletion::HolderDestroyLeader(flight) = completion {
            let _ = self.run_holder_destroy(flight, DestroyWorkspaceRequest::default());
        }
    }

    fn fail_completion_finalization(
        &self,
        handler: &WorkspaceSessionHandler,
        command_session_id: &NamespaceExecutionId,
        finalize_outcome: &Arc<OnceLock<FinalizeOutcome>>,
        class: &'static str,
        attempts: usize,
    ) {
        self.mark_destroy_failed(handler);
        let _ = finalize_outcome.set(FinalizeOutcome::finalization_failed(class, attempts));
        self.obs().event(
            names::WORKSPACE_SESSION_FINALIZE_FAILED,
            json!({
                "workspace_session_id": handler.workspace_session_id.0,
                "command_session_id": command_session_id.0,
                "stage": "holder_finalization",
                "class": class,
                "attempts": attempts,
            }),
        );
    }
}

const fn mpla_action_allowed(phase: MplaStoragePhase, action: StorageAdminAction) -> bool {
    matches!(
        (phase, action),
        (MplaStoragePhase::Prepared, StorageAdminAction::Mount)
            | (MplaStoragePhase::Mounted, StorageAdminAction::Quiesce)
            | (
                MplaStoragePhase::Quiesced,
                StorageAdminAction::StrictUnmount
            )
            | (MplaStoragePhase::Unmounted, StorageAdminAction::Cleanup)
    )
}

const fn mpla_phase_after(action: StorageAdminAction) -> MplaStoragePhase {
    match action {
        StorageAdminAction::Mount => MplaStoragePhase::Mounted,
        StorageAdminAction::Quiesce => MplaStoragePhase::Quiesced,
        StorageAdminAction::StrictUnmount => MplaStoragePhase::Unmounted,
        StorageAdminAction::Cleanup => MplaStoragePhase::Cleaned,
    }
}
