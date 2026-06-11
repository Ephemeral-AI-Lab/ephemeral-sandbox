//! The daemon's owned service instances.
//!
//! `DaemonServer` constructs one [`Services`] from typed config and threads a
//! shared reference through [`crate::runtime::context::DispatchContext`]. This
//! is the explicit replacement for process-global service state: handlers reach
//! the plugin and isolated-workspace runtimes only through the context, and
//! nothing else may be added here.

use eos_config::configs::daemon::PluginRuntimeConfig;
use eos_config::configs::isolated_workspace::IsolatedWorkspaceConfig;
use eos_workspace_runtime::WorkspaceRuntime;

use crate::services::plugin::PluginRuntime;

/// Per-server daemon services used by dispatch handlers.
pub struct Services {
    pub(crate) plugin: PluginRuntime,
    pub(crate) workspace: WorkspaceRuntime,
}

impl Services {
    /// Build the daemon services from their typed config sections.
    #[must_use]
    pub fn new(plugin: PluginRuntimeConfig, isolated_workspace: IsolatedWorkspaceConfig) -> Self {
        Self {
            plugin: PluginRuntime::new(plugin),
            workspace: WorkspaceRuntime::new(isolated_workspace),
        }
    }
}

impl Default for Services {
    fn default() -> Self {
        Self::new(
            PluginRuntimeConfig::default(),
            IsolatedWorkspaceConfig::default(),
        )
    }
}
