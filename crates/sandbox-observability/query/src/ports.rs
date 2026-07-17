use sandbox_observability_telemetry::{LayerStackBytes, Reader};
use sandbox_observability_telemetry::collect::process_topology::WorkspaceProcessTopology;
use sandbox_runtime_layerstack::service::StackObservation;
use sandbox_runtime_layerstack::LayerDeltaDescription;

pub struct QueryContext {
    pub reader: Reader,
    pub sandbox_id: String,
    pub daemon_pid: u32,
    pub runtime_dir: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryLimits {
    pub resource_window_ms: u64,
    pub layer_delta_default_limit: usize,
    pub layer_delta_max_limit: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservabilitySnapshot {
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub active_namespace_executions: Vec<NamespaceExecutionSnapshot>,
    pub partial_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    pub workspace_id: String,
    pub network_profile: String,
    pub finalize_policy: String,
    pub namespace_fd_count: Option<usize>,
    pub base_root_hash: Option<String>,
    pub layer_count: Option<usize>,
    pub layer_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceExecutionSnapshot {
    pub namespace_execution_id: String,
    pub workspace_session_id: String,
    pub operation_name: String,
}

pub trait ObservabilityInput {
    fn query_context(&self) -> Option<QueryContext>;

    fn query_limits(&self) -> QueryLimits;

    fn cgroup_topology(&self) -> WorkspaceProcessTopology;

    fn observability_snapshot(&self) -> ObservabilitySnapshot;

    fn observe_layerstack(&self) -> Result<StackObservation, String>;

    fn layerstack_bytes(&self) -> LayerStackBytes;

    fn describe_layer_delta(
        &self,
        layer_path: &str,
        limit: usize,
    ) -> Result<LayerDeltaDescription, String>;
}
