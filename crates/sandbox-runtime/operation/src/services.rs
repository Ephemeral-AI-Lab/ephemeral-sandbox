use std::sync::Arc;

use crate::command::CommandOperationService;
use crate::layerstack::LayerStackService;
use crate::observability::{AsyncTraceSink, RuntimeObservabilitySnapshot};
use crate::workspace_crate::{profile::WorkspaceModeManager, WorkspaceRuntimeService};
use crate::workspace_session::WorkspaceSessionService;

#[derive(Clone)]
pub struct SandboxRuntimeOperations {
    pub command: Arc<CommandOperationService>,
    pub workspace_session: Arc<WorkspaceSessionService>,
    pub layerstack: Arc<LayerStackService>,
}

impl SandboxRuntimeOperations {
    #[must_use]
    pub fn new(
        command: Arc<CommandOperationService>,
        workspace_session: Arc<WorkspaceSessionService>,
        layerstack: Arc<LayerStackService>,
    ) -> Self {
        assert!(
            command.shares_workspace_session(&workspace_session),
            "SandboxRuntimeOperations command service must use the same workspace_session Arc"
        );
        Self {
            command,
            workspace_session,
            layerstack,
        }
    }

    #[must_use]
    pub fn from_config(config: SandboxRuntimeConfig) -> Self {
        Self::from_config_with_async_trace_sink(config, None)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn from_config_with_async_trace_sink(
        config: SandboxRuntimeConfig,
        async_trace_sink: Option<AsyncTraceSink>,
    ) -> Self {
        let layer_stack_root = config.workspace.layer_stack_root.clone();
        let workspace_runtime = Arc::new(WorkspaceRuntimeService::new(
            WorkspaceModeManager::new(
                config
                    .workspace
                    .workspace_root
                    .to_string_lossy()
                    .into_owned(),
                config.workspace.caps.into(),
                config.workspace.scratch_root,
            ),
            layer_stack_root.clone(),
        ));
        let workspace_session = Arc::new(WorkspaceSessionService::new(workspace_runtime));
        let layerstack = Arc::new(
            LayerStackService::new(layer_stack_root)
                .expect("layerstack service initialization failed"),
        );
        let command = Arc::new(CommandOperationService::new_with_async_trace_sink(
            Arc::clone(&workspace_session),
            ::sandbox_runtime_command::CommandConfig {
                scratch_root: config.command.scratch_root,
            },
            async_trace_sink,
        ));
        Self::new(command, workspace_session, layerstack)
    }

    #[must_use]
    pub fn observability_snapshot(&self) -> RuntimeObservabilitySnapshot {
        let (workspaces, mut partial_errors) = self.workspace_session.snapshot_workspaces();
        let active_executions = match self.command.process_store().snapshot_active_executions() {
            Ok(snapshots) => snapshots,
            Err(error) => {
                partial_errors.push(error);
                Vec::new()
            }
        };

        RuntimeObservabilitySnapshot {
            workspaces,
            active_executions,
            partial_errors,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxRuntimeConfig {
    pub workspace: WorkspaceRuntimeConfig,
    pub command: CommandRuntimeConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceRuntimeConfig {
    pub workspace_root: std::path::PathBuf,
    pub layer_stack_root: std::path::PathBuf,
    pub scratch_root: std::path::PathBuf,
    pub caps: WorkspaceResourceCaps,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandRuntimeConfig {
    pub scratch_root: std::path::PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceResourceCaps {
    pub upperdir_bytes: u64,
    pub memavail_fraction: f64,
    pub setup_timeout_s: f64,
    pub exit_grace_s: f64,
    pub rfc1918_egress: Rfc1918Egress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rfc1918Egress {
    Allow,
    Deny,
}

impl From<WorkspaceResourceCaps> for crate::workspace_crate::profile::ResourceCaps {
    fn from(caps: WorkspaceResourceCaps) -> Self {
        Self {
            upperdir_bytes: caps.upperdir_bytes,
            memavail_fraction: caps.memavail_fraction,
            setup_timeout_s: caps.setup_timeout_s,
            exit_grace_s: caps.exit_grace_s,
            rfc1918_egress: match caps.rfc1918_egress {
                Rfc1918Egress::Allow => crate::workspace_crate::profile::Rfc1918Egress::Allow,
                Rfc1918Egress::Deny => crate::workspace_crate::profile::Rfc1918Egress::Deny,
            },
        }
    }
}
