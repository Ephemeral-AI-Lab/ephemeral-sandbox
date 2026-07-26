use std::sync::PoisonError;

use sandbox_observability_telemetry::record::names;

use crate::workspace_crate::{RemountOutcome, WorkspaceSessionId};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

/// One session's disposition in the post-commit remount sweep, plus the
/// pre-attempt manifest layer ids that attribute it to squashed blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweptSession {
    pub workspace_session_id: WorkspaceSessionId,
    pub pre_manifest_layer_ids: Vec<String>,
    pub disposition: SweptDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweptDisposition {
    SessionGone,
    Identity,
    Migrated,
    Leased { reason: String },
    Faulty { class_detail: String },
}

impl WorkspaceSessionService {
    /// Attempt the live remount of one session through `with_gated_session`:
    /// resolve inside the gate, run the remount under it. Session-not-found at
    /// the gate is a silent skip; remounts take no ledger entry and never
    /// trigger the finalize policy (§2.3 / F1). After a verified switch
    /// (migrated or parked) the registry copy of the handle is refreshed from
    /// the workspace runtime.
    pub fn remount_session(&self, workspace_session_id: &WorkspaceSessionId) -> SweptSession {
        let result: Result<SweptSession, std::convert::Infallible> =
            self.obs().scope(names::WORKSPACE_SESSION_REMOUNT, |span| {
                span.attr("workspace_session_id", workspace_session_id.0.clone());
                let attempted = self.with_gated_session(workspace_session_id, |handler| {
                    let pre_manifest_layer_ids = handler
                        .handle
                        .snapshot
                        .manifest
                        .layers
                        .iter()
                        .map(|layer| layer.layer_id.clone())
                        .collect();
                    let cgroup_procs_path = handler
                        .cgroup_path
                        .as_ref()
                        .map(|cgroup| cgroup.join("cgroup.procs"));
                    let outcome = self
                        .workspace()
                        .remount_workspace(workspace_session_id, cgroup_procs_path);
                    let disposition = match outcome {
                        Ok(None) => SweptDisposition::SessionGone,
                        Ok(Some(RemountOutcome::Identity)) => SweptDisposition::Identity,
                        Ok(Some(RemountOutcome::Migrated { .. })) => {
                            self.refresh_session_handle(workspace_session_id);
                            SweptDisposition::Migrated
                        }
                        Ok(Some(RemountOutcome::Leased { reason })) => {
                            if reason == "pinned:rollback_unmount_busy" {
                                self.refresh_session_handle(workspace_session_id);
                            }
                            SweptDisposition::Leased { reason }
                        }
                        Ok(Some(RemountOutcome::Faulty { class_detail })) => {
                            SweptDisposition::Faulty { class_detail }
                        }
                        Err(error) => SweptDisposition::Leased {
                            reason: format!("mount_uncertain:remount_transaction:{error}"),
                        },
                    };
                    swept(workspace_session_id, pre_manifest_layer_ids, disposition)
                });
                let swept_session = match attempted {
                    Ok(swept_session) => swept_session,
                    Err(WorkspaceSessionError::NotFound { .. }) => swept(
                        workspace_session_id,
                        Vec::new(),
                        SweptDisposition::SessionGone,
                    ),
                    Err(_) => swept(
                        workspace_session_id,
                        Vec::new(),
                        SweptDisposition::Leased {
                            reason: "mount_uncertain:session_registry_unavailable".to_owned(),
                        },
                    ),
                };
                span.attr("disposition", format!("{:?}", swept_session.disposition));
                Ok(swept_session)
            });
        match result {
            Ok(swept_session) => swept_session,
            Err(never) => match never {},
        }
    }

    /// Destroy a faulty session through the ordinary destroy path (still
    /// under its gate) and report cleanup errors for the result line. The
    /// command-owner release proof fails closed while an execution remains
    /// live, preserving the session for a later sweep or recovery retry.
    pub fn destroy_faulty_session(&self, workspace_session_id: &WorkspaceSessionId) -> Vec<String> {
        let gate = self.session_gate(workspace_session_id);
        let _admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
        let handler = match self.resolve_session(workspace_session_id.clone()) {
            Ok(handler) => handler,
            Err(WorkspaceSessionError::NotFound { .. }) => {
                self.discard_resurrected_gate(workspace_session_id, &gate);
                return Vec::new();
            }
            Err(error) => return vec![format!("resolve for destroy: {error}")],
        };
        match self.destroy_session_under_gate(
            handler,
            crate::workspace_crate::DestroyWorkspaceRequest::default(),
        ) {
            Ok(result) => result
                .lease_release_error
                .map(|error| vec![error])
                .unwrap_or_default(),
            Err(error) => vec![format!("destroy: {error}")],
        }
    }

    /// Persist the workspace handle set once, after the post-commit remount
    /// sweep — the batched replacement for the old per-session persistence.
    /// Best-effort at the call site: a failure changes no live mount, only the
    /// on-disk boot-reap record (which keys on the remount-invariant run dir).
    ///
    /// # Errors
    /// Returns the workspace error when the handle-file write fails.
    pub fn persist_handles(&self) -> Result<(), WorkspaceSessionError> {
        Ok(self.workspace().persist_handles()?)
    }

    fn refresh_session_handle(&self, workspace_session_id: &WorkspaceSessionId) {
        let refreshed = self
            .workspace()
            .current_handle(workspace_session_id)
            .ok()
            .flatten();
        let Some(handle) = refreshed else {
            return;
        };
        if let Ok(mut sessions) = self.lock_sessions() {
            if let Some(session) = sessions.get_mut(workspace_session_id) {
                session.handle = handle;
            }
        }
    }
}

fn swept(
    workspace_session_id: &WorkspaceSessionId,
    pre_manifest_layer_ids: Vec<String>,
    disposition: SweptDisposition,
) -> SweptSession {
    SweptSession {
        workspace_session_id: workspace_session_id.clone(),
        pre_manifest_layer_ids,
        disposition,
    }
}
