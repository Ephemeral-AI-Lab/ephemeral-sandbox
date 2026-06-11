//! Host-neutral sandbox runtime services.
//!
//! Workspace runtime code owns isolated-workspace lease custody, lifecycle
//! sweeps, and caller-keyed workspace-run cancellation. Routing code owns typed
//! command/file target selection between isolated and direct workspace backends.
//! Plugin operation machinery lives in `eos-plugin-ops`; this crate composes it
//! with workspace state for cross-family policy.

#![forbid(unsafe_code)]

use std::sync::Arc;

use eos_config::configs::daemon::PluginRuntimeConfig;
use eos_config::configs::isolated_workspace::IsolatedWorkspaceConfig;
use eos_plugin::PluginError;
use eos_plugin_ops::{PluginRuntime, PluginRuntimeError};
use serde_json::Value;

pub mod workspace;
pub mod maintenance {
    pub mod sweepers {
        use crate::WorkspaceRuntime;

        #[must_use]
        pub fn sweep_workspace_ttl(workspace: &WorkspaceRuntime) -> usize {
            workspace.ttl_sweep()
        }

        pub fn sweep_command_sessions() {
            eos_command_ops::runtime::command_session_reaper_sweep();
        }

        pub fn recover_orphaned_command_sessions() {
            eos_command_ops::runtime::recover_orphaned_command_sessions();
        }
    }
}
pub mod routing {
    pub mod command_op;
    pub mod file_op;
}

pub use workspace::{CallerCancel, ExitOutcome, WorkspaceEnterError, WorkspaceRuntime};

/// Runtime service instances shared by daemon dispatch handlers.
pub struct RuntimeServices {
    pub plugin: PluginRuntime,
    pub workspace: WorkspaceRuntime,
}

impl RuntimeServices {
    #[must_use]
    pub fn new(
        plugin: PluginRuntimeConfig,
        isolated_workspace: IsolatedWorkspaceConfig,
        launcher: Arc<dyn eos_isolated_workspace::NsRunnerLauncher>,
    ) -> Self {
        Self {
            plugin: PluginRuntime::new(plugin, launcher),
            workspace: WorkspaceRuntime::new(isolated_workspace),
        }
    }

    pub fn ensure_plugin_family_allowed(&self, args: &Value) -> Result<(), PluginRuntimeError> {
        eos_plugin_ops::ensure::validate_plugin_caller_fields(args)?;
        let caller_id = args
            .get("caller_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if !caller_id.is_empty() && self.workspace.caller_has_active_handle(caller_id) {
            return Err(PluginRuntimeError::Plugin(
                PluginError::ForbiddenInIsolatedWorkspace,
            ));
        }
        Ok(())
    }
}
