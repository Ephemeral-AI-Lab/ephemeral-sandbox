use std::sync::{Arc, OnceLock};

use sandbox_observability::record::names;
use sandbox_observability::SpanStatus;
use serde_json::json;

use crate::layerstack::{
    LayerStackRevision, LayerStackServiceError, PublishChangesRequest, PublishChangesResult,
};
use crate::workspace_crate::{
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, DestroyWorkspaceRequest,
    ProtectedPathDrop, ProtectedPathDropReason,
};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::{FinalizationState, FinalizeOutcome, WorkspaceSessionHandler};

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
        finalize_outcome: &Arc<OnceLock<FinalizeOutcome>>,
    ) {
        let result: Result<(), std::convert::Infallible> =
            self.obs().scope(names::WORKSPACE_SESSION_FINALIZE, |span| {
                span.attr(
                    "workspace_session_id",
                    handler.workspace_session_id.0.clone(),
                );
                let mut published = false;
                match self.capture_finalize_changes(&handler) {
                    Ok(captured) if captured.changes.is_empty() => {}
                    Ok(captured) => match self.publish_finalize_changes(&handler, captured) {
                        Ok(_) => published = true,
                        Err(error) => {
                            let publish_reject_class = publish_reject_class(&error);
                            let _ = finalize_outcome.set(FinalizeOutcome {
                                publish_reject_class,
                            });
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
                    Err(error) => {
                        span.attr("capture_error", error.to_string());
                    }
                }
                span.attr("published", published);
                self.destroy_finalized_session(&handler);
                Ok(())
            });
        match result {
            Ok(()) => {}
            Err(never) => match never {},
        }
    }

    fn capture_finalize_changes(
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

    fn publish_finalize_changes(
        &self,
        handler: &WorkspaceSessionHandler,
        captured: CapturedWorkspaceChanges,
    ) -> Result<PublishChangesResult, LayerStackServiceError> {
        self.layerstack().publish_changes(PublishChangesRequest {
            expected_base: layerstack_revision(&captured.base_revision),
            base_manifest: captured.base_manifest,
            protected_drops: layer_protected_drops(captured.protected_drops),
            changes: captured.changes,
            owner: format!("workspace_session:{}", handler.workspace_session_id.0),
        })
    }

    fn destroy_finalized_session(&self, handler: &WorkspaceSessionHandler) {
        let destroyed = self.obs().scope(names::WORKSPACE_SESSION_DESTROY, |_span| {
            let snapshot = self.snapshot_for_destroy(&handler.workspace_session_id)?;
            self.destroy_snapshot(snapshot, DestroyWorkspaceRequest::default())
        });
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
        }
    }
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
