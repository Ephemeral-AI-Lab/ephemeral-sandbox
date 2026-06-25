use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sandbox_observability::{
    ObservabilityNamespaceExecutionSnapshotRow, ObservabilityPaths, ObservabilityResourceSampleRow,
    ObservabilitySnapshotReadOptions, ObservabilitySnapshotRows, ObservabilityStore,
    ObservabilityWorkspaceSnapshotRow, ResourceSampleRecord, SandboxSnapshotRecord, StoreError,
    WorkspaceSnapshotRecord, MAX_ERROR_MESSAGE_LENGTH, MAX_ID_LENGTH, MAX_KIND_LENGTH,
    MAX_PATH_LENGTH,
};
use sandbox_runtime::{
    RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot, SandboxRuntimeOperations,
};

use crate::server::ServerConfig;

use super::cgroup::CgroupSample;
use super::disk::{self, DiskSample};
use super::namespace_execution;
use serde_json::{json, Value};

const DISK_SAMPLE_MIN_INTERVAL: Duration = Duration::from_secs(10);
const MAX_RESOURCE_WINDOW_MS: u64 = 600_000;

pub struct DaemonObservability {
    sandbox_id: String,
    paths: ObservabilityPaths,
    pub(crate) store: ObservabilityStore,
    next_sample_id: AtomicU64,
    disk_samples: Mutex<HashMap<DiskCacheKey, CachedDiskSample>>,
    resource_counters: Mutex<HashMap<ResourceScopeKey, PreviousResourceCounters>>,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct DiskCacheKey {
    workspace_id: String,
    upperdir: String,
}

#[derive(Debug, Clone)]
struct CachedDiskSample {
    sampled_at: Instant,
    sample: DiskSample,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct ResourceScopeKey {
    workspace_id: Option<String>,
}

#[derive(Debug, Clone)]
struct PreviousResourceCounters {
    sampled_at_unix_ms: i64,
    cpu_usage_usec: Option<i64>,
    memory_current_bytes: Option<i64>,
    disk_upperdir_bytes: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ResourceDeltas {
    sample_delta_ms: Option<i64>,
    cpu_usage_delta_usec: Option<i64>,
    memory_current_delta_bytes: Option<i64>,
    disk_upperdir_delta_bytes: Option<i64>,
}

impl DaemonObservability {
    pub(crate) fn from_config(config: &ServerConfig) -> Option<Self> {
        let sandbox_id = config
            .sandbox_id
            .as_ref()
            .filter(|sandbox_id| !sandbox_id.is_empty())?
            .clone();
        let paths = ObservabilityPaths::from_socket_path(config.socket_path.clone()).ok()?;
        let store = ObservabilityStore::open(&paths).ok()?;
        Some(Self {
            sandbox_id: bound_id(sandbox_id),
            paths,
            store,
            next_sample_id: AtomicU64::new(1),
            disk_samples: Mutex::new(HashMap::new()),
            resource_counters: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn collect(
        &self,
        config: &ServerConfig,
        operations: &SandboxRuntimeOperations,
    ) -> Result<(), StoreError> {
        self.write_snapshot(
            config,
            operations.observability_snapshot(),
            unix_ms(),
            false,
        )
    }

    pub(crate) fn observability_snapshot_response(
        &self,
        request: &sandbox_protocol::Request,
    ) -> sandbox_protocol::Response {
        let options = match snapshot_read_options(request) {
            Ok(options) => options,
            Err(response) => return response,
        };
        match self
            .store
            .read_observability_snapshot(&self.sandbox_id, &options)
        {
            Ok(rows) => match snapshot_value(rows) {
                Ok(value) => sandbox_protocol::Response::ok(value),
                Err(response) => response,
            },
            Err(error) => sandbox_protocol::Response::fault(
                sandbox_protocol::error_kind::INTERNAL_ERROR,
                format!("observability snapshot read failed: {error}"),
            ),
        }
    }

    pub(crate) fn write_snapshot(
        &self,
        config: &ServerConfig,
        snapshot: RuntimeObservabilitySnapshot,
        sampled_at_unix_ms: i64,
        force_fresh_disk: bool,
    ) -> Result<(), StoreError> {
        let RuntimeObservabilitySnapshot {
            workspaces,
            active_namespace_executions,
            mut partial_errors,
        } = snapshot;

        let mut workspace_records = Vec::new();
        let mut resource_samples = vec![self.resource_record(
            None,
            sampled_at_unix_ms,
            sandbox_cgroup_sample(config),
            DiskSample::empty(),
        )];
        for workspace in &workspaces {
            let Some(workspace_id) = bounded_required_id(
                "workspace_id",
                &workspace.workspace_id.0,
                &mut partial_errors,
            ) else {
                continue;
            };
            workspace_records.push(self.workspace_record(
                workspace,
                workspace_id.clone(),
                sampled_at_unix_ms,
            ));
            resource_samples.push(self.workspace_resource_record(
                &workspace_id,
                workspace.upperdir.as_deref(),
                workspace.cgroup_path.as_deref(),
                sampled_at_unix_ms,
                force_fresh_disk,
            ));
        }

        let mut namespace_execution_records = Vec::new();
        for execution in &active_namespace_executions {
            let Some(namespace_execution_id) = bounded_required_id(
                "namespace_execution_id",
                &execution.namespace_execution_id.0,
                &mut partial_errors,
            ) else {
                continue;
            };
            let Some(workspace_session_id) = bounded_required_id(
                "namespace_execution_workspace_session_id",
                &execution.workspace_session_id.0,
                &mut partial_errors,
            ) else {
                continue;
            };
            namespace_execution_records.push(namespace_execution::snapshot_record(
                &self.sandbox_id,
                execution,
                namespace_execution_id,
                workspace_session_id,
                sampled_at_unix_ms,
            ));
        }

        self.store.upsert_sandbox_snapshot(&self.sandbox_record(
            config,
            sampled_at_unix_ms,
            &partial_errors,
        ))?;

        let active_workspace_ids = workspace_records
            .iter()
            .map(|workspace| workspace.workspace_id.clone())
            .collect::<Vec<_>>();
        self.store
            .upsert_workspace_snapshots(&self.sandbox_id, &workspace_records)?;
        self.store.reconcile_workspace_snapshots(
            &self.sandbox_id,
            &active_workspace_ids,
            sampled_at_unix_ms,
        )?;

        self.store.replace_namespace_execution_snapshots(
            &self.sandbox_id,
            &namespace_execution_records,
        )?;

        self.store.insert_resource_samples(&resource_samples)?;
        Ok(())
    }

    fn sandbox_record(
        &self,
        config: &ServerConfig,
        sampled_at_unix_ms: i64,
        partial_errors: &[String],
    ) -> SandboxSnapshotRecord {
        SandboxSnapshotRecord {
            sandbox_id: self.sandbox_id.clone(),
            state: "ready".to_owned(),
            workspace_root: None,
            daemon_runtime_dir: Some(bound_path(path_string(self.paths.daemon_runtime_dir()))),
            socket_path: Some(bound_path(path_string(&config.socket_path))),
            pid_path: Some(bound_path(path_string(&config.pid_path))),
            daemon_pid: Some(i64::from(std::process::id())),
            sampled_at_unix_ms,
            error_message: error_summary(partial_errors),
        }
    }

    fn workspace_record(
        &self,
        workspace: &RuntimeWorkspaceSnapshot,
        workspace_id: String,
        sampled_at_unix_ms: i64,
    ) -> WorkspaceSnapshotRecord {
        WorkspaceSnapshotRecord {
            sandbox_id: self.sandbox_id.clone(),
            workspace_id,
            state: "active".to_owned(),
            profile: Some(bound_kind(workspace.profile.as_str().to_owned())),
            workspace_root: Some(bound_path(path_string(&workspace.workspace_root))),
            upperdir: workspace
                .upperdir
                .as_deref()
                .map(path_string)
                .map(bound_path),
            workdir: workspace
                .workdir
                .as_deref()
                .map(path_string)
                .map(bound_path),
            namespace_fd_count: workspace.namespace_fd_count.map(usize_to_i64),
            base_manifest_version: workspace.base_manifest_version,
            base_root_hash: workspace.base_root_hash.clone().map(bound_id),
            layer_count: workspace.layer_count.map(usize_to_i64),
            sampled_at_unix_ms,
            error_message: None,
        }
    }

    fn workspace_resource_record(
        &self,
        workspace_id: &str,
        upperdir: Option<&Path>,
        cgroup_path: Option<&Path>,
        sampled_at_unix_ms: i64,
        force_fresh_disk: bool,
    ) -> ResourceSampleRecord {
        let disk = upperdir
            .map(|upperdir| self.disk_sample(workspace_id, upperdir, force_fresh_disk))
            .unwrap_or_else(DiskSample::empty);
        let cgroup = match cgroup_path {
            Some(cgroup_path) => CgroupSample::read(cgroup_path),
            None => CgroupSample::unavailable("workspace cgroup unavailable"),
        };
        self.resource_record(Some(workspace_id), sampled_at_unix_ms, cgroup, disk)
    }

    fn resource_record(
        &self,
        workspace_id: Option<&str>,
        sampled_at_unix_ms: i64,
        cgroup: CgroupSample,
        disk: DiskSample,
    ) -> ResourceSampleRecord {
        let deltas = self.resource_deltas(
            workspace_id,
            sampled_at_unix_ms,
            cgroup.cpu_usage_usec,
            cgroup.memory_current_bytes,
            disk.upperdir_bytes,
        );
        ResourceSampleRecord {
            sample_id: self.next_sample_id(sampled_at_unix_ms),
            sandbox_id: self.sandbox_id.clone(),
            workspace_id: workspace_id.map(str::to_owned),
            sampled_at_unix_ms,
            cgroup_path: cgroup.cgroup_path.map(bound_path),
            cgroup_available: cgroup.cgroup_available,
            cgroup_error: cgroup.cgroup_error.map(bound_error),
            cpu_usage_usec: cgroup.cpu_usage_usec,
            cpu_usage_delta_usec: deltas.cpu_usage_delta_usec,
            sample_delta_ms: deltas.sample_delta_ms,
            memory_current_bytes: cgroup.memory_current_bytes,
            memory_current_delta_bytes: deltas.memory_current_delta_bytes,
            memory_max_bytes: cgroup.memory_max_bytes,
            memory_max_unlimited: cgroup.memory_max_unlimited,
            disk_upperdir_bytes: disk.upperdir_bytes,
            disk_upperdir_delta_bytes: deltas.disk_upperdir_delta_bytes,
            disk_file_count: disk.file_count,
            disk_dir_count: disk.dir_count,
            disk_symlink_count: disk.symlink_count,
            disk_truncated: disk.truncated,
            disk_read_error_count: disk.read_error_count,
            disk_first_error_path: disk.first_error_path.map(bound_path),
        }
    }

    fn resource_deltas(
        &self,
        workspace_id: Option<&str>,
        sampled_at_unix_ms: i64,
        cpu_usage_usec: Option<i64>,
        memory_current_bytes: Option<i64>,
        disk_upperdir_bytes: Option<i64>,
    ) -> ResourceDeltas {
        let key = ResourceScopeKey {
            workspace_id: workspace_id.map(str::to_owned),
        };
        let current = PreviousResourceCounters {
            sampled_at_unix_ms,
            cpu_usage_usec,
            memory_current_bytes,
            disk_upperdir_bytes,
        };
        let mut counters = lock_resource_counters(&self.resource_counters);
        let deltas = counters
            .get(&key)
            .map(|previous| ResourceDeltas {
                sample_delta_ms: sampled_at_unix_ms.checked_sub(previous.sampled_at_unix_ms),
                cpu_usage_delta_usec: checked_non_negative_delta(
                    cpu_usage_usec,
                    previous.cpu_usage_usec,
                ),
                memory_current_delta_bytes: checked_delta(
                    memory_current_bytes,
                    previous.memory_current_bytes,
                ),
                disk_upperdir_delta_bytes: checked_delta(
                    disk_upperdir_bytes,
                    previous.disk_upperdir_bytes,
                ),
            })
            .unwrap_or_default();
        counters.insert(key, current);
        deltas
    }

    fn next_sample_id(&self, sampled_at_unix_ms: i64) -> String {
        let next = self.next_sample_id.fetch_add(1, Ordering::Relaxed);
        format!("sample-{sampled_at_unix_ms}-{next}")
    }

    fn disk_sample(&self, workspace_id: &str, upperdir: &Path, force_fresh: bool) -> DiskSample {
        let key = DiskCacheKey {
            workspace_id: workspace_id.to_owned(),
            upperdir: bound_path(path_string(upperdir)),
        };
        let now = Instant::now();
        if !force_fresh {
            let cache = lock_disk_samples(&self.disk_samples);
            if let Some(cached) = cache.get(&key) {
                if now.duration_since(cached.sampled_at) < DISK_SAMPLE_MIN_INTERVAL {
                    return cached.sample.clone();
                }
            }
        }

        let sample = disk::sample_upperdir(upperdir);
        lock_disk_samples(&self.disk_samples).insert(
            key,
            CachedDiskSample {
                sampled_at: now,
                sample: sample.clone(),
            },
        );
        sample
    }
}

fn unix_ms() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn sandbox_cgroup_sample(config: &ServerConfig) -> CgroupSample {
    match &config.cgroup_root {
        Some(cgroup_root) => CgroupSample::read(cgroup_root),
        None => CgroupSample::unavailable("cgroup root unavailable"),
    }
}

fn snapshot_read_options(
    request: &sandbox_protocol::Request,
) -> Result<ObservabilitySnapshotReadOptions, sandbox_protocol::Response> {
    Ok(ObservabilitySnapshotReadOptions {
        resource_window_ms: request
            .optional_u64("resource_window_ms")?
            .map(|window_ms| window_ms.min(MAX_RESOURCE_WINDOW_MS)),
    })
}

fn snapshot_value(rows: ObservabilitySnapshotRows) -> Result<Value, sandbox_protocol::Response> {
    let Some(sandbox) = rows.sandbox.as_ref() else {
        return Err(sandbox_protocol::Response::fault(
            sandbox_protocol::error_kind::INTERNAL_ERROR,
            "observability root snapshot unavailable",
        ));
    };
    let availability = if snapshot_has_partial_errors(&rows) {
        "partial"
    } else {
        "available"
    };
    Ok(json!({
        "sandbox_id": sandbox.sandbox_id.as_str(),
        "lifecycle_state": sandbox.state.as_str(),
        "availability": availability,
        "sampled_at_unix_ms": sandbox.sampled_at_unix_ms,
        "errors": error_list(sandbox.error_message.as_deref()),
        "daemon": {
            "socket_path": sandbox.socket_path.as_deref(),
            "pid_path": sandbox.pid_path.as_deref(),
            "daemon_pid": sandbox.daemon_pid,
            "runtime_dir": sandbox.daemon_runtime_dir.as_deref(),
        },
        "resources": resource_bundle_value(None, &rows),
        "workspaces": rows
            .workspaces
            .iter()
            .map(|workspace| workspace_value(workspace, &rows))
            .collect::<Vec<_>>(),
    }))
}

fn snapshot_has_partial_errors(rows: &ObservabilitySnapshotRows) -> bool {
    rows.sandbox
        .as_ref()
        .and_then(|sandbox| sandbox.error_message.as_ref())
        .is_some()
        || rows
            .workspaces
            .iter()
            .any(|workspace| workspace.error_message.is_some())
        || rows
            .active_namespace_executions
            .iter()
            .any(|execution| execution.error_message.is_some())
}

fn workspace_value(
    workspace: &ObservabilityWorkspaceSnapshotRow,
    rows: &ObservabilitySnapshotRows,
) -> Value {
    json!({
        "workspace_id": workspace.workspace_id.as_str(),
        "lifecycle_state": workspace.state.as_str(),
        "profile": workspace.profile.as_deref(),
        "sampled_at_unix_ms": workspace.sampled_at_unix_ms,
        "errors": error_list(workspace.error_message.as_deref()),
        "layers": {
            "base_manifest_version": workspace.base_manifest_version,
            "base_root_hash": workspace.base_root_hash.as_deref(),
            "layer_count": workspace.layer_count,
        },
        "namespace_fd_count": workspace.namespace_fd_count,
        "resources": resource_bundle_value(Some(&workspace.workspace_id), rows),
        "active_namespace_executions": rows
            .active_namespace_executions
            .iter()
            .filter(|execution| execution.workspace_session_id == workspace.workspace_id)
            .map(namespace_execution_value)
            .collect::<Vec<_>>(),
    })
}

fn namespace_execution_value(execution: &ObservabilityNamespaceExecutionSnapshotRow) -> Value {
    json!({
        "namespace_execution_id": execution.namespace_execution_id.as_str(),
        "operation": execution.operation.as_str(),
        "lifecycle_state": execution.lifecycle_state.as_str(),
        "sampled_at_unix_ms": execution.sampled_at_unix_ms,
        "error": execution.error_message.as_deref(),
    })
}

fn resource_bundle_value(scope: Option<&str>, rows: &ObservabilitySnapshotRows) -> Value {
    let latest = rows
        .latest_resources
        .iter()
        .find(|sample| sample.workspace_id.as_deref() == scope)
        .map(resource_sample_value)
        .unwrap_or(Value::Null);
    let history = rows
        .resource_history
        .iter()
        .filter(|sample| sample.workspace_id.as_deref() == scope)
        .map(resource_sample_value)
        .collect::<Vec<_>>();
    json!({
        "latest": latest,
        "history": history,
    })
}

fn resource_sample_value(sample: &ObservabilityResourceSampleRow) -> Value {
    json!({
        "sampled_at_unix_ms": sample.sampled_at_unix_ms,
        "sample_delta_ms": sample.sample_delta_ms,
        "cgroup": {
            "available": sample.cgroup_available,
            "cpu_usage_usec": sample.cpu_usage_usec,
            "cpu_usage_delta_usec": sample.cpu_usage_delta_usec,
            "memory_current_bytes": sample.memory_current_bytes,
            "memory_current_delta_bytes": sample.memory_current_delta_bytes,
            "memory_max_bytes": sample.memory_max_bytes,
            "memory_max_unlimited": sample.memory_max_unlimited,
            "error": sample.cgroup_error.as_deref(),
        },
        "disk": {
            "upperdir_bytes": sample.disk_upperdir_bytes,
            "upperdir_delta_bytes": sample.disk_upperdir_delta_bytes,
            "file_count": sample.disk_file_count,
            "dir_count": sample.disk_dir_count,
            "symlink_count": sample.disk_symlink_count,
            "truncated": sample.disk_truncated,
            "read_error_count": sample.disk_read_error_count,
            "first_error_path": sample.disk_first_error_path.as_deref(),
        },
    })
}

fn error_list(error: Option<&str>) -> Vec<&str> {
    error.into_iter().collect()
}

fn path_string(path: &Path) -> String {
    PathBuf::from(path).to_string_lossy().into_owned()
}

fn usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn error_summary(errors: &[String]) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    Some(bound_error(
        errors
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("; "),
    ))
}

fn bounded_required_id(
    field: &'static str,
    value: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    if value.is_empty() {
        errors.push(bound_error(format!("{field} is empty")));
        None
    } else {
        Some(bound_id(value.to_owned()))
    }
}

fn bound_id(value: String) -> String {
    bound_string_with_hash(value, MAX_ID_LENGTH)
}

fn bound_kind(value: String) -> String {
    bound_string(value, MAX_KIND_LENGTH)
}

fn bound_error(value: String) -> String {
    bound_string(value, MAX_ERROR_MESSAGE_LENGTH)
}

fn bound_path(value: String) -> String {
    bound_string(value, MAX_PATH_LENGTH)
}

fn bound_string(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        value
    } else {
        let mut end = max_bytes;
        while !value.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        value[..end].to_owned()
    }
}

fn bound_string_with_hash(value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let suffix = format!("~{:016x}", stable_hash(value.as_bytes()));
    let prefix_len = max_bytes.saturating_sub(suffix.len());
    if prefix_len == 0 {
        return bound_string(suffix, max_bytes);
    }
    let mut end = prefix_len.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{}", &value[..end], suffix)
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn lock_disk_samples(
    cache: &Mutex<HashMap<DiskCacheKey, CachedDiskSample>>,
) -> MutexGuard<'_, HashMap<DiskCacheKey, CachedDiskSample>> {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_resource_counters(
    counters: &Mutex<HashMap<ResourceScopeKey, PreviousResourceCounters>>,
) -> MutexGuard<'_, HashMap<ResourceScopeKey, PreviousResourceCounters>> {
    counters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn checked_delta(current: Option<i64>, previous: Option<i64>) -> Option<i64> {
    current
        .zip(previous)
        .and_then(|(current, previous)| current.checked_sub(previous))
}

fn checked_non_negative_delta(current: Option<i64>, previous: Option<i64>) -> Option<i64> {
    checked_delta(current, previous).filter(|delta| *delta >= 0)
}
