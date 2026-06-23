use std::sync::Arc;

use crate::command::CommandOperationService;
use crate::layerstack::LayerStackService;
use crate::namespace_execution::{
    BeginNamespaceExecution, CompleteNamespaceExecution, NamespaceExecutionId,
    NamespaceExecutionRecord, NamespaceExecutionStore,
};
use crate::observability::{AsyncTraceSink, RuntimeObservabilitySnapshot};
use crate::workspace_crate::{profile::WorkspaceModeManager, WorkspaceRuntimeService};
use crate::workspace_session::WorkspaceSessionService;

#[derive(Clone)]
pub struct SandboxRuntimeOperations {
    pub command: Arc<CommandOperationService>,
    pub workspace_session: Arc<WorkspaceSessionService>,
    pub layerstack: Arc<LayerStackService>,
    namespace_execution: Arc<NamespaceExecutionStore>,
}

impl SandboxRuntimeOperations {
    #[must_use]
    pub fn new(
        command: Arc<CommandOperationService>,
        workspace_session: Arc<WorkspaceSessionService>,
        layerstack: Arc<LayerStackService>,
    ) -> Self {
        let namespace_execution = Arc::clone(command.namespace_execution_store());
        Self::new_with_namespace_execution_store(
            command,
            workspace_session,
            layerstack,
            namespace_execution,
        )
    }

    #[doc(hidden)]
    #[must_use]
    pub fn new_with_namespace_execution_store(
        command: Arc<CommandOperationService>,
        workspace_session: Arc<WorkspaceSessionService>,
        layerstack: Arc<LayerStackService>,
        namespace_execution: Arc<NamespaceExecutionStore>,
    ) -> Self {
        assert!(
            command.shares_workspace_session(&workspace_session),
            "SandboxRuntimeOperations command service must use the same workspace_session Arc"
        );
        assert!(
            command.shares_namespace_execution_store(&namespace_execution),
            "SandboxRuntimeOperations command service must use the same namespace_execution Arc"
        );
        Self {
            command,
            workspace_session,
            layerstack,
            namespace_execution,
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
        let namespace_execution = Arc::new(NamespaceExecutionStore::new());
        let layerstack = Arc::new(
            LayerStackService::new(layer_stack_root)
                .expect("layerstack service initialization failed"),
        );
        let command = Arc::new(CommandOperationService::new_with_async_trace_sink(
            Arc::clone(&workspace_session),
            ::sandbox_runtime_command::CommandConfig {
                scratch_root: config.command.scratch_root,
            },
            Arc::clone(&namespace_execution),
            async_trace_sink,
        ));
        Self::new_with_namespace_execution_store(
            command,
            workspace_session,
            layerstack,
            namespace_execution,
        )
    }

    #[must_use]
    pub fn observability_snapshot(&self) -> RuntimeObservabilitySnapshot {
        let (workspaces, mut partial_errors) = self.workspace_session.snapshot_workspaces();
        let active_namespace_executions = match self
            .namespace_execution
            .snapshot_active_namespace_executions()
        {
            Ok(snapshots) => snapshots,
            Err(error) => {
                partial_errors.push(error);
                Vec::new()
            }
        };
        let completed_namespace_executions = match self
            .namespace_execution
            .drain_completed_namespace_executions(256)
        {
            Ok(completed) => completed,
            Err(error) => {
                partial_errors.push(error);
                Vec::new()
            }
        };
        match self.namespace_execution.drain_partial_errors() {
            Ok(errors) => partial_errors.extend(errors),
            Err(error) => partial_errors.push(error),
        }

        RuntimeObservabilitySnapshot {
            workspaces,
            active_namespace_executions,
            completed_namespace_executions,
            partial_errors,
        }
    }

    pub fn ack_completed_namespace_executions(
        &self,
        namespace_execution_ids: &[NamespaceExecutionId],
    ) -> Result<(), String> {
        self.namespace_execution
            .ack_completed_namespace_executions(namespace_execution_ids)
    }

    #[doc(hidden)]
    pub fn begin_namespace_execution_for_test(
        &self,
        namespace_execution_id: NamespaceExecutionId,
        begin: BeginNamespaceExecution,
    ) -> Result<(), String> {
        self.namespace_execution
            .begin_namespace_execution(namespace_execution_id, begin)
    }

    #[doc(hidden)]
    pub fn complete_namespace_execution_for_test(
        &self,
        namespace_execution_id: &NamespaceExecutionId,
        completion: CompleteNamespaceExecution,
    ) -> Result<NamespaceExecutionRecord, String> {
        self.namespace_execution
            .complete_namespace_execution(namespace_execution_id, completion)
    }

    #[doc(hidden)]
    pub fn drain_completed_namespace_executions_for_test(
        &self,
        limit: usize,
    ) -> Result<Vec<NamespaceExecutionRecord>, String> {
        self.namespace_execution
            .drain_completed_namespace_executions(limit)
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
