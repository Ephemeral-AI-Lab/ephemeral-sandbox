use std::sync::Arc;

use sandbox_observability::Observer;
use sandbox_runtime_layerstack::service::StackObservation;
use serde_json::json;

use crate::command::CommandOperationService;
use crate::layerstack::LayerStackService;
use crate::observability::RuntimeObservabilitySnapshot;
use crate::workspace_crate::{session::WorkspaceManager, WorkspaceRuntimeService};
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

    /// Assemble the runtime services over one shared process `Observer` (a clone
    /// of the daemon's). Every emitting service holds that same handle, so daemon
    /// and runtime spans share one id sequence and one parent chain.
    #[must_use]
    pub fn from_config(config: SandboxRuntimeConfig, observer: Observer) -> Self {
        let layer_stack_root = config.workspace.layer_stack_root.clone();
        let workspace_runtime = Arc::new(WorkspaceRuntimeService::new(
            WorkspaceManager::new(
                config
                    .workspace
                    .workspace_root
                    .to_string_lossy()
                    .into_owned(),
                config.workspace.caps.into(),
                config.workspace.scratch_root,
                observer.clone(),
            ),
            layer_stack_root.clone(),
        ));
        let workspace_session = Arc::new(WorkspaceSessionService::with_cgroup_root(
            workspace_runtime,
            config.cgroup_root.clone(),
            observer.clone(),
        ));
        emit_daemon_progress(
            "layerstack.ensure_workspace_base",
            "started",
            format!(
                "ensuring workspace base for {}",
                config.workspace.workspace_root.display()
            ),
        );
        let base_result = sandbox_runtime_layerstack::ensure_workspace_base(
            &layer_stack_root,
            &config.workspace.workspace_root,
        );
        match base_result {
            Ok((_binding, built)) => emit_daemon_progress(
                "layerstack.ensure_workspace_base",
                "completed",
                if built {
                    "workspace base built"
                } else {
                    "workspace base already exists"
                },
            ),
            Err(error) => {
                emit_daemon_progress(
                    "layerstack.ensure_workspace_base",
                    "failed",
                    error.to_string(),
                );
                panic!("layerstack workspace base initialization failed: {error}");
            }
        }
        let layerstack = Arc::new(
            LayerStackService::new(layer_stack_root, observer.clone())
                .expect("layerstack service initialization failed"),
        );
        let command = Arc::new(CommandOperationService::new(
            Arc::clone(&workspace_session),
            Arc::clone(&layerstack),
            crate::command::CommandConfig {
                scratch_root: config.namespace_execution.scratch_root,
            },
            observer,
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

    /// Live per-layer lease breakdown of the active manifest (in-memory state).
    ///
    /// The daemon merges this with the observability leaf reader's disk byte
    /// sizes (keyed by layer id) to render the `layerstack` inventory.
    pub fn observe_layerstack(
        &self,
    ) -> Result<StackObservation, crate::layerstack::LayerStackServiceError> {
        self.layerstack.observe()
    }

    /// Storage root of the layer stack, for the observability leaf byte reader.
    #[must_use]
    pub fn layer_stack_root(&self) -> &std::path::Path {
        self.layerstack.layer_stack_root()
    }
}

fn emit_daemon_progress(phase: &str, state: &str, message: impl Into<String>) {
    let mut event = json!({
        "event": "progress",
        "op": "daemon.startup",
        "phase": phase,
        "state": state,
        "message": message.into(),
    });
    if let Some(sandbox_id) = daemon_sandbox_id() {
        event["sandbox_id"] = json!(sandbox_id);
    }
    eprintln!("{event}");
}

fn daemon_sandbox_id() -> Option<String> {
    std::env::var("SANDBOX_DAEMON_SANDBOX_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxRuntimeConfig {
    pub workspace: WorkspaceRuntimeConfig,
    pub namespace_execution: NamespaceExecutionRuntimeConfig,
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
pub struct NamespaceExecutionRuntimeConfig {
    pub scratch_root: std::path::PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceResourceCaps {
    pub setup_timeout_s: f64,
    pub exit_grace_s: f64,
    pub rfc1918_egress: Rfc1918Egress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rfc1918Egress {
    Allow,
    Deny,
}

impl From<WorkspaceResourceCaps> for crate::workspace_crate::session::ResourceCaps {
    fn from(caps: WorkspaceResourceCaps) -> Self {
        Self {
            setup_timeout_s: caps.setup_timeout_s,
            exit_grace_s: caps.exit_grace_s,
            rfc1918_egress: match caps.rfc1918_egress {
                Rfc1918Egress::Allow => crate::workspace_crate::session::Rfc1918Egress::Allow,
                Rfc1918Egress::Deny => crate::workspace_crate::session::Rfc1918Egress::Deny,
            },
        }
    }
}
