use crate::workspace_crate::{CaptureChangesRequest, CapturedWorkspaceChanges};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionService};

use super::super::model::WorkspaceSessionHandler;
use tracing::{field, Span};

impl WorkspaceSessionService {
    pub fn capture_session_changes(
        &self,
        handler: &WorkspaceSessionHandler,
        request: CaptureChangesRequest,
    ) -> Result<CapturedWorkspaceChanges, WorkspaceSessionError> {
        let span = tracing::info_span!(
            "workspace.capture_changes",
            include_stats = request.include_stats,
            status = field::Empty,
            error_kind = field::Empty,
            changed_path_count = field::Empty,
            protected_drop_count = field::Empty,
            metadata_path_count = field::Empty,
            stats_included = field::Empty,
        );
        let _span_guard = span.enter();
        let result = self.capture_session_changes_inner(handler, request);
        record_capture_changes_result(&span, &result);
        result
    }

    fn capture_session_changes_inner(
        &self,
        handler: &WorkspaceSessionHandler,
        request: CaptureChangesRequest,
    ) -> Result<CapturedWorkspaceChanges, WorkspaceSessionError> {
        let mut sessions = self.lock_sessions()?;
        let session = sessions
            .get_mut(&handler.workspace_session_id)
            .ok_or_else(|| WorkspaceSessionError::not_found(&handler.workspace_session_id))?;
        let handle = session.active_handle()?;
        let result = self.workspace().capture_changes(&handle, request)?;
        session.refresh_after_capture(result.base_revision.clone());

        Ok(result)
    }
}

fn record_capture_changes_result(
    span: &Span,
    result: &Result<CapturedWorkspaceChanges, WorkspaceSessionError>,
) {
    match result {
        Ok(result) => {
            span.record("status", "ok");
            span.record("changed_path_count", result.changed_paths.len());
            span.record("protected_drop_count", result.protected_drops.len());
            span.record("metadata_path_count", result.metadata_path_count);
            span.record("stats_included", result.stats.is_some());
        }
        Err(error) => {
            span.record("status", "error");
            span.record("error_kind", error.kind());
        }
    }
}
