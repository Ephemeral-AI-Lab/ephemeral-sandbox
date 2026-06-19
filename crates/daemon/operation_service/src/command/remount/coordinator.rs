use std::sync::Arc;

use super::quiesce::merge_report;
use super::{
    CommandRemountInspection, CommandRemountQuiesce, RemountBlockReason, RemountCancellationToken,
    RemountSwitchState,
};
use crate::command::{CancellationState, CommandLifecycleState, CommandOperationService};
use crate::workspace_crate::WorkspaceId;
use crate::workspace_remount::CommandRemountCoordinator;

impl CommandRemountCoordinator for CommandOperationService {
    fn begin_workspace_remount_quiesce(
        &self,
        workspace_session_id: &WorkspaceId,
    ) -> CommandRemountQuiesce {
        let _admission_guard = self.lock_remount_admission();
        let command_ids = self
            .registry()
            .commands_for_workspace_session(workspace_session_id);
        let cancellation = RemountCancellationToken::new();
        let mut quiesce = CommandRemountQuiesce {
            inspection: CommandRemountInspection {
                active_commands: command_ids.len(),
                ..CommandRemountInspection::default()
            },
            held_process_group_ids: Vec::new(),
            command_ids: Vec::new(),
            process_store: Arc::clone(self.process_store()),
            cancellation,
            switch_state: RemountSwitchState::Quiescing,
            controller: self.remount_controller(),
        };
        if command_ids.is_empty() {
            quiesce.switch_state = RemountSwitchState::ReadyToSwitch;
            return quiesce;
        }

        for command_id in command_ids {
            quiesce.inspection.command_ids.push(command_id.clone());
            let Some(active) = self.process_store().active(&command_id) else {
                quiesce
                    .inspection
                    .block_if_clear(RemountBlockReason::ActiveCommandMissing);
                continue;
            };
            let process = Arc::clone(&active.process);
            let workspace_root = active.workspace_root.clone();
            drop(active);

            let Some(pgid) = process.process_group_id() else {
                quiesce
                    .inspection
                    .block_if_clear(RemountBlockReason::ProcessGroupUnavailable);
                continue;
            };
            quiesce.inspection.process_group_ids.push(pgid);
            quiesce.command_ids.push(command_id.clone());
            let cancellation = quiesce.cancellation.clone();
            if self
                .process_store()
                .update_active(&command_id, |active| {
                    active.lifecycle_state = CommandLifecycleState::QuiescedForRemount;
                    active.cancellation = CancellationState::None;
                    active.remount_cancellation = Some(cancellation);
                    active.remount_switch_state = Some(RemountSwitchState::Quiescing);
                })
                .is_none()
            {
                quiesce
                    .inspection
                    .block_if_clear(RemountBlockReason::ActiveCommandMissing);
                continue;
            }
            let command_report = quiesce
                .controller
                .inspect_command_process_group(pgid, &workspace_root);
            let held = command_report.blocked_reason.is_none();
            merge_report(&mut quiesce.inspection, command_report);
            if held {
                quiesce.held_process_group_ids.push(pgid);
            }
        }

        quiesce.inspection.command_ids.sort();
        quiesce.inspection.command_ids.dedup();
        quiesce.inspection.process_group_ids.sort_unstable();
        quiesce.inspection.process_group_ids.dedup();
        quiesce.set_switch_state(RemountSwitchState::ReadyToSwitch);
        if !quiesce.inspection.can_live_remount() {
            quiesce.resume();
        }
        quiesce
    }
}
