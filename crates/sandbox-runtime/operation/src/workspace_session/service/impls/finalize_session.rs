use std::sync::{Arc, OnceLock};

use sandbox_observability_telemetry::record::names;
use sandbox_observability_telemetry::SpanStatus;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::layerstack::{
    LayerStackRevision, LayerStackServiceError, PublishChangesRequest, PublishChangesResult,
};
use crate::workspace_crate::{
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, DestroyWorkspaceRequest,
    HolderFinalization, HolderFinalizationProof, ProtectedPathDrop, ProtectedPathDropReason,
};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::{FinalizationState, FinalizeOutcome, WorkspaceSessionHandler};

const HOLDER_FINALIZATION_MAX_ATTEMPTS: usize = 3;

impl WorkspaceSessionService {
    /// The `publish_then_destroy` policy runner: capture the session's upperdir
    /// changes, publish them to the layerstack (skipped when the capture is
    /// empty), then destroy the session. Runs under the admission gate held by
    /// the completing path and never holds the `sessions` map across capture,
    /// publish, or destroy I/O. Infallible: a rejected publish is surfaced via
    /// span status, the `finalize.publish_failed` event, and the completing
    /// command's outcome slot, and the destroy still proceeds; a failed destroy
    /// leaves the session `finalize_failed` for `guarded_destroy` recovery.
    pub(crate) fn finalize_session_snapshot(
        &self,
        handler: WorkspaceSessionHandler,
        proof: &HolderFinalizationProof,
        finalization_attempts: usize,
        finalize_outcome: &Arc<OnceLock<FinalizeOutcome>>,
    ) {
        let result: Result<(), std::convert::Infallible> =
            self.obs().scope(names::WORKSPACE_SESSION_FINALIZE, |span| {
                span.attr(
                    "workspace_session_id",
                    handler.workspace_session_id.0.clone(),
                );
                let mut published = false;
                let mut layer_committed = false;
                match self.capture_session_changes_after_holder_quiesced(&handler, proof) {
                    Ok(captured) if captured.changes.is_empty() => {}
                    Ok(captured) => match self.publish_session_changes(&handler, captured) {
                        Ok(result) => {
                            published = true;
                            layer_committed = !result.no_op;
                        }
                        Err(error) => {
                            let publish_reject_class = publish_reject_class(&error);
                            let _ = finalize_outcome
                                .set(FinalizeOutcome::publish_rejected(publish_reject_class));
                            span.status(SpanStatus::Error)
                                .attr("publish_reject_class", publish_reject_class);
                            self.obs().event(
                                names::WORKSPACE_SESSION_FINALIZE_PUBLISH_FAILED,
                                json!({
                                    "workspace_session_id": handler.workspace_session_id.0,
                                    "reject_class": publish_reject_class,
                                    "detail": error.to_string(),
                                }),
                            );
                        }
                    },
                    Err(_) => {
                        let class = "holder_finalization_capture_failed";
                        let _ = finalize_outcome.set(FinalizeOutcome::finalization_failed(
                            class,
                            finalization_attempts,
                        ));
                        self.mark_destroy_failed(&handler);
                        let recovery_preserved = self
                            .preserve_holder_recovery_artifact(
                                &handler.workspace_session_id,
                                &handler,
                            )
                            .is_ok();
                        span.status(SpanStatus::Error)
                            .attr("capture_failed", true)
                            .attr("recovery_preserved", recovery_preserved);
                        self.obs().event(
                            names::WORKSPACE_SESSION_FINALIZE_FAILED,
                            json!({
                                "workspace_session_id": handler.workspace_session_id.0,
                                "stage": "capture_after_holder_quiesced",
                                "class": class,
                                "recovery_preserved": recovery_preserved,
                            }),
                        );
                    }
                }
                span.attr("published", published);
                if !self.destroy_finalized_session(&handler) {
                    let _ = finalize_outcome.set(FinalizeOutcome::finalization_failed(
                        "holder_finalization_destroy_failed",
                        finalization_attempts,
                    ));
                }
                if layer_committed {
                    self.layerstack().notify_autosquash_layer_committed();
                }
                Ok(())
            });
        match result {
            Ok(()) => {}
            Err(never) => match never {},
        }
    }

    pub(in crate::workspace_session::service::impls) fn capture_session_changes(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Result<CapturedWorkspaceChanges, WorkspaceSessionError> {
        self.obs()
            .scope(names::WORKSPACE_SESSION_CAPTURE_CHANGES, |_span| {
                Ok(self.workspace().capture_changes(
                    &handler.handle,
                    CaptureChangesRequest {
                        include_stats: false,
                    },
                )?)
            })
    }

    pub(in crate::workspace_session::service::impls) fn capture_session_changes_after_holder_quiesced(
        &self,
        handler: &WorkspaceSessionHandler,
        proof: &HolderFinalizationProof,
    ) -> Result<CapturedWorkspaceChanges, WorkspaceSessionError> {
        self.obs()
            .scope(names::WORKSPACE_SESSION_CAPTURE_CHANGES, |_span| {
                Ok(self.workspace().capture_changes_after_holder_quiesced(
                    &handler.handle,
                    proof,
                    CaptureChangesRequest {
                        include_stats: false,
                    },
                )?)
            })
    }

    pub(in crate::workspace_session::service::impls) fn quiesce_session_holder(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> (HolderFinalization, usize) {
        for attempt in 1..=HOLDER_FINALIZATION_MAX_ATTEMPTS {
            let result = self
                .workspace()
                .quiesce_holder_for_finalization(&handler.handle);
            if !matches!(result, HolderFinalization::Unknown { .. })
                || attempt == HOLDER_FINALIZATION_MAX_ATTEMPTS
            {
                return (result, attempt);
            }
        }
        unreachable!("bounded holder finalization loop always returns")
    }

    pub(in crate::workspace_session::service::impls) fn mark_holder_quiesced_for_finalization(
        &self,
        handler: &WorkspaceSessionHandler,
    ) -> Result<(), WorkspaceSessionError> {
        let mut sessions = self.lock_sessions()?;
        let session = sessions
            .get_mut(&handler.workspace_session_id)
            .ok_or_else(|| WorkspaceSessionError::not_found(&handler.workspace_session_id))?;
        if session.handler() != *handler
            || session.finalization_state != FinalizationState::Finalizing
        {
            return Err(WorkspaceSessionError::not_found(
                &handler.workspace_session_id,
            ));
        }
        session.holder_quiesced_for_finalization = true;
        Ok(())
    }

    pub(in crate::workspace_session::service::impls) fn publish_session_changes(
        &self,
        handler: &WorkspaceSessionHandler,
        captured: CapturedWorkspaceChanges,
    ) -> Result<PublishChangesResult, LayerStackServiceError> {
        self.layerstack().publish_changes(PublishChangesRequest {
            publication_id: publication_id(&handler.workspace_session_id.0),
            expected_base: layerstack_revision(&captured.base_revision),
            base_manifest: captured.base_manifest,
            protected_drops: layer_protected_drops(captured.protected_drops),
            changes: captured.changes,
            owner: format!("workspace_session:{}", handler.workspace_session_id.0),
        })
    }

    fn destroy_finalized_session(&self, handler: &WorkspaceSessionHandler) -> bool {
        let destroyed =
            self.destroy_session_under_gate(handler.clone(), DestroyWorkspaceRequest::default());
        if let Err(error) = destroyed {
            let failure = WorkspaceSessionError::FinalizationFailed {
                workspace_session_id: handler.workspace_session_id.clone(),
                error: error.to_string(),
            };
            if let Ok(mut sessions) = self.lock_sessions() {
                if let Some(session) = sessions.get_mut(&handler.workspace_session_id) {
                    session.finalization_state = FinalizationState::FinalizeFailed;
                }
            }
            self.obs().event(
                names::WORKSPACE_SESSION_FINALIZE_FAILED,
                json!({
                    "workspace_session_id": handler.workspace_session_id.0,
                    "error": failure.to_string(),
                }),
            );
            false
        } else {
            true
        }
    }
}

fn publication_id(workspace_session_id: &str) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(b"EOS-LS3-WORKSPACE-PUBLICATION\0");
    hasher.update(workspace_session_id.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    let mut id = [0_u8; 16];
    id.copy_from_slice(&digest[..16]);
    if id == [0; 16] {
        id[15] = 1;
    }
    id
}

fn publish_reject_class(error: &LayerStackServiceError) -> &'static str {
    match error {
        LayerStackServiceError::PublishRejected { rejection } => match rejection.reason {
            sandbox_runtime_layerstack::PublishRejectReason::InvalidBaseRevision => {
                "invalid_base_revision"
            }
            sandbox_runtime_layerstack::PublishRejectReason::ProtectedPath => "protected_path",
            sandbox_runtime_layerstack::PublishRejectReason::SourceConflict => "source_conflict",
            sandbox_runtime_layerstack::PublishRejectReason::OpaqueDirProtectedDescendant => {
                "opaque_dir_protected_descendant"
            }
            sandbox_runtime_layerstack::PublishRejectReason::OpaqueDirMixedRoutes => {
                "opaque_dir_mixed_routes"
            }
            sandbox_runtime_layerstack::PublishRejectReason::OpaqueDirExpansionLimit => {
                "opaque_dir_expansion_limit"
            }
            sandbox_runtime_layerstack::PublishRejectReason::RoutePreparationFailed => {
                "route_preparation_failed"
            }
        },
        _ => "publish_error",
    }
}

fn layerstack_revision(revision: &BaseRevision) -> LayerStackRevision {
    LayerStackRevision {
        manifest_version: revision.version,
        root_hash: revision.root_hash.clone(),
        layer_count: revision.layer_count,
    }
}

fn layer_protected_drops(
    drops: Vec<ProtectedPathDrop>,
) -> Vec<sandbox_runtime_layerstack::LayerProtectedDrop> {
    drops
        .into_iter()
        .map(|drop| sandbox_runtime_layerstack::LayerProtectedDrop {
            path: drop.path,
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
        })
        .collect()
}
