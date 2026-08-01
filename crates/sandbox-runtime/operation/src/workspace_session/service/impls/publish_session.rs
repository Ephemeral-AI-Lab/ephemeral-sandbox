use std::sync::PoisonError;

use sandbox_observability_telemetry::record::names;
use sandbox_observability_telemetry::SpanStatus;
use sandbox_runtime_mpla_poc::MonotonicTimer;
use serde_json::json;

use crate::layerstack::LayerStackServiceError;
use crate::workspace_crate::{
    DestroyWorkspaceRequest, ProtectedPathDrop, ProtectedPathDropReason, WorkspaceSessionId,
};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::{
    FinalizationState, FinalizePolicy, PublishWorkspaceSessionResult,
    WorkspaceSessionPublishDetails,
};

impl WorkspaceSessionService {
    /// Publish an explicit session's complete delta and close the session.
    ///
    /// # Errors
    /// Returns a retained-session error before commit, a partial-success error
    /// when close fails after publication, or a session admission error.
    pub fn publish_workspace_session(
        &self,
        workspace_session_id: WorkspaceSessionId,
        grace_s: Option<f64>,
    ) -> Result<PublishWorkspaceSessionResult, WorkspaceSessionError> {
        self.obs().scope(names::WORKSPACE_SESSION_PUBLISH, |span| {
            span.attr("workspace_session_id", workspace_session_id.0.clone());
            let gate = self.session_gate(&workspace_session_id);
            let matched_publication_timer = MonotonicTimer::start().map_err(|error| {
                WorkspaceSessionError::FinalizationFailed {
                    workspace_session_id: workspace_session_id.clone(),
                    error: format!("start matched publication clock: {error}"),
                }
            })?;
            let _admission = gate.lock().unwrap_or_else(PoisonError::into_inner);
            let handler = {
                let mut sessions = self.lock_sessions()?;
                let Some(session) = sessions.get_mut(&workspace_session_id) else {
                    drop(sessions);
                    self.discard_resurrected_gate(&workspace_session_id, &gate);
                    return Err(WorkspaceSessionError::not_found(&workspace_session_id));
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
                    return Err(WorkspaceSessionError::not_found(&workspace_session_id));
                }
                if session.mpla_binding.is_some() {
                    return Err(WorkspaceSessionError::MplaLifecycle {
                        workspace_session_id,
                        reason: "MPLA sessions are finalized through storage-admin cleanup and destroy, not generic publish".to_owned(),
                    });
                }
                if session.finalize_policy != FinalizePolicy::NoOp {
                    return Err(WorkspaceSessionError::not_found(&workspace_session_id));
                }
                if !session.active_commands.is_empty() {
                    return Err(WorkspaceSessionError::ActiveCommands {
                        workspace_session_id,
                        active_command_session_ids: session
                            .active_commands
                            .iter()
                            .cloned()
                            .collect(),
                    });
                }
                session.finalization_state = FinalizationState::Finalizing;
                session.handler()
            };

            let captured = match self.capture_session_changes(&handler) {
                Ok(captured) => captured,
                Err(error) => {
                    self.restore_active_after_publish_failure(&handler.workspace_session_id);
                    span.status(SpanStatus::Error)
                        .attr("stage", "capture")
                        .attr("session_retained", true);
                    return Err(WorkspaceSessionError::PublishRetained {
                        workspace_session_id: handler.workspace_session_id,
                        stage: super::super::model::PublishFailureStage::Capture,
                        diagnostic: error.to_string(),
                        publish_rejection: None,
                    });
                }
            };

            let published = match explicit_publish_input(captured)
                .and_then(|captured| self.publish_session_changes(&handler, captured))
            {
                Ok(published) => published,
                Err(error) => {
                    let (diagnostic, publish_rejection) = publish_error_parts(error);
                    self.restore_active_after_publish_failure(&handler.workspace_session_id);
                    span.status(SpanStatus::Error)
                        .attr("stage", "publish")
                        .attr("session_retained", true);
                    return Err(WorkspaceSessionError::PublishRetained {
                        workspace_session_id: handler.workspace_session_id,
                        stage: super::super::model::PublishFailureStage::Publish,
                        diagnostic,
                        publish_rejection,
                    });
                }
            };

            let publish = WorkspaceSessionPublishDetails {
                no_op: published.no_op,
                revision: published.revision,
                route_summary: published.route_summary,
            };
            span.attr("no_op", publish.no_op)
                .attr("revision", publish.revision.manifest_version)
                .attr("source_count", publish.route_summary.source_count)
                .attr("ignored_count", publish.route_summary.ignored_count)
                .attr("committed", !publish.no_op);

            let destroyed = self
                .destroy_session_under_gate(handler.clone(), DestroyWorkspaceRequest { grace_s });

            match destroyed {
                Ok(result) => {
                    let matched_publication_span = matched_publication_timer.finish();
                    if !publish.no_op {
                        self.layerstack().notify_autosquash_layer_committed();
                    }
                    let matched_publication_span =
                        matched_publication_span.map_err(|error| {
                            WorkspaceSessionError::FinalizationFailed {
                                workspace_session_id: handler.workspace_session_id.clone(),
                                error: format!("finish matched publication clock: {error}"),
                            }
                        })?;
                    span.attr("destroyed", true)
                        .attr("cleanup_outcome", "destroyed");
                    Ok(PublishWorkspaceSessionResult {
                        workspace_session_id: handler.workspace_session_id,
                        publish,
                        evicted_upperdir_bytes: result.evicted_upperdir_bytes,
                        matched_publication_span,
                    })
                }
                Err(error) => {
                    if !publish.no_op {
                        self.layerstack().notify_autosquash_layer_committed();
                    }
                    self.mark_publish_finalize_failed(&handler.workspace_session_id);
                    span.status(SpanStatus::Error)
                        .attr("stage", "destroy")
                        .attr("publish_completed", true)
                        .attr("destroyed", false)
                        .attr("cleanup_outcome", "finalize_failed");
                    self.obs().event(
                        names::WORKSPACE_SESSION_FINALIZE_FAILED,
                        json!({
                            "workspace_session_id": handler.workspace_session_id.0,
                            "stage": "destroy",
                            "publish_completed": true,
                            "layer_committed": !publish.no_op,
                            "cleanup_error": true,
                        }),
                    );
                    Err(WorkspaceSessionError::PublishedButNotClosed {
                        workspace_session_id: handler.workspace_session_id,
                        publish,
                        diagnostic: error.to_string(),
                    })
                }
            }
        })
    }

    fn restore_active_after_publish_failure(&self, workspace_session_id: &WorkspaceSessionId) {
        if let Ok(mut sessions) = self.lock_sessions() {
            if let Some(session) = sessions.get_mut(workspace_session_id) {
                if session.finalization_state == FinalizationState::Finalizing {
                    session.finalization_state = FinalizationState::Active;
                }
            }
        }
    }

    fn mark_publish_finalize_failed(&self, workspace_session_id: &WorkspaceSessionId) {
        if let Ok(mut sessions) = self.lock_sessions() {
            if let Some(session) = sessions.get_mut(workspace_session_id) {
                if matches!(
                    session.finalization_state,
                    FinalizationState::Finalizing | FinalizationState::FinalizeFailed
                ) {
                    session.finalization_state = FinalizationState::FinalizeFailed;
                    session.holder_cleanup_terminal = true;
                }
            }
        }
    }
}

fn publish_error_parts(
    error: LayerStackServiceError,
) -> (
    String,
    Option<Box<sandbox_runtime_layerstack::PublishReject>>,
) {
    match error {
        LayerStackServiceError::PublishRejected { rejection } => {
            (format!("publish rejected: {rejection:?}"), Some(rejection))
        }
        LayerStackServiceError::InvalidBaseRevision { expected, base } => (
            format!("invalid base revision: expected {expected:?}, base {base:?}"),
            Some(Box::new(sandbox_runtime_layerstack::PublishReject {
                path: None,
                reason: sandbox_runtime_layerstack::PublishRejectReason::InvalidBaseRevision,
                source_conflict: None,
                protected_drop: None,
                message: None,
            })),
        ),
        error => (error.to_string(), None),
    }
}

fn explicit_publish_input(
    captured: crate::workspace_crate::CapturedWorkspaceChanges,
) -> Result<crate::workspace_crate::CapturedWorkspaceChanges, LayerStackServiceError> {
    let Some(drop) = captured.protected_drops.first() else {
        return Ok(captured);
    };
    Err(LayerStackServiceError::PublishRejected {
        rejection: Box::new(sandbox_runtime_layerstack::PublishReject {
            path: None,
            reason: sandbox_runtime_layerstack::PublishRejectReason::ProtectedPath,
            source_conflict: None,
            protected_drop: Some(layer_protected_drop(drop)),
            message: None,
        }),
    })
}

fn layer_protected_drop(
    drop: &ProtectedPathDrop,
) -> sandbox_runtime_layerstack::LayerProtectedDrop {
    sandbox_runtime_layerstack::LayerProtectedDrop {
        path: drop.path.clone(),
        reason: match drop.reason {
            ProtectedPathDropReason::UnsupportedSpecialFile => {
                sandbox_runtime_layerstack::LayerProtectedDropReason::UnsupportedSpecialFile
            }
            ProtectedPathDropReason::InvalidLayerPath => {
                sandbox_runtime_layerstack::LayerProtectedDropReason::InvalidLayerPath
            }
            ProtectedPathDropReason::CommandScratchPath => {
                sandbox_runtime_layerstack::LayerProtectedDropReason::CommandScratchPath
            }
        },
    }
}
