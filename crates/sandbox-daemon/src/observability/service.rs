//! Daemon observability sampling, rotation, and lifecycle. `collect()` emits a
//! compact append-only log through `sandbox-observability-telemetry`.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use sandbox_config::configs::observability::ViewsConfig;
use sandbox_observability_telemetry::collect::cgroup::CgroupSample;
use sandbox_observability_telemetry::collect::disk;
use sandbox_observability_telemetry::{
    record, sample_layerstack, LayerStackBytes, ObservabilityPaths, Observer, ObserverConfig,
    Reader, Sink, WalkBudget,
};
use sandbox_runtime::{
    RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot, SandboxRuntimeOperations,
};
use serde_json::{json, Map, Value};

use crate::rpc::ServerConfig;

/// Metric keys the daemon emits as monotonic counters; the `Reader` Δs exactly
/// these at read time.
const COUNTER_KEYS: &[&str] = &["cpu_usec"];

pub struct DaemonObservability {
    sandbox_id: String,
    paths: ObservabilityPaths,
    observer: Observer,
    max_file_bytes: u64,
    pub(crate) sampling: WalkBudget,
    pub(crate) views: ViewsConfig,
    rotate_lock: Mutex<()>,
}

impl DaemonObservability {
    pub(crate) fn from_config(config: &ServerConfig) -> Option<Self> {
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
            Sink::new(
                paths.log_path().to_path_buf(),
                config.observability.max_line_bytes,
            ),
        );
        Some(Self {
            sandbox_id,
            paths,
            observer,
            max_file_bytes: config.observability.max_file_bytes,
            sampling: WalkBudget {
                max_nodes: config.observability.sampling.max_walk_nodes,
                max_depth: config.observability.sampling.max_walk_depth,
            },
            views: config.observability.views,
            rotate_lock: Mutex::new(()),
        })
    }

    /// A clone of the one process `Observer`. The runtime gets this same handle
    /// so daemon (`d-*`) and runtime spans share one id sequence and parent chain.
    pub(crate) fn observer(&self) -> Observer {
        self.observer.clone()
    }

    /// One periodic tick: rotate if oversized, then emit one `obs.sample` per
    /// scope (`sandbox`, each workspace, `stack`). Best-effort throughout.
    pub(crate) fn collect(&self, config: &ServerConfig, operations: &SandboxRuntimeOperations) {
        self.rotate_if_needed();
        self.emit_resource_samples(config, &operations.observability_snapshot());
        self.emit_stack_sample(operations);
    }

    /// Emit the `sandbox` sample plus one per live workspace.
    pub(crate) fn emit_resource_samples(
        &self,
        config: &ServerConfig,
        snapshot: &RuntimeObservabilitySnapshot,
    ) {
        self.observer
            .sample("sandbox", sandbox_metrics(&sandbox_cgroup_sample(config)));
        for workspace in &snapshot.workspaces {
            let scope = workspace.workspace_id.0.as_str();
            if scope.is_empty() {
                continue;
            }
            self.observer
                .sample(scope, workspace_metrics(workspace, self.sampling));
        }
    }

    /// Persist the resource samples used by a snapshot response. Unlike the
    /// periodic path, this is fallible so a snapshot never presents an older
    /// sample as the current post-operation reading after an append failure.
    pub(crate) fn refresh_resource_samples(
        &self,
        config: &ServerConfig,
        snapshot: &RuntimeObservabilitySnapshot,
    ) -> std::io::Result<()> {
        self.rotate_if_needed();
        self.observer
            .try_sample("sandbox", sandbox_metrics(&sandbox_cgroup_sample(config)))?;
        for workspace in &snapshot.workspaces {
            let scope = workspace.workspace_id.0.as_str();
            if scope.is_empty() {
                continue;
            }
            self.observer
                .try_sample(scope, workspace_metrics(workspace, self.sampling))?;
        }
        Ok(())
    }

    fn emit_stack_sample(&self, operations: &SandboxRuntimeOperations) {
        let Ok(observation) = operations.observe_layerstack() else {
            return;
        };
        let bytes = sample_layerstack(operations.layer_stack_root(), self.sampling);
        self.observer.sample(
            "stack",
            Self::stack_metrics(
                observation.layers.len(),
                observation.active_lease_count,
                &bytes,
            ),
        );
    }

    pub(crate) fn stack_metrics(
        layer_count: usize,
        active_lease_count: usize,
        bytes: &LayerStackBytes,
    ) -> Value {
        json!({
            "layer_count": layer_count,
            "layers_bytes": bytes.total_bytes,
            "layers_allocated_bytes": bytes.total_allocated_bytes,
            "storage_logical_bytes": bytes.storage_logical_bytes,
            "storage_allocated_bytes": bytes.storage_allocated_bytes,
            "staging_entry_count": bytes.staging_entry_count,
            "active_leases": active_lease_count,
        })
    }

    /// Rotate `observability.ndjson` → `observability.ndjson.1` (replacing any
    /// prior `.1`) once it exceeds the cap. Serialized by `rotate_lock` with the
    /// size re-checked under it, so two concurrent `collect()` ticks can't both
    /// rename and clobber freshly rotated history. The `Sink` opens its fd per
    /// append, so the next sample re-creates the primary log — no explicit reopen.
    fn rotate_if_needed(&self) {
        let _guard = self
            .rotate_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Ok(metadata) = std::fs::metadata(self.paths.log_path()) else {
            return;
        };
        if metadata.len() <= self.max_file_bytes {
            return;
        }
        let _ = std::fs::rename(self.paths.log_path(), self.paths.rotated_log_path());
    }

    pub(super) fn reader(&self) -> Reader {
        Reader::new(
            self.paths.log_path().to_path_buf(),
            self.paths.rotated_log_path().to_path_buf(),
        )
    }

    pub(super) fn sandbox_id(&self) -> &str {
        &self.sandbox_id
    }

    pub(super) fn runtime_dir(&self) -> &Path {
        self.paths.daemon_runtime_dir()
    }
}

fn sandbox_cgroup_sample(config: &ServerConfig) -> CgroupSample {
    match &config.cgroup_root {
        Some(cgroup_root) => CgroupSample::read(cgroup_root),
        None => CgroupSample::unavailable("cgroup root unavailable"),
    }
}

fn sandbox_metrics(cgroup: &CgroupSample) -> Value {
    let mut metrics = cgroup_metrics(cgroup);
    tag_counters(&mut metrics);
    Value::Object(metrics)
}

fn workspace_metrics(workspace: &RuntimeWorkspaceSnapshot, sampling: WalkBudget) -> Value {
    let cgroup = match workspace.cgroup_path.as_deref() {
        Some(cgroup_path) => CgroupSample::read(cgroup_path),
        None => CgroupSample::unavailable("workspace cgroup unavailable"),
    };
    let mut metrics = cgroup_metrics(&cgroup);
    if let Some(upperdir) = workspace.upperdir.as_deref() {
        let disk = disk::sample_upperdir(upperdir, sampling);
        if let Some(bytes) = disk.upperdir_bytes {
            metrics.insert("disk_bytes".to_owned(), json!(bytes));
        }
        if let Some(bytes) = disk.upperdir_allocated_bytes {
            metrics.insert("disk_allocated_bytes".to_owned(), json!(bytes));
        }
        if let Some(files) = disk.file_count {
            metrics.insert("files".to_owned(), json!(files));
        }
        if disk.truncated == Some(true) {
            metrics.insert("disk_truncated".to_owned(), json!(true));
        }
    }
    tag_counters(&mut metrics);
    Value::Object(metrics)
}

fn cgroup_metrics(cgroup: &CgroupSample) -> Map<String, Value> {
    let mut metrics = Map::new();
    if let Some(cpu_usec) = cgroup.cpu_usage_usec {
        metrics.insert("cpu_usec".to_owned(), json!(cpu_usec));
    }
    if let Some(mem_cur) = cgroup.memory_current_bytes {
        metrics.insert("mem_cur".to_owned(), json!(mem_cur));
    }
    if let Some(mem_max) = cgroup.memory_max_bytes {
        metrics.insert("mem_max".to_owned(), json!(mem_max));
    }
    if cgroup.memory_max_unlimited == Some(true) {
        metrics.insert("mem_max_unlimited".to_owned(), json!(true));
    }
    if !cgroup.cgroup_available {
        metrics.insert("cgroup_available".to_owned(), json!(false));
    }
    if let Some(cgroup_error) = &cgroup.cgroup_error {
        metrics.insert("cgroup_error".to_owned(), json!(cgroup_error));
    }
    metrics
}

fn tag_counters(metrics: &mut Map<String, Value>) {
    metrics.insert(record::COUNTERS_METRIC_KEY.to_owned(), json!(COUNTER_KEYS));
}
