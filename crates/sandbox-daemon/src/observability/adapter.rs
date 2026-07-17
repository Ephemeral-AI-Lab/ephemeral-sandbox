use sandbox_config::configs::observability::ViewsConfig;
use sandbox_observability_query::ports::{
    NamespaceExecutionSnapshot, ObservabilityInput, ObservabilitySnapshot, QueryContext,
    QueryLimits, WorkspaceSnapshot,
};
use sandbox_observability_telemetry::collect::process_topology::{
    WorkspaceProcessInput, WorkspaceProcessTopology,
};
use sandbox_observability_telemetry::{sample_layerstack, LayerStackBytes, WalkBudget};
use sandbox_runtime::{
    LayerDeltaDescription, RuntimeNamespaceExecutionSnapshot, RuntimeObservabilitySnapshot,
    RuntimeWorkspaceSnapshot, SandboxRuntimeOperations, StackObservation,
};

use super::DaemonObservability;

pub(crate) struct DaemonObservabilityAdapter<'a> {
    state: (
        &'a SandboxRuntimeOperations,
        Option<&'a DaemonObservability>,
    ),
}

impl<'a> DaemonObservabilityAdapter<'a> {
    pub(crate) const fn new(
        operations: &'a SandboxRuntimeOperations,
        observability: Option<&'a DaemonObservability>,
    ) -> Self {
        Self {
            state: (operations, observability),
        }
    }

    fn operations(&self) -> &SandboxRuntimeOperations {
        self.state.0
    }

    fn observability(&self) -> Option<&DaemonObservability> {
        self.state.1
    }
}

impl ObservabilityInput for DaemonObservabilityAdapter<'_> {
    fn query_context(&self) -> Option<QueryContext> {
        let observability = self.observability()?;
        Some(QueryContext {
            reader: observability.reader(),
            sandbox_id: observability.sandbox_id().to_owned(),
            daemon_pid: std::process::id(),
            runtime_dir: observability.runtime_dir().to_string_lossy().into_owned(),
        })
    }

    fn query_limits(&self) -> QueryLimits {
        let views = self
            .observability()
            .map_or_else(ViewsConfig::default, |observability| observability.views);
        QueryLimits {
            resource_window_ms: views.resource_window_ms,
            layer_delta_default_limit: views.layer_delta_default_limit,
            layer_delta_max_limit: views.layer_delta_max_limit,
        }
    }

    fn cgroup_topology(&self) -> WorkspaceProcessTopology {
        let snapshot = self.operations().observability_snapshot();
        if snapshot.workspaces.is_empty() && !snapshot.partial_errors.is_empty() {
            return WorkspaceProcessTopology::unavailable(format!(
                "runtime workspace snapshot failed: {}",
                snapshot.partial_errors.join("; ")
            ));
        }
        let workspaces = snapshot
            .workspaces
            .into_iter()
            .map(|workspace| WorkspaceProcessInput {
                workspace_id: workspace.workspace_id.0,
                holder_pid: u32::try_from(workspace.holder_pid).unwrap_or(0),
            })
            .collect();
        WorkspaceProcessTopology::collect(std::path::Path::new("/proc"), workspaces)
    }

    fn observability_snapshot(&self) -> ObservabilitySnapshot {
        map_snapshot(self.operations().observability_snapshot())
    }

    fn observe_layerstack(&self) -> Result<StackObservation, String> {
        self.operations()
            .observe_layerstack()
            .map_err(|error| error.to_string())
    }

    fn layerstack_bytes(&self) -> LayerStackBytes {
        let sampling = self
            .observability()
            .map_or_else(WalkBudget::default, |observability| observability.sampling);
        sample_layerstack(self.operations().layer_stack_root(), sampling)
    }

    fn describe_layer_delta(
        &self,
        layer_path: &str,
        limit: usize,
    ) -> Result<LayerDeltaDescription, String> {
        let layer_dir = self.operations().layer_stack_root().join(layer_path);
        sandbox_runtime::describe_layer_delta(&layer_dir, limit).map_err(|error| error.to_string())
    }
}

pub(crate) fn map_snapshot(snapshot: RuntimeObservabilitySnapshot) -> ObservabilitySnapshot {
    ObservabilitySnapshot {
        workspaces: snapshot.workspaces.into_iter().map(map_workspace).collect(),
        active_namespace_executions: snapshot
            .active_namespace_executions
            .into_iter()
            .map(map_namespace_execution)
            .collect(),
        partial_errors: snapshot.partial_errors,
    }
}

fn map_workspace(workspace: RuntimeWorkspaceSnapshot) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        workspace_id: workspace.workspace_id.0,
        network_profile: workspace.network.as_str().to_owned(),
        finalize_policy: workspace.finalize_policy.as_str().to_owned(),
        namespace_fd_count: workspace.namespace_fd_count,
        base_root_hash: workspace.base_root_hash,
        layer_count: workspace.layer_count,
        layer_ids: workspace.layer_ids,
    }
}

fn map_namespace_execution(
    execution: RuntimeNamespaceExecutionSnapshot,
) -> NamespaceExecutionSnapshot {
    NamespaceExecutionSnapshot {
        namespace_execution_id: execution.namespace_execution_id.0,
        workspace_session_id: execution.workspace_session_id.0,
        operation_name: execution.operation_name,
        command: execution.command,
    }
}
