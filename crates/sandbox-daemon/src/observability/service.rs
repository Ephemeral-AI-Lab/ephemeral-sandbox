use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sandbox_observability::{
    ExecutionSnapshotRecord, ObservabilityPaths, ObservabilityStore, ResourceSampleRecord,
    SandboxSnapshotRecord, StoreError, WorkspaceSnapshotRecord, MAX_COMMAND_LENGTH,
    MAX_ERROR_MESSAGE_LENGTH, MAX_ID_LENGTH, MAX_KIND_LENGTH, MAX_OPERATION_LENGTH,
    MAX_PATH_LENGTH, MAX_SNAPSHOT_STATE_LENGTH,
};
use sandbox_runtime::{
    RuntimeExecutionSnapshot, RuntimeObservabilitySnapshot, RuntimeWorkspaceSnapshot,
    SandboxRuntimeOperations,
};

use crate::server::ServerConfig;

use super::cgroup::CgroupSample;
use super::disk::{self, DiskSample};

const DISK_SAMPLE_MIN_INTERVAL: Duration = Duration::from_secs(10);

pub(crate) struct DaemonObservability {
    sandbox_id: String,
    paths: ObservabilityPaths,
    store: ObservabilityStore,
    next_sample_id: AtomicU64,
    disk_samples: Mutex<HashMap<DiskCacheKey, CachedDiskSample>>,
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

    #[cfg(test)]
    #[allow(dead_code, reason = "used by path-included daemon integration tests")]
    pub(crate) fn collect_runtime_snapshot_for_test(
        &self,
        config: &ServerConfig,
        snapshot: RuntimeObservabilitySnapshot,
    ) -> Result<(), StoreError> {
        self.write_snapshot(config, snapshot, unix_ms(), false)
    }

    fn write_snapshot(
        &self,
        config: &ServerConfig,
        snapshot: RuntimeObservabilitySnapshot,
        sampled_at_unix_ms: i64,
        force_fresh_disk: bool,
    ) -> Result<(), StoreError> {
        let RuntimeObservabilitySnapshot {
            workspaces,
            active_executions,
            mut partial_errors,
        } = snapshot;

        let mut workspace_records = Vec::new();
        let mut resource_samples = vec![self.resource_record(
            None,
            sampled_at_unix_ms,
            CgroupSample::unavailable("cgroup path unavailable"),
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
                sampled_at_unix_ms,
                force_fresh_disk,
            ));
        }

        let mut execution_records = Vec::new();
        for execution in &active_executions {
            let Some(execution_id) =
                bounded_required_id("execution_id", &execution.execution_id, &mut partial_errors)
            else {
                continue;
            };
            let Some(workspace_id) = bounded_required_id(
                "execution_workspace_id",
                &execution.workspace_id.0,
                &mut partial_errors,
            ) else {
                continue;
            };
            execution_records.push(self.execution_record(
                execution,
                execution_id,
                workspace_id,
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

        let active_execution_ids = execution_records
            .iter()
            .map(|execution| execution.execution_id.clone())
            .collect::<Vec<_>>();
        self.store
            .upsert_execution_snapshots(&self.sandbox_id, &execution_records)?;
        self.store
            .prune_execution_snapshots(&self.sandbox_id, &active_execution_ids)?;

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
            state: if partial_errors.is_empty() {
                "ready".to_owned()
            } else {
                "unavailable".to_owned()
            },
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
            remount_state: Some(bound_state(workspace.remount_state.clone())),
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

    fn execution_record(
        &self,
        execution: &RuntimeExecutionSnapshot,
        execution_id: String,
        workspace_id: String,
        sampled_at_unix_ms: i64,
    ) -> ExecutionSnapshotRecord {
        ExecutionSnapshotRecord {
            sandbox_id: self.sandbox_id.clone(),
            workspace_id,
            execution_id,
            execution_kind: bound_required_text(
                &execution.execution_kind,
                MAX_KIND_LENGTH,
                "unknown",
            ),
            operation: execution.operation.clone().map(bound_operation),
            command_session_id: execution
                .command_session_id
                .as_ref()
                .map(|command_session_id| bound_id(command_session_id.0.clone())),
            command: execution.command.clone().map(bound_command),
            lifecycle_state: bound_required_text(
                &execution.lifecycle_state,
                MAX_SNAPSHOT_STATE_LENGTH,
                "unknown",
            ),
            finalization_state: bound_required_text(
                &execution.finalization_state,
                MAX_SNAPSHOT_STATE_LENGTH,
                "unknown",
            ),
            workspace_ownership: Some(bound_kind(execution.workspace_ownership.clone())),
            started_at_unix_ms: execution.started_at_unix_ms,
            wall_time_ms: execution.wall_time_ms,
            process_group_id: execution.process_group_id.map(i64::from),
            transcript_path: execution
                .transcript_path
                .as_deref()
                .map(path_string)
                .map(bound_path),
            sampled_at_unix_ms,
            error_message: None,
        }
    }

    fn workspace_resource_record(
        &self,
        workspace_id: &str,
        upperdir: Option<&Path>,
        sampled_at_unix_ms: i64,
        force_fresh_disk: bool,
    ) -> ResourceSampleRecord {
        let disk = upperdir
            .map(|upperdir| self.disk_sample(workspace_id, upperdir, force_fresh_disk))
            .unwrap_or_else(DiskSample::empty);
        self.resource_record(
            Some(workspace_id),
            sampled_at_unix_ms,
            CgroupSample::unavailable("cgroup path unavailable"),
            disk,
        )
    }

    fn resource_record(
        &self,
        workspace_id: Option<&str>,
        sampled_at_unix_ms: i64,
        cgroup: CgroupSample,
        disk: DiskSample,
    ) -> ResourceSampleRecord {
        ResourceSampleRecord {
            sample_id: self.next_sample_id(sampled_at_unix_ms),
            sandbox_id: self.sandbox_id.clone(),
            workspace_id: workspace_id.map(str::to_owned),
            sampled_at_unix_ms,
            cgroup_path: cgroup.cgroup_path.map(bound_path),
            cgroup_available: cgroup.cgroup_available,
            cgroup_error: cgroup.cgroup_error.map(bound_error),
            cpu_usage_usec: cgroup.cpu_usage_usec,
            memory_current_bytes: cgroup.memory_current_bytes,
            memory_max_bytes: cgroup.memory_max_bytes,
            memory_max_unlimited: cgroup.memory_max_unlimited,
            disk_upperdir_bytes: disk.upperdir_bytes,
            disk_file_count: disk.file_count,
            disk_dir_count: disk.dir_count,
            disk_symlink_count: disk.symlink_count,
            disk_truncated: disk.truncated,
            disk_read_error_count: disk.read_error_count,
            disk_first_error_path: disk.first_error_path.map(bound_path),
        }
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

fn bound_required_text(value: &str, max_bytes: usize, fallback: &'static str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        bound_string(value.to_owned(), max_bytes)
    }
}

fn bound_id(value: String) -> String {
    bound_string(value, MAX_ID_LENGTH)
}

fn bound_kind(value: String) -> String {
    bound_string(value, MAX_KIND_LENGTH)
}

fn bound_operation(value: String) -> String {
    bound_string(value, MAX_OPERATION_LENGTH)
}

fn bound_state(value: String) -> String {
    bound_string(value, MAX_SNAPSHOT_STATE_LENGTH)
}

fn bound_command(value: String) -> String {
    bound_string(value, MAX_COMMAND_LENGTH)
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

fn lock_disk_samples(
    cache: &Mutex<HashMap<DiskCacheKey, CachedDiskSample>>,
) -> MutexGuard<'_, HashMap<DiskCacheKey, CachedDiskSample>> {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
