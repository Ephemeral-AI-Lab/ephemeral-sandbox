use crate::workspace_crate::{RemountWorkspaceRequest, WorkspaceSessionId};
use crate::workspace_remount::{
    RemountBlockReason, RemountSwitchState, WorkspaceRemountError, WorkspaceRemountOutcome,
    WorkspaceRemountService,
};
use tracing::{field, Span};

impl WorkspaceRemountService {
    pub fn remount_workspace_session(
        &self,
        workspace_session_id: WorkspaceSessionId,
    ) -> Result<WorkspaceRemountOutcome, WorkspaceRemountError> {
        let span = tracing::info_span!(
            "workspace.remount",
            status = field::Empty,
            error_kind = field::Empty,
            remounted = field::Empty,
            blocked_reason = field::Empty,
            active_commands = field::Empty,
            process_count = field::Empty,
            quiesced_process_count = field::Empty,
            inspected = field::Empty,
            quiesce_attempted = field::Empty,
            resumed = field::Empty,
        );
        let _span_guard = span.enter();
        let result = self.remount_workspace_session_inner(workspace_session_id);
        record_remount_result(&span, &result);
        result
    }

    fn remount_workspace_session_inner(
        &self,
        workspace_session_id: WorkspaceSessionId,
    ) -> Result<WorkspaceRemountOutcome, WorkspaceRemountError> {
        let handler = self.workspace.begin_remount(workspace_session_id.clone())?;
        let mut quiesce = self
            .command
            .begin_workspace_remount_quiesce(&workspace_session_id);

        let blocked_reason = quiesce.inspection().blocked_reason.clone().or_else(|| {
            quiesce
                .cancellation_requested()
                .then(|| RemountBlockReason::RemountCancelledBeforeSwitch.to_string())
        });
        if let Some(reason) = blocked_reason {
            self.workspace.block_remount(workspace_session_id.clone())?;
            let inspection = quiesce.finish();
            return Ok(WorkspaceRemountOutcome {
                workspace_session_id,
                remounted: false,
                blocked_reason: Some(reason),
                command_inspection: inspection,
                updated_handler: None,
            });
        }

        quiesce.set_switch_state(RemountSwitchState::CriticalSwitch);
        let request = RemountWorkspaceRequest {
            layer_paths: handler.handle.snapshot.layer_paths.clone(),
        };
        let remount_result = self.workspace.apply_and_finish_remount(&handler, request);
        quiesce.set_switch_state(RemountSwitchState::Resuming);

        match remount_result {
            Ok(updated_handler) => {
                let inspection = quiesce.finish();
                Ok(WorkspaceRemountOutcome {
                    workspace_session_id,
                    remounted: true,
                    blocked_reason: None,
                    command_inspection: inspection,
                    updated_handler: Some(updated_handler),
                })
            }
            Err(error) => {
                let _ = quiesce.finish();
                Err(WorkspaceRemountError::WorkspaceSession(error))
            }
        }
    }
}

fn record_remount_result(
    span: &Span,
    result: &Result<WorkspaceRemountOutcome, WorkspaceRemountError>,
) {
    match result {
        Ok(outcome) => {
            span.record("status", "ok");
            span.record("remounted", outcome.remounted);
            if let Some(reason) = outcome.blocked_reason.as_deref() {
                span.record("blocked_reason", bounded_remount_block_reason(reason));
            }
            let inspection = &outcome.command_inspection;
            span.record("active_commands", inspection.active_commands as u64);
            span.record("process_count", inspection.process_count as u64);
            span.record(
                "quiesced_process_count",
                inspection.quiesced_process_count as u64,
            );
            span.record("inspected", inspection.inspected);
            span.record("quiesce_attempted", inspection.quiesce_attempted);
            span.record("resumed", inspection.resumed);
        }
        Err(error) => {
            span.record("status", "error");
            span.record("error_kind", error.kind());
        }
    }
}

fn bounded_remount_block_reason(reason: &str) -> &'static str {
    match reason {
        "active_command_missing" => "active_command_missing",
        "process_group_unavailable" => "process_group_unavailable",
        "remount_cancelled_before_switch" => "remount_cancelled_before_switch",
        _ => "process_group_blocked",
    }
}
