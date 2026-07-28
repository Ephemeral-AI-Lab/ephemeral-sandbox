use std::sync::PoisonError;

use crate::workspace_crate::{DestroyWorkspaceRequest, DestroyWorkspaceResult, WorkspaceSessionId};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::MplaStoragePhase;

impl WorkspaceSessionService {
    /// Guarded explicit destroy. A live holder still rejects a non-empty
    /// command ledger. A dead holder instead claims (or joins) the shared
    /// holder teardown, which cancels and joins those commands before recovery
    /// preservation and raw resource release.
    ///
    /// # Errors
    /// Returns [`WorkspaceSessionError::ActiveCommands`] only for a live
    /// holder, [`WorkspaceSessionError::NotFound`] for an unknown session, or
    /// the shared teardown failure.
    pub fn guarded_destroy(
        &self,
        workspace_session_id: WorkspaceSessionId,
        grace_s: Option<f64>,
    ) -> Result<DestroyWorkspaceResult, WorkspaceSessionError> {
        let gate = self.session_gate(&workspace_session_id);
        if let Some(flight) = self.existing_destroy_flight(&workspace_session_id)? {
            return Self::wait_destroy_flight(&flight);
        }
        let admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(flight) = self.existing_destroy_flight(&workspace_session_id)? {
            drop(admission);
            return Self::wait_destroy_flight(&flight);
        }

        let (handler, holder_is_live, active_commands) = {
            let sessions = self.lock_sessions()?;
            let Some(session) = sessions.get(&workspace_session_id) else {
                drop(sessions);
                self.discard_resurrected_gate(&workspace_session_id, &gate);
                return Err(WorkspaceSessionError::not_found(&workspace_session_id));
            };
            if let Some(binding) = &session.mpla_binding {
                if binding.phase != MplaStoragePhase::Cleaned {
                    return Err(WorkspaceSessionError::MplaLifecycle {
                        workspace_session_id: workspace_session_id.clone(),
                        reason: format!(
                            "destroy requires a successful MPLA cleanup receipt (current phase: {})",
                            binding.phase.as_str()
                        ),
                    });
                }
            }
            (
                session.handler(),
                self.workspace().holder_is_live(&session.handle),
                session.active_commands.iter().cloned().collect::<Vec<_>>(),
            )
        };

        if holder_is_live {
            if !active_commands.is_empty() {
                return Err(WorkspaceSessionError::ActiveCommands {
                    workspace_session_id,
                    active_command_session_ids: active_commands,
                });
            }
            let (flight, leader) = self.claim_destroy_flight(&handler)?;
            if !leader {
                drop(admission);
                return Self::wait_destroy_flight(&flight);
            }
            return self.destroy_claimed_session(
                handler,
                DestroyWorkspaceRequest { grace_s },
                flight,
            );
        }

        let (flight, leader) = self.claim_holder_destroy_flight(&handler)?;
        drop(admission);
        if leader {
            self.run_holder_destroy(flight, DestroyWorkspaceRequest { grace_s })
        } else {
            Self::wait_destroy_flight(&flight)
        }
    }
}
