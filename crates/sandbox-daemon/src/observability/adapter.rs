use sandbox_config::configs::observability::ViewsConfig;
use sandbox_observability_query::ports::{
    DaemonMetricsRequestClass, NamespaceExecutionSnapshot, ObservabilityInput,
    ObservabilitySnapshot, QueryContext, QueryLimits, ResourceQueryContext, WorkspaceSnapshot,
};
use sandbox_observability_telemetry::collect::process_topology::{
    AppliedCgroupLimits, DaemonDiagnosticWorkspaceHolder, DaemonLifecycleMetrics,
    DaemonOwnershipMetrics, DaemonProcessMetrics, DaemonRuntimeConfigMetrics, DaemonRuntimeUsage,
    WorkspaceProcessInput, WorkspaceProcessTopology,
};
use sandbox_observability_telemetry::{sample_layerstack, LayerStackBytes, WalkBudget};
use sandbox_runtime::{
    workspace_session::HolderLifecycleEventKind, LayerDeltaDescription,
    RuntimeNamespaceExecutionSnapshot, RuntimeObservabilitySnapshot,
    RuntimeOwnershipTopologySnapshot, RuntimeWorkspaceSnapshot, SandboxRuntimeOperations,
    StackObservation,
};
use tokio_util::task::TaskTracker;

use super::DaemonObservability;
use crate::rpc::{BlockingAdmission, ConnectionAdmission};

pub(crate) struct DaemonObservabilityAdapter<'a> {
    state: (
        &'a SandboxRuntimeOperations,
        Option<&'a DaemonObservability>,
        &'a BlockingAdmission,
        &'a ConnectionAdmission,
        &'a TaskTracker,
    ),
}

impl<'a> DaemonObservabilityAdapter<'a> {
    pub(crate) const fn new(
        operations: &'a SandboxRuntimeOperations,
        observability: Option<&'a DaemonObservability>,
        blocking_admission: &'a BlockingAdmission,
        connection_admission: &'a ConnectionAdmission,
        async_tasks: &'a TaskTracker,
    ) -> Self {
        Self {
            state: (
                operations,
                observability,
                blocking_admission,
                connection_admission,
                async_tasks,
            ),
        }
    }

    fn operations(&self) -> &SandboxRuntimeOperations {
        self.state.0
    }

    fn observability(&self) -> Option<&DaemonObservability> {
        self.state.1
    }

    fn blocking_admission(&self) -> &BlockingAdmission {
        self.state.2
    }

    fn connection_admission(&self) -> &ConnectionAdmission {
        self.state.3
    }

    fn async_tasks(&self) -> &TaskTracker {
        self.state.4
    }

    fn collect_daemon_metrics(
        &self,
        snapshot: &RuntimeOwnershipTopologySnapshot,
        request_class: DaemonMetricsRequestClass,
    ) -> DaemonProcessMetrics {
        let mut daemon =
            DaemonProcessMetrics::collect(std::path::Path::new("/proc"), std::process::id());
        let workspace_holders = snapshot
            .workspaces
            .iter()
            .map(|workspace| DaemonDiagnosticWorkspaceHolder {
                workspace_id: workspace.workspace_id.0.clone(),
                holder_pid: workspace.holder_pid,
            })
            .collect::<Vec<_>>();
        let blocking_admission_in_use = self.blocking_admission().in_use();
        let connection_admission_in_use = self.connection_admission().in_use();
        let runtime_usage = DaemonRuntimeUsage {
            active_async_tasks: Some(self.async_tasks().len()),
            active_blocking_tasks: Some(blocking_admission_in_use),
            blocking_queue_depth: Some(0),
            blocking_admission_in_use: Some(blocking_admission_in_use),
            connection_admission_in_use: Some(connection_admission_in_use),
            active_commands: Some(snapshot.active_command_count),
            command_queue_depth: Some(0),
        };
        let namespace_control_fd_count = snapshot
            .ownership
            .namespace_fd_count
            .zip(snapshot.ownership.control_fd_count)
            .and_then(|(namespace, control)| namespace.checked_add(control));
        let ownership = DaemonOwnershipMetrics {
            open_workspaces: snapshot.workspaces.len(),
            live_holders: snapshot
                .workspaces
                .iter()
                .filter(|workspace| workspace.holder_live)
                .count(),
            exited_unreaped_holders: snapshot.ownership.exited_unreaped_holders,
            namespace_fd_count: snapshot.ownership.namespace_fd_count,
            control_fd_count: snapshot.ownership.control_fd_count,
            namespace_control_fd_count,
            active_scratch_directories: snapshot.ownership.active_scratch_directories,
            persisted_workspace_handles: snapshot.ownership.persisted_workspace_handles,
            active_layer_leases: Some(snapshot.active_layer_lease_count),
        };
        let lifecycle = map_lifecycle(
            self.operations()
                .workspace_session
                .holder_lifecycle_snapshot(),
        );
        daemon.runtime_config = self
            .observability()
            .map_or_else(DaemonRuntimeConfigMetrics::default, |observability| {
                observability.runtime_config()
            });
        daemon.runtime_usage = runtime_usage;
        daemon.ownership = ownership;
        daemon.lifecycle = lifecycle;
        daemon.allocator = super::allocator::collect_current();
        if let Some(observability) = self.observability() {
            daemon.diagnostics = observability.observe_diagnostics(
                request_class,
                &daemon,
                &daemon.runtime_usage,
                &daemon.ownership,
                &workspace_holders,
            );
        }
        daemon
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
            sink_stats: observability.observer().sink_stats(),
        })
    }

    fn resource_query_context(&self) -> Option<ResourceQueryContext> {
        let observability = self.observability()?;
        Some(ResourceQueryContext {
            reader: observability.resource_reader(),
            sandbox_id: observability.sandbox_id().to_owned(),
            sink_stats: observability.resource_sink_stats(),
            collection_failures: observability.resource_collection_failures(),
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

    fn cgroup_topology(
        &self,
        request_class: DaemonMetricsRequestClass,
    ) -> WorkspaceProcessTopology {
        let snapshot = self.operations().ownership_topology_snapshot();
        let daemon = self.collect_daemon_metrics(&snapshot, request_class);
        if snapshot.workspaces.is_empty() && !snapshot.partial_errors.is_empty() {
            let mut topology = WorkspaceProcessTopology::unavailable(format!(
                "runtime workspace snapshot failed: {}",
                snapshot.partial_errors.join("; ")
            ));
            topology.daemon = Some(daemon);
            return topology;
        }
        let workspaces = snapshot
            .workspaces
            .into_iter()
            .map(|workspace| WorkspaceProcessInput {
                workspace_id: workspace.workspace_id.0,
                holder_pid: u32::try_from(workspace.holder_pid).unwrap_or(0),
                cgroup_path: workspace.cgroup_path.as_deref().map(hierarchy_cgroup_path),
                applied_cgroup_limits: workspace.applied_cgroup_limits.map(|limits| {
                    AppliedCgroupLimits {
                        nano_cpus: limits.nano_cpus,
                        memory_high_bytes: limits.memory_high_bytes,
                        memory_max_bytes: limits.memory_max_bytes,
                        pids_max: limits.pids_max,
                    }
                }),
                workload_cgroup_state: workspace.workload_cgroup_state,
                workload_cgroup_reason: workspace.workload_cgroup_reason,
            })
            .collect();
        let mut topology =
            WorkspaceProcessTopology::collect(std::path::Path::new("/proc"), workspaces);
        topology.daemon = Some(daemon);
        topology
    }

    fn daemon_metrics(&self, request_class: DaemonMetricsRequestClass) -> DaemonProcessMetrics {
        let snapshot = self.operations().ownership_topology_snapshot();
        self.collect_daemon_metrics(&snapshot, request_class)
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

/// Convert the runtime's controller-filesystem path into the unified cgroup
/// hierarchy coordinates used by `/proc/*/cgroup` and the public topology.
/// Keeping both daemon and workspace rows in one coordinate system also makes
/// the public path safe to append to `/sys/fs/cgroup` for independent reads.
pub(crate) fn hierarchy_cgroup_path(path: &std::path::Path) -> String {
    const CGROUP_FS_ROOT: &str = "/sys/fs/cgroup";
    let relative = path.strip_prefix(CGROUP_FS_ROOT).unwrap_or(path);
    if relative.as_os_str().is_empty() {
        return "/".to_owned();
    }
    let text = relative.to_string_lossy();
    if text.starts_with('/') {
        text.into_owned()
    } else {
        format!("/{text}")
    }
}

pub(crate) fn map_lifecycle(
    snapshot: sandbox_runtime::workspace_session::HolderLifecycleSnapshot,
) -> DaemonLifecycleMetrics {
    let mut last_holder_exit_reason = None;
    let mut last_cleanup_failure = None;
    let mut last_cleanup_result = None;
    let mut last_cleanup_duration_ms = None;
    for event in snapshot.events.iter().rev() {
        match event.kind {
            HolderLifecycleEventKind::ExitObserved if last_holder_exit_reason.is_none() => {
                last_holder_exit_reason = Some(bounded_summary(&event.detail));
            }
            HolderLifecycleEventKind::CleanupTerminal if last_cleanup_result.is_none() => {
                last_cleanup_result = Some(bounded_summary(&event.detail));
                last_cleanup_duration_ms = event.cleanup_duration_ms;
            }
            HolderLifecycleEventKind::CleanupFailure if last_cleanup_failure.is_none() => {
                last_cleanup_failure = Some(bounded_summary(&event.detail));
            }
            _ => {}
        }
        if last_holder_exit_reason.is_some() && last_cleanup_result.is_some() {
            break;
        }
    }
    DaemonLifecycleMetrics {
        holder_exit_total: snapshot.holder_exit_total,
        cleanup_attempt_total: snapshot.cleanup_attempt_total,
        cleanup_failure_total: snapshot.cleanup_failure_total,
        cleanup_terminal_total: snapshot.cleanup_terminal_total,
        dropped_event_total: snapshot.dropped_event_total,
        retained_event_count: snapshot.events.len(),
        last_holder_exit_reason,
        last_cleanup_failure,
        last_cleanup_result,
        last_cleanup_duration_ms,
    }
}

fn bounded_summary(value: &str) -> String {
    const LIMIT: usize = 512;
    if value.len() <= LIMIT {
        return value.to_owned();
    }
    let mut end = LIMIT;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
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
        finalization_state: workspace.finalization_state.as_str().to_owned(),
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
