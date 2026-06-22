use std::sync::Arc;

use crate::cgroup_monitor::CgroupMonitorOperationService;
use crate::command::CommandOperationService;
use crate::layerstack::LayerStackService;
use crate::workspace_crate::{profile::WorkspaceModeManager, WorkspaceRuntimeService};
use crate::workspace_session::WorkspaceSessionService;

#[derive(Clone)]
pub struct SandboxRuntimeOperations {
    pub command: Arc<CommandOperationService>,
    pub cgroup_monitor: Arc<CgroupMonitorOperationService>,
    pub layerstack: Arc<LayerStackService>,
}

impl SandboxRuntimeOperations {
    #[must_use]
    pub fn new(command: Arc<CommandOperationService>, layerstack: Arc<LayerStackService>) -> Self {
        let cgroup_monitor = Arc::new(CgroupMonitorOperationService::new(Arc::clone(
            command.workspace(),
        )));
        Self {
            command,
            cgroup_monitor,
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
        let cgroup_monitor: ::sandbox_runtime_workspace::CgroupMonitorConfig =
            config.cgroup_monitor.into();
        let workspace_session = Arc::new(WorkspaceSessionService::with_cgroup_monitor(
            workspace_runtime,
            cgroup_monitor.clone(),
        ));
        let layerstack = Arc::new(
            LayerStackService::new(layer_stack_root)
                .expect("layerstack service initialization failed"),
        );
        let command = Arc::new(CommandOperationService::new(
            workspace_session,
            Arc::clone(&layerstack),
            ::sandbox_runtime_command::CommandConfig {
                scratch_root: config.command.scratch_root,
                cgroup_monitor,
            },
        ));
        Self::new(command, layerstack)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxRuntimeConfig {
    pub workspace: WorkspaceRuntimeConfig,
    pub command: CommandRuntimeConfig,
    pub cgroup_monitor: CgroupMonitorRuntimeConfig,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupMonitorRuntimeConfig {
    pub enabled: bool,
    pub sample_interval_ms: u64,
    pub retained_samples_per_target: usize,
    pub include_pids: bool,
    pub include_pressure: bool,
    pub include_disk: bool,
}

impl Default for CgroupMonitorRuntimeConfig {
    fn default() -> Self {
        let default = ::sandbox_runtime_workspace::CgroupMonitorConfig::default();
        Self {
            enabled: default.enabled,
            sample_interval_ms: default.sample_interval_ms,
            retained_samples_per_target: default.retained_samples_per_target,
            include_pids: default.include_pids,
            include_pressure: default.include_pressure,
            include_disk: default.include_disk,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceResourceCaps {
    pub ttl_s: f64,
    pub total_cap: u32,
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
            ttl_s: caps.ttl_s,
            total_cap: caps.total_cap,
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

impl From<CgroupMonitorRuntimeConfig> for ::sandbox_runtime_workspace::CgroupMonitorConfig {
    fn from(config: CgroupMonitorRuntimeConfig) -> Self {
        Self {
            enabled: config.enabled,
            sample_interval_ms: config.sample_interval_ms,
            retained_samples_per_target: config.retained_samples_per_target,
            include_pids: config.include_pids,
            include_pressure: config.include_pressure,
            include_disk: config.include_disk,
        }
    }
}
