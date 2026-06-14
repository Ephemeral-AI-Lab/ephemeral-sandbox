//! Daemon-owned runtime services shared by dispatch handlers.

use std::sync::Arc;

use command::CommandConfig;
use config::configs::daemon::PluginRuntimeConfig;
use config::configs::isolated_workspace::IsolatedWorkspaceConfig;
use layerstack::CommitOptions;
use operation::command::CommandOps;
use operation::CallerId;
use plugin::{PluginRuntime, PluginRuntimeError};
use serde_json::Value;

use crate::WorkspaceRuntime;

pub(crate) mod background_tasks {
    use operation::command::CommandOps;

    use crate::WorkspaceRuntime;

    #[must_use]
    pub(crate) fn evict_idle_workspaces_once(workspace: &WorkspaceRuntime) -> usize {
        let report = workspace.evict_idle_workspaces_report();
        let count = report.evicted.len();
        if count > 0 {
            crate::trace::push_background_record(crate::trace::idle_workspace_evict_record(
                &report,
            ));
        }
        count
    }

    pub(crate) fn advance_active_commands_once(command: &CommandOps) {
        for record in command.advance_active_commands_once(std::time::Instant::now()) {
            crate::trace::push_background_record(record);
        }
    }

    pub(crate) fn recover_orphaned_commands(command: &CommandOps) {
        command.recover_orphaned_commands();
    }
}

/// Runtime service instances shared by daemon dispatch handlers.
pub struct RuntimeServices {
    pub command: Arc<CommandOps>,
    pub commit_options: CommitOptions,
    pub plugin: PluginRuntime,
    pub workspace: WorkspaceRuntime,
}

impl RuntimeServices {
    #[must_use]
    pub fn new(
        plugin: PluginRuntimeConfig,
        isolated_workspace: IsolatedWorkspaceConfig,
        command: CommandConfig,
        launcher: Arc<dyn workspace::NsRunnerLauncher>,
    ) -> Self {
        Self::with_commit_options(
            plugin,
            isolated_workspace,
            command,
            launcher,
            CommitOptions::default(),
        )
    }

    #[must_use]
    pub fn with_commit_options(
        plugin: PluginRuntimeConfig,
        isolated_workspace: IsolatedWorkspaceConfig,
        command: CommandConfig,
        launcher: Arc<dyn workspace::NsRunnerLauncher>,
        commit_options: CommitOptions,
    ) -> Self {
        let command = Arc::new(CommandOps::with_commit_options(command, commit_options));
        Self {
            command: Arc::clone(&command),
            commit_options,
            plugin: PluginRuntime::with_commit_options(plugin, launcher, commit_options),
            workspace: WorkspaceRuntime::new(isolated_workspace, command),
        }
    }

    pub fn ensure_plugin_family_allowed(&self, args: &Value) -> Result<(), PluginRuntimeError> {
        operation::plugin::contract::validate_plugin_caller_fields(args)
            .map_err(|err| PluginRuntimeError::InvalidRequest(err.message()))?;
        self.ensure_plugin_caller_allowed(&CallerId::from_wire(args))
    }

    pub fn ensure_plugin_caller_allowed(
        &self,
        caller: &CallerId,
    ) -> Result<(), PluginRuntimeError> {
        if !caller.as_str().is_empty() && self.workspace.caller_has_active_handle(caller.as_str()) {
            return Err(PluginRuntimeError::ForbiddenInIsolatedWorkspace);
        }
        Ok(())
    }
}
