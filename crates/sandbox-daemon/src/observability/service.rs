//! Daemon observability: emit periodic `obs.sample` lines through the leaf
//! `Observer`, rotate the one log when it grows past the configured cap, and
//! reshape live runtime state + the latest samples into the `snapshot`/`cgroup`
//! views. No storage engine, no write-time deltas — deltas are read-time.

use std::sync::{Mutex, PoisonError};

use sandbox_observability::collect::cgroup::CgroupSample;
use sandbox_observability::collect::disk;
use sandbox_observability::{
    record, sample_layerstack, Event, ObservabilityPaths, Observer, ObserverConfig, RawFilter,
    Reader, SampleDelta, Sink, SpanNode,
};
use sandbox_runtime::{
    RuntimeNamespaceExecutionSnapshot, RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot,
    SandboxRuntimeOperations,
};
use serde_json::{json, Map, Value};

use crate::server::ServerConfig;

/// The window used to fetch the single newest sample of a scope, independent of
/// the bounded trend window.
const LATEST_SAMPLE_WINDOW_MS: i64 = i64::MAX / 4;

/// Metric keys the daemon emits as monotonic counters; the `Reader` Δs exactly
/// these at read time.
const COUNTER_KEYS: &[&str] = &["cpu_usec"];

pub struct DaemonObservability {
    sandbox_id: String,
    paths: ObservabilityPaths,
    observer: Observer,
    max_file_bytes: u64,
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
            Sink::new(paths.log_path().to_path_buf()),
        );
        Some(Self {
            sandbox_id,
            paths,
            observer,
            max_file_bytes: config.observability.max_file_bytes,
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
            self.observer.sample(scope, workspace_metrics(workspace));
        }
    }

    fn emit_stack_sample(&self, operations: &SandboxRuntimeOperations) {
        let Ok(observation) = operations.observe_layerstack() else {
            return;
        };
        let bytes = sample_layerstack(operations.layer_stack_root());
        self.observer.sample(
            "stack",
            json!({
                "layer_count": observation.layers.len(),
                "layers_bytes": bytes.total_bytes,
                "active_leases": observation.active_lease_count,
            }),
        );
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

    fn reader(&self) -> Reader {
        Reader::new(
            self.paths.log_path().to_path_buf(),
            self.paths.rotated_log_path().to_path_buf(),
        )
    }

    /// The live `snapshot` view: entity state from the runtime registry joined
    /// with the latest `Sample` per scope. The log is read only for "resources
    /// (latest)"; entity state never comes from it.
    pub(crate) fn snapshot_value(&self, snapshot: RuntimeObservabilitySnapshot) -> Value {
        let reader = self.reader();
        let RuntimeObservabilitySnapshot {
            workspaces,
            active_namespace_executions,
            partial_errors,
        } = snapshot;
        let availability = if partial_errors.is_empty() {
            "available"
        } else {
            "partial"
        };
        json!({
            "sandbox_id": self.sandbox_id,
            "lifecycle_state": "ready",
            "availability": availability,
            "sampled_at_unix_ms": super::unix_ms(),
            "errors": partial_errors,
            "daemon": {
                "daemon_pid": std::process::id(),
                "runtime_dir": self.paths.daemon_runtime_dir().to_string_lossy(),
            },
            "resources": resource_bundle(&reader, "sandbox"),
            "workspaces": workspaces
                .iter()
                .map(|workspace| workspace_value(&reader, workspace, &active_namespace_executions))
                .collect::<Vec<_>>(),
        })
    }

    /// The `cgroup` view: the per-scope sample series with read-time deltas.
    pub(crate) fn cgroup_series(&self, scope: &str, window_ms: u64) -> Value {
        let series = self
            .reader()
            .samples(scope, window_to_i64(window_ms))
            .iter()
            .map(sample_delta_value)
            .collect::<Vec<_>>();
        Value::Array(series)
    }

    /// Stack samples within `window_ms`, for the layerstack `--window-ms` trend.
    pub(crate) fn stack_trend(&self, window_ms: u64) -> Vec<Value> {
        self.reader()
            .samples("stack", window_to_i64(window_ms))
            .iter()
            .map(|delta| {
                let mut value = delta.metrics.clone();
                value.insert("ts".to_owned(), json!(delta.ts));
                Value::Object(value)
            })
            .collect()
    }

    /// The newest workspace upper-bytes reading, for the per-session layerstack
    /// view.
    pub(crate) fn latest_upper_bytes(&self, scope: &str) -> Option<u64> {
        latest_sample(&self.reader(), scope)?
            .metrics
            .get("disk_bytes")?
            .as_u64()
    }

    /// The `raw` view: verbatim log lines kept by the filter, ordered by `ts`.
    pub(crate) fn raw_lines(&self, filter: RawFilter) -> Vec<String> {
        self.reader().raw(filter)
    }

    /// The `events` view: parsed `Event` records kept by the same filter shape.
    pub(crate) fn events(&self, filter: RawFilter) -> Vec<Event> {
        self.reader().events(filter)
    }

    /// The `trace` view: one flow folded into a span forest from the log.
    pub(crate) fn trace(&self, id: &str) -> Vec<SpanNode> {
        self.reader().trace(id)
    }
}

fn window_to_i64(window_ms: u64) -> i64 {
    i64::try_from(window_ms).unwrap_or(i64::MAX)
}

fn latest_sample(reader: &Reader, scope: &str) -> Option<SampleDelta> {
    reader.samples(scope, LATEST_SAMPLE_WINDOW_MS).pop()
}

fn resource_bundle(reader: &Reader, scope: &str) -> Value {
    let latest = latest_sample(reader, scope)
        .map(|delta| sample_delta_value(&delta))
        .unwrap_or(Value::Null);
    json!({ "latest": latest, "history": [] })
}

fn sample_delta_value(delta: &SampleDelta) -> Value {
    json!({
        "ts": delta.ts,
        "sample_delta_ms": delta.sample_delta_ms,
        "metrics": delta.metrics,
        "deltas": delta.deltas,
    })
}

fn workspace_value(
    reader: &Reader,
    workspace: &RuntimeWorkspaceSnapshot,
    executions: &[RuntimeNamespaceExecutionSnapshot],
) -> Value {
    let scope = workspace.workspace_id.0.as_str();
    json!({
        "workspace_id": scope,
        "lifecycle_state": "active",
        "network_profile": workspace.network.as_str(),
        "layers": {
            "base_manifest_version": workspace.base_manifest_version,
            "base_root_hash": workspace.base_root_hash,
            "layer_count": workspace.layer_count,
        },
        "namespace_fd_count": workspace.namespace_fd_count,
        "resources": resource_bundle(reader, scope),
        "active_namespace_executions": executions
            .iter()
            .filter(|execution| execution.workspace_session_id.0 == scope)
            .map(namespace_execution_value)
            .collect::<Vec<_>>(),
    })
}

fn namespace_execution_value(execution: &RuntimeNamespaceExecutionSnapshot) -> Value {
    json!({
        "namespace_execution_id": execution.namespace_execution_id.0,
        "operation": execution.operation_name,
        "lifecycle_state": "running",
    })
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

fn workspace_metrics(workspace: &RuntimeWorkspaceSnapshot) -> Value {
    let cgroup = match workspace.cgroup_path.as_deref() {
        Some(cgroup_path) => CgroupSample::read(cgroup_path),
        None => CgroupSample::unavailable("workspace cgroup unavailable"),
    };
    let mut metrics = cgroup_metrics(&cgroup);
    if let Some(upperdir) = workspace.upperdir.as_deref() {
        let disk = disk::sample_upperdir(upperdir);
        if let Some(bytes) = disk.upperdir_bytes {
            metrics.insert("disk_bytes".to_owned(), json!(bytes));
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
