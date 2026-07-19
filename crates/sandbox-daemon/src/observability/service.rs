//! Daemon observability query metadata and event access.

use std::path::Path;
use std::sync::Arc;

use sandbox_config::configs::observability::ViewsConfig;
use sandbox_observability_query::ports::DaemonMetricsRequestClass;
use sandbox_observability_telemetry::collect::process_topology::{
    DaemonDiagnosticState, DaemonDiagnosticWorkspaceHolder, DaemonOwnershipMetrics,
    DaemonProcessMetrics, DaemonRuntimeConfigMetrics, DaemonRuntimeUsage,
};
use sandbox_observability_telemetry::{
    record, ObservabilityPaths, Observer, ObserverConfig, Reader, Sink, WalkBudget,
};
use sandbox_runtime::SandboxRuntimeConfig;

use crate::rpc::ServerConfig;

use super::diagnostics::DiagnosticTracker;
use super::resources::ResourceSampler;

const INFRASTRUCTURE_THREAD_ALLOWANCE: usize = 4;

pub struct DaemonObservability {
    sandbox_id: String,
    paths: ObservabilityPaths,
    observer: Observer,
    resource_sampler: Arc<ResourceSampler>,
    runtime_config: DaemonRuntimeConfigMetrics,
    diagnostics: DiagnosticTracker,
    pub(crate) sampling: WalkBudget,
    pub(crate) views: ViewsConfig,
}

impl DaemonObservability {
    pub(crate) fn from_config(
        config: &ServerConfig,
        runtime: &SandboxRuntimeConfig,
    ) -> Option<Self> {
        let sandbox_id = config
            .sandbox_id
            .as_ref()
            .filter(|sandbox_id| !sandbox_id.is_empty())?
            .clone();
        let paths = ObservabilityPaths::from_socket_path(config.socket_path.clone()).ok()?;
        let observer = Observer::new(
            ObserverConfig {
                proc: record::proc::DAEMON,
                enabled: config.observability.enabled,
            },
            Sink::with_budget(
                paths.log_path().to_path_buf(),
                config.observability.max_line_bytes,
                config.observability.max_disk_bytes,
            ),
        );
        let resource_sampler = Arc::new(ResourceSampler::new(
            config.observability.resource_stats,
            paths.resource_log_path().to_path_buf(),
        ));
        let diagnostics = DiagnosticTracker::new(
            config.observability.diagnostics,
            paths.observability_dir().join("daemon-diagnostic.json"),
        );
        Some(Self {
            sandbox_id,
            paths,
            observer,
            resource_sampler,
            runtime_config: DaemonRuntimeConfigMetrics {
                worker_threads: Some(config.worker_threads),
                max_blocking_threads: Some(config.max_blocking_requests),
                blocking_thread_keep_alive_s: Some(config.blocking_thread_keep_alive_s),
                max_concurrent_connections: Some(config.max_concurrent_connections),
                max_active_commands: Some(runtime.command.max_active),
                max_blocking_queue_depth: Some(0),
                max_command_queue_depth: Some(0),
                infrastructure_thread_allowance: Some(INFRASTRUCTURE_THREAD_ALLOWANCE),
            },
            diagnostics,
            sampling: WalkBudget {
                max_nodes: config.observability.sampling.max_walk_nodes,
                max_depth: config.observability.sampling.max_walk_depth,
            },
            views: config.observability.views,
        })
    }

    /// A clone of the one process `Observer`. The runtime gets this same handle
    /// so daemon (`d-*`) and runtime spans share one id sequence and parent chain.
    pub(crate) fn observer(&self) -> Observer {
        self.observer.clone()
    }

    pub(super) fn reader(&self) -> Reader {
        Reader::with_limits(
            self.paths.log_path().to_path_buf(),
            self.paths.rotated_log_path().to_path_buf(),
            self.observer.max_line_bytes(),
            sandbox_observability_telemetry::MAX_RESPONSE_RECORDS,
            sandbox_observability_telemetry::MAX_RESPONSE_BYTES,
        )
    }

    pub(super) fn resource_reader(&self) -> Reader {
        Reader::with_limits(
            self.paths.resource_log_path().to_path_buf(),
            self.paths.rotated_resource_log_path().to_path_buf(),
            sandbox_observability_telemetry::MAX_LINE_BYTES,
            sandbox_observability_telemetry::MAX_RESPONSE_RECORDS,
            sandbox_observability_telemetry::MAX_RESPONSE_BYTES,
        )
    }

    pub(crate) fn start_resource_sampler(
        &self,
        tasks: &tokio_util::task::TaskTracker,
        shutdown: tokio_util::sync::CancellationToken,
    ) {
        self.resource_sampler.start(tasks, shutdown);
    }

    pub(super) fn resource_sink_stats(&self) -> sandbox_observability_telemetry::SinkStats {
        self.resource_sampler.sink_stats()
    }

    pub(super) fn resource_collection_failures(&self) -> u64 {
        self.resource_sampler.collection_failures()
    }

    pub(super) fn sandbox_id(&self) -> &str {
        &self.sandbox_id
    }

    pub(super) fn runtime_dir(&self) -> &Path {
        self.paths.daemon_runtime_dir()
    }

    pub(super) fn runtime_config(&self) -> DaemonRuntimeConfigMetrics {
        self.runtime_config.clone()
    }

    pub(super) fn observe_diagnostics(
        &self,
        request_class: DaemonMetricsRequestClass,
        process: &DaemonProcessMetrics,
        runtime_usage: &DaemonRuntimeUsage,
        ownership: &DaemonOwnershipMetrics,
        workspace_holders: &[DaemonDiagnosticWorkspaceHolder],
    ) -> DaemonDiagnosticState {
        self.diagnostics.observe(
            request_class,
            process,
            runtime_usage,
            ownership,
            workspace_holders,
        )
    }
}
