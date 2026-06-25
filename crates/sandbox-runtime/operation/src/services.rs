use std::sync::Arc;

use crate::command::CommandOperationService;
use crate::layerstack::LayerStackService;
use crate::observability::RuntimeObservabilitySnapshot;
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
        Self {
            command,
            workspace_session,
            layerstack,
        }
    }

    #[must_use]
    pub fn from_config(config: SandboxRuntimeConfig) -> Self {
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
        let workspace_session = Arc::new(WorkspaceSessionService::with_cgroup_root(
            workspace_runtime,
            config.cgroup_root.clone(),
        ));
        sandbox_runtime_layerstack::ensure_workspace_base(
            &layer_stack_root,
            &config.workspace.workspace_root,
        )
        .expect("layerstack workspace base initialization failed");
        let layerstack = Arc::new(
            LayerStackService::new(layer_stack_root)
                .expect("layerstack service initialization failed"),
        );
        let command = Arc::new(CommandOperationService::new(
            Arc::clone(&workspace_session),
            crate::command::CommandConfig {
                scratch_root: config.command.scratch_root,
            },
        ));
        Self::new(command, workspace_session, layerstack)
    }

    #[must_use]
    pub fn observability_snapshot(&self) -> RuntimeObservabilitySnapshot {
        let (workspaces, partial_errors) = self.workspace_session.snapshot_workspaces();
        let active_namespace_executions = self.command.active_namespace_executions();
        RuntimeObservabilitySnapshot {
            workspaces,
            active_namespace_executions,
            partial_errors,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxRuntimeConfig {
    pub workspace: WorkspaceRuntimeConfig,
    pub command: CommandRuntimeConfig,
    pub cgroup_root: Option<std::path::PathBuf>,
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
