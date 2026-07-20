use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

const PROCESS_LIMIT: usize = 512;
const WARNING_LIMIT: usize = 16;
const STATUS_LIMIT: usize = 64 * 1024;
const STAT_LIMIT: usize = 4 * 1024;
const COMM_LIMIT: usize = 256;
const CGROUP_LIMIT: usize = 64 * 1024;
const IO_LIMIT: usize = 16 * 1024;
const SMAPS_ROLLUP_LIMIT: usize = 64 * 1024;
const FD_COUNT_LIMIT: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProcessInput {
    pub workspace_id: String,
    pub holder_pid: u32,
    pub cgroup_path: Option<String>,
    pub applied_cgroup_limits: Option<AppliedCgroupLimits>,
    pub workload_cgroup_state: String,
    pub workload_cgroup_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct AppliedCgroupLimits {
    pub nano_cpus: u64,
    pub memory_high_bytes: u64,
    pub memory_max_bytes: u64,
    pub pids_max: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WorkspaceProcessTopology {
    pub schema_version: u8,
    pub available: bool,
    pub source: Option<String>,
    pub error: Option<String>,
    pub truncated: bool,
    pub warnings: Vec<String>,
    pub workspaces: Vec<WorkspaceProcesses>,
    pub daemon: Option<DaemonProcessMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DaemonProcessMetrics {
    pub available: bool,
    pub error: Option<String>,
    pub sampled_at_unix_ms: u64,
    pub pid: u32,
    pub name: Option<String>,
    pub state: Option<String>,
    pub virtual_memory_bytes: Option<u64>,
    pub resident_memory_bytes: Option<u64>,
    pub peak_resident_memory_bytes: Option<u64>,
    pub proportional_set_size_bytes: Option<u64>,
    pub unique_set_size_bytes: Option<u64>,
    pub private_dirty_bytes: Option<u64>,
    pub anonymous_huge_pages_bytes: Option<u64>,
    pub anonymous_memory_bytes: Option<u64>,
    pub file_memory_bytes: Option<u64>,
    pub shared_memory_bytes: Option<u64>,
    pub data_memory_bytes: Option<u64>,
    pub swap_bytes: Option<u64>,
    pub cpu_time_us: Option<u64>,
    pub start_time_ticks: Option<u64>,
    pub thread_count: Option<u64>,
    pub file_descriptor_count: Option<u64>,
    pub io_read_bytes: Option<u64>,
    pub io_write_bytes: Option<u64>,
    pub read_syscalls: Option<u64>,
    pub write_syscalls: Option<u64>,
    pub voluntary_context_switches: Option<u64>,
    pub involuntary_context_switches: Option<u64>,
    pub cgroup_memberships: Vec<String>,
    pub cgroup_path: Option<String>,
    pub warnings: Vec<String>,
    pub runtime_config: DaemonRuntimeConfigMetrics,
    pub runtime_usage: DaemonRuntimeUsage,
    pub ownership: DaemonOwnershipMetrics,
    pub lifecycle: DaemonLifecycleMetrics,
    pub allocator: DaemonAllocatorMetrics,
    pub diagnostics: DaemonDiagnosticState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DaemonRuntimeConfigMetrics {
    pub worker_threads: Option<usize>,
    pub max_blocking_threads: Option<usize>,
    pub blocking_thread_keep_alive_s: Option<f64>,
    pub max_concurrent_connections: Option<usize>,
    pub max_active_commands: Option<usize>,
    pub max_blocking_queue_depth: Option<usize>,
    pub max_command_queue_depth: Option<usize>,
    pub infrastructure_thread_allowance: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DaemonRuntimeUsage {
    pub active_async_tasks: Option<usize>,
    pub active_blocking_tasks: Option<usize>,
    pub blocking_queue_depth: Option<usize>,
    pub blocking_admission_in_use: Option<usize>,
    pub connection_admission_in_use: Option<usize>,
    pub active_commands: Option<usize>,
    pub command_queue_depth: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DaemonOwnershipMetrics {
    pub open_workspaces: usize,
    pub live_holders: usize,
    pub exited_unreaped_holders: Option<usize>,
    pub namespace_fd_count: Option<usize>,
    pub control_fd_count: Option<usize>,
    pub namespace_control_fd_count: Option<usize>,
    pub active_scratch_directories: Option<usize>,
    pub persisted_workspace_handles: Option<usize>,
    pub active_layer_leases: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DaemonLifecycleMetrics {
    pub holder_exit_total: u64,
    pub cleanup_attempt_total: u64,
    pub cleanup_failure_total: u64,
    pub cleanup_terminal_total: u64,
    pub dropped_event_total: u64,
    pub retained_event_count: usize,
    pub last_holder_exit_reason: Option<String>,
    pub last_cleanup_failure: Option<String>,
    pub last_cleanup_result: Option<String>,
    pub last_cleanup_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DaemonAllocatorMetrics {
    /// True only when the selected allocator exposes all four process-wide
    /// counters below. Process RSS or anonymous memory is not an allocator
    /// residence substitute.
    pub supported: bool,
    pub allocated_bytes: Option<u64>,
    pub active_bytes: Option<u64>,
    pub mapped_bytes: Option<u64>,
    pub resident_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonDiagnosticTrigger {
    Cpu,
    AnonymousMemory,
    ExitedUnreapedHolder,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DaemonDiagnosticWindow {
    pub trigger: Option<DaemonDiagnosticTrigger>,
    pub started_at_unix_ms: Option<u64>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DaemonDiagnosticCooldown {
    pub active: bool,
    pub until_unix_ms: Option<u64>,
    pub remaining_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DaemonDiagnosticCpuInterval {
    pub elapsed_ms: u64,
    pub cpu_time_delta_us: Option<u64>,
    pub percent_of_one_core: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DaemonDiagnosticMemory {
    pub resident_memory_bytes: Option<u64>,
    pub proportional_set_size_bytes: Option<u64>,
    pub anonymous_memory_bytes: Option<u64>,
    pub private_dirty_bytes: Option<u64>,
    pub anonymous_huge_pages_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonDiagnosticRedaction {
    pub workspace_file_content_excluded: bool,
    pub environment_variables_excluded: bool,
    pub authentication_material_excluded: bool,
    pub full_command_lines_excluded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DaemonDiagnosticWorkspaceHolder {
    pub workspace_id: String,
    pub holder_pid: i32,
}

impl Default for DaemonDiagnosticRedaction {
    fn default() -> Self {
        Self {
            workspace_file_content_excluded: true,
            environment_variables_excluded: true,
            authentication_material_excluded: true,
            full_command_lines_excluded: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DaemonDiagnosticSummary {
    pub id: String,
    pub fingerprint: String,
    pub size_bytes: usize,
    pub captured_at_unix_ms: u64,
    pub trigger: DaemonDiagnosticTrigger,
    pub activity_classes: Vec<String>,
    pub cpu_interval: DaemonDiagnosticCpuInterval,
    pub memory: DaemonDiagnosticMemory,
    pub thread_count: Option<u64>,
    pub runtime_config: DaemonRuntimeConfigMetrics,
    pub runtime_usage: DaemonRuntimeUsage,
    pub ownership: DaemonOwnershipMetrics,
    pub workspace_ids: Vec<String>,
    pub workspace_holders: Vec<DaemonDiagnosticWorkspaceHolder>,
    pub workspace_ids_truncated: bool,
    pub omitted_workspace_id_count: usize,
    pub redaction: DaemonDiagnosticRedaction,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DaemonDiagnosticState {
    pub enabled: bool,
    pub max_artifact_bytes: usize,
    pub trigger_count: u64,
    pub active_window: DaemonDiagnosticWindow,
    pub cooldown: DaemonDiagnosticCooldown,
    pub latest: Option<DaemonDiagnosticSummary>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceProcesses {
    pub workspace_id: String,
    pub state: WorkspaceProcessState,
    pub holder_pid: u32,
    pub cgroup_path: Option<String>,
    pub applied_cgroup_limits: Option<AppliedCgroupLimits>,
    pub workload_cgroup_state: String,
    pub workload_cgroup_reason: Option<String>,
    pub pid_namespace: Option<String>,
    pub mount_namespace: Option<String>,
    pub processes: Vec<WorkspaceProcess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceProcessState {
    Active,
    Idle,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceProcess {
    pub pid: u32,
    pub namespace_pid: u32,
    pub parent_pid: u32,
    pub name: String,
    pub state: String,
    pub kind: WorkspaceProcessKind,
    pub cgroup_memberships: Vec<String>,
    pub resident_memory_bytes: Option<u64>,
    pub cpu_time_us: Option<u64>,
    pub start_time_ticks: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceProcessKind {
    NamespaceInit,
    Process,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NamespaceIdentity {
    pub device: u64,
    pub inode: u64,
}

pub trait NamespaceIdentityReader {
    fn identity(&self, path: &Path) -> io::Result<NamespaceIdentity>;

    fn diagnostic(&self, path: &Path) -> Option<String>;
}

/// Enumerates the numeric process directories visible below one procfs root.
///
/// The separate port keeps the collector's scan count observable in tests and
/// makes the empty namespace-index fast path explicit. Implementations return
/// a sorted, de-duplicated PID list.
pub trait ProcEntryReader {
    fn numeric_entries(&self, proc_root: &Path) -> io::Result<Vec<u32>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcNamespaceIdentityReader;

impl NamespaceIdentityReader for ProcNamespaceIdentityReader {
    fn identity(&self, path: &Path) -> io::Result<NamespaceIdentity> {
        namespace_identity(path)
    }

    fn diagnostic(&self, path: &Path) -> Option<String> {
        std::fs::read_link(path)
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcFsEntryReader;

impl ProcEntryReader for ProcFsEntryReader {
    fn numeric_entries(&self, proc_root: &Path) -> io::Result<Vec<u32>> {
        numeric_proc_entries(proc_root)
    }
}

impl WorkspaceProcessTopology {
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            schema_version: 2,
            available: false,
            error: Some(message.into()),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn collect(proc_root: &Path, workspaces: Vec<WorkspaceProcessInput>) -> Self {
        Self::collect_with_reader(proc_root, workspaces, &ProcNamespaceIdentityReader)
    }

    #[must_use]
    pub fn collect_with_reader(
        proc_root: &Path,
        inputs: Vec<WorkspaceProcessInput>,
        namespace_reader: &dyn NamespaceIdentityReader,
    ) -> Self {
        Self::collect_with_readers(proc_root, inputs, namespace_reader, &ProcFsEntryReader)
    }

    #[must_use]
    pub fn collect_with_readers(
        proc_root: &Path,
        mut inputs: Vec<WorkspaceProcessInput>,
        namespace_reader: &dyn NamespaceIdentityReader,
        proc_entry_reader: &dyn ProcEntryReader,
    ) -> Self {
        inputs.sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));
        let mut warnings = WarningBuffer::default();
        let clock_ticks_per_second = clock_ticks_per_second();
        if clock_ticks_per_second.is_none() {
            warnings
                .push("processor clock tick rate unavailable; CPU estimates omitted".to_owned());
        }
        inputs.dedup_by(|right, left| {
            let duplicate = left.workspace_id == right.workspace_id;
            if duplicate {
                warnings.push(format!(
                    "duplicate runtime workspace snapshot omitted: {}",
                    right.workspace_id
                ));
            }
            duplicate
        });

        let mut builders = inputs
            .into_iter()
            .map(|input| WorkspaceBuilder::new(input, proc_root, namespace_reader, &mut warnings))
            .collect::<Vec<_>>();
        let mut reverse = HashMap::new();
        let mut ambiguous = HashSet::new();
        for index in 0..builders.len() {
            let Some(identity) = builders[index].identity else {
                continue;
            };
            if ambiguous.contains(&identity) {
                builders[index].partial = true;
                continue;
            }
            if let Some(previous) = reverse.insert(identity, index) {
                reverse.remove(&identity);
                ambiguous.insert(identity);
                builders[previous].partial = true;
                builders[index].partial = true;
                warnings.push(format!(
                    "ambiguous namespace identity for workspaces {} and {}",
                    builders[previous].workspace.workspace_id,
                    builders[index].workspace.workspace_id
                ));
            }
        }

        if reverse.is_empty() {
            return finish_topology(builders, warnings, false);
        }

        let pids = match proc_entry_reader.numeric_entries(proc_root) {
            Ok(pids) => pids,
            Err(error) => {
                return Self::unavailable(format!(
                    "procfs enumeration failed at {}: {error}",
                    proc_root.display()
                ))
            }
        };

        let mut retained = 0;
        let mut truncated = false;
        for pid in pids {
            let pid_root = proc_root.join(pid.to_string());
            let identity = match process_namespace_identity(&pid_root, namespace_reader) {
                Ok(identity) => identity,
                Err(error) if is_proc_race(&error) => continue,
                Err(error) => {
                    warnings.push(format!("process {pid} namespace stat failed: {error}"));
                    continue;
                }
            };
            let Some(index) = reverse.get(&identity).copied() else {
                continue;
            };
            if retained == PROCESS_LIMIT {
                truncated = true;
                builders[index].partial = true;
                continue;
            }
            let process = match read_process(&pid_root, pid, clock_ticks_per_second, &mut warnings)
            {
                Ok(process) => process,
                Err(error) if is_proc_race(&error) => continue,
                Err(error) => {
                    builders[index].partial = true;
                    warnings.push(format!("process {pid} metadata read failed: {error}"));
                    continue;
                }
            };
            builders[index].workspace.processes.push(process);
            retained += 1;
        }
        if truncated {
            warnings.push(format!(
                "workspace process rows truncated at {PROCESS_LIMIT}"
            ));
        }
        finish_topology(builders, warnings, truncated)
    }
}

impl DaemonProcessMetrics {
    #[must_use]
    pub fn collect(proc_root: &Path, pid: u32) -> Self {
        let sampled_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let pid_root = proc_root.join(pid.to_string());
        let status = match read_bounded(&pid_root.join("status"), STATUS_LIMIT) {
            Ok(status) if !status.truncated => status,
            Ok(_) => {
                return Self::unavailable(
                    pid,
                    sampled_at_unix_ms,
                    "daemon status exceeded the read limit",
                )
            }
            Err(error) => {
                return Self::unavailable(
                    pid,
                    sampled_at_unix_ms,
                    format!("daemon status read failed: {error}"),
                )
            }
        };
        let mut warnings = WarningBuffer::default();
        let status = parse_daemon_status(&status.text);
        let mut usage = read_process_usage(&pid_root, pid, clock_ticks_per_second(), &mut warnings);
        if let Some(precise_cpu_time_us) = read_schedstat_cpu_time_us(&pid_root) {
            usage.cpu_time_us = Some(
                usage
                    .cpu_time_us
                    .map_or(precise_cpu_time_us, |coarse_cpu_time_us| {
                        coarse_cpu_time_us.max(precise_cpu_time_us)
                    }),
            );
        }
        let memory = read_daemon_smaps(&pid_root, &mut warnings);
        let io = read_daemon_io(&pid_root, &mut warnings);
        let file_descriptor_count = count_file_descriptors(&pid_root, &mut warnings);
        let cgroup = read_bounded(&pid_root.join("cgroup"), CGROUP_LIMIT).unwrap_or_default();
        if cgroup.truncated {
            warnings.push("daemon cgroup membership truncated".to_owned());
        }
        let cgroup_memberships = complete_nonempty_lines(&cgroup.text, cgroup.truncated);
        Self {
            available: true,
            error: None,
            sampled_at_unix_ms,
            pid,
            name: status.name,
            state: status.state,
            virtual_memory_bytes: status.virtual_memory_bytes,
            resident_memory_bytes: status.resident_memory_bytes,
            peak_resident_memory_bytes: status.peak_resident_memory_bytes,
            proportional_set_size_bytes: memory.proportional_set_size_bytes,
            unique_set_size_bytes: memory.unique_set_size_bytes,
            private_dirty_bytes: memory.private_dirty_bytes,
            anonymous_huge_pages_bytes: memory.anonymous_huge_pages_bytes,
            anonymous_memory_bytes: status.anonymous_memory_bytes,
            file_memory_bytes: status.file_memory_bytes,
            shared_memory_bytes: status.shared_memory_bytes,
            data_memory_bytes: status.data_memory_bytes,
            swap_bytes: status.swap_bytes,
            cpu_time_us: usage.cpu_time_us,
            start_time_ticks: usage.start_time_ticks,
            thread_count: status.thread_count,
            file_descriptor_count,
            io_read_bytes: io.read_bytes,
            io_write_bytes: io.write_bytes,
            read_syscalls: io.read_syscalls,
            write_syscalls: io.write_syscalls,
            voluntary_context_switches: status.voluntary_context_switches,
            involuntary_context_switches: status.involuntary_context_switches,
            cgroup_path: unified_cgroup_path(&cgroup_memberships),
            cgroup_memberships,
            warnings: warnings.finish(),
            runtime_config: DaemonRuntimeConfigMetrics::default(),
            runtime_usage: DaemonRuntimeUsage::default(),
            ownership: DaemonOwnershipMetrics::default(),
            lifecycle: DaemonLifecycleMetrics::default(),
            allocator: DaemonAllocatorMetrics::default(),
            diagnostics: DaemonDiagnosticState::default(),
        }
    }

    fn unavailable(pid: u32, sampled_at_unix_ms: u64, message: impl Into<String>) -> Self {
        Self {
            available: false,
            error: Some(message.into()),
            sampled_at_unix_ms,
            pid,
            name: None,
            state: None,
            virtual_memory_bytes: None,
            resident_memory_bytes: None,
            peak_resident_memory_bytes: None,
            proportional_set_size_bytes: None,
            unique_set_size_bytes: None,
            private_dirty_bytes: None,
            anonymous_huge_pages_bytes: None,
            anonymous_memory_bytes: None,
            file_memory_bytes: None,
            shared_memory_bytes: None,
            data_memory_bytes: None,
            swap_bytes: None,
            cpu_time_us: None,
            start_time_ticks: None,
            thread_count: None,
            file_descriptor_count: None,
            io_read_bytes: None,
            io_write_bytes: None,
            read_syscalls: None,
            write_syscalls: None,
            voluntary_context_switches: None,
            involuntary_context_switches: None,
            cgroup_memberships: Vec::new(),
            cgroup_path: None,
            warnings: Vec::new(),
            runtime_config: DaemonRuntimeConfigMetrics::default(),
            runtime_usage: DaemonRuntimeUsage::default(),
            ownership: DaemonOwnershipMetrics::default(),
            lifecycle: DaemonLifecycleMetrics::default(),
            allocator: DaemonAllocatorMetrics::default(),
            diagnostics: DaemonDiagnosticState::default(),
        }
    }
}

struct WorkspaceBuilder {
    workspace: WorkspaceProcesses,
    identity: Option<(NamespaceIdentity, NamespaceIdentity)>,
    partial: bool,
}

fn finish_topology(
    mut builders: Vec<WorkspaceBuilder>,
    warnings: WarningBuffer,
    truncated: bool,
) -> WorkspaceProcessTopology {
    for builder in &mut builders {
        builder
            .workspace
            .processes
            .sort_by_key(|process| process.pid);
        builder.workspace.state = if builder.partial {
            WorkspaceProcessState::Partial
        } else if builder
            .workspace
            .processes
            .iter()
            .any(|process| process.namespace_pid != 1)
        {
            WorkspaceProcessState::Active
        } else {
            WorkspaceProcessState::Idle
        };
    }

    WorkspaceProcessTopology {
        schema_version: 2,
        available: true,
        source: Some("proc_namespaces".to_owned()),
        error: None,
        truncated,
        warnings: warnings.finish(),
        workspaces: builders
            .into_iter()
            .map(|builder| builder.workspace)
            .collect(),
        daemon: None,
    }
}

impl WorkspaceBuilder {
    fn new(
        input: WorkspaceProcessInput,
        proc_root: &Path,
        namespace_reader: &dyn NamespaceIdentityReader,
        warnings: &mut WarningBuffer,
    ) -> Self {
        let holder_root = proc_root.join(input.holder_pid.to_string());
        let pid_path = holder_root.join("ns/pid_for_children");
        let mount_path = holder_root.join("ns/mnt");
        let pid_namespace = namespace_reader.diagnostic(&pid_path);
        let mount_namespace = namespace_reader.diagnostic(&mount_path);
        let identity = namespace_reader.identity(&pid_path).and_then(|pid| {
            namespace_reader
                .identity(&mount_path)
                .map(|mount| (pid, mount))
        });
        let (identity, partial) = match identity {
            Ok(identity) => (Some(identity), false),
            Err(error) => {
                warnings.push(format!(
                    "workspace {} holder {} namespace stat failed: {error}",
                    input.workspace_id, input.holder_pid
                ));
                (None, true)
            }
        };
        Self {
            workspace: WorkspaceProcesses {
                workspace_id: input.workspace_id,
                state: WorkspaceProcessState::Idle,
                holder_pid: input.holder_pid,
                cgroup_path: input.cgroup_path,
                applied_cgroup_limits: input.applied_cgroup_limits,
                workload_cgroup_state: input.workload_cgroup_state,
                workload_cgroup_reason: input.workload_cgroup_reason,
                pid_namespace,
                mount_namespace,
                processes: Vec::new(),
            },
            identity,
            partial,
        }
    }
}

fn unified_cgroup_path(memberships: &[String]) -> Option<String> {
    memberships.iter().find_map(|membership| {
        let mut parts = membership.splitn(3, ':');
        let hierarchy = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        (hierarchy == "0" && controllers.is_empty() && path.starts_with('/'))
            .then(|| path.to_owned())
    })
}

fn numeric_proc_entries(proc_root: &Path) -> io::Result<Vec<u32>> {
    let entries = std::fs::read_dir(proc_root)?;
    let mut pids = Vec::new();
    for entry in entries {
        let entry = entry?;
        if let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

fn process_namespace_identity(
    pid_root: &Path,
    reader: &dyn NamespaceIdentityReader,
) -> io::Result<(NamespaceIdentity, NamespaceIdentity)> {
    let pid = reader.identity(&pid_root.join("ns/pid"))?;
    let mount = reader.identity(&pid_root.join("ns/mnt"))?;
    Ok((pid, mount))
}

fn read_process(
    pid_root: &Path,
    pid: u32,
    clock_ticks_per_second: Option<u64>,
    warnings: &mut WarningBuffer,
) -> io::Result<WorkspaceProcess> {
    let status = read_bounded(&pid_root.join("status"), STATUS_LIMIT)?;
    if status.truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "status exceeded the read limit",
        ));
    }
    let metadata = parse_status(&status.text)?;
    let comm = read_bounded(&pid_root.join("comm"), COMM_LIMIT)?;
    if comm.truncated {
        warnings.push(format!("process {pid} comm truncated"));
    }
    let name = comm
        .text
        .trim_end_matches(['\r', '\n'])
        .chars()
        .take(COMM_LIMIT)
        .collect::<String>();
    let cgroup = read_bounded(&pid_root.join("cgroup"), CGROUP_LIMIT).unwrap_or_default();
    if cgroup.truncated {
        warnings.push(format!("process {pid} cgroup membership truncated"));
    }
    let cgroup_memberships = complete_nonempty_lines(&cgroup.text, cgroup.truncated);
    let usage = read_process_usage(pid_root, pid, clock_ticks_per_second, warnings);
    Ok(WorkspaceProcess {
        pid,
        namespace_pid: metadata.namespace_pid,
        parent_pid: metadata.parent_pid,
        name: if name.is_empty() {
            "unknown".to_owned()
        } else {
            name
        },
        state: metadata.state.to_string(),
        kind: if metadata.namespace_pid == 1 {
            WorkspaceProcessKind::NamespaceInit
        } else {
            WorkspaceProcessKind::Process
        },
        cgroup_memberships,
        resident_memory_bytes: metadata.resident_memory_bytes,
        cpu_time_us: usage.cpu_time_us,
        start_time_ticks: usage.start_time_ticks,
    })
}

#[derive(Default)]
struct ProcessUsage {
    cpu_time_us: Option<u64>,
    start_time_ticks: Option<u64>,
}

fn read_process_usage(
    pid_root: &Path,
    pid: u32,
    clock_ticks_per_second: Option<u64>,
    warnings: &mut WarningBuffer,
) -> ProcessUsage {
    let Some(clock_ticks_per_second) = clock_ticks_per_second else {
        return ProcessUsage::default();
    };
    let stat = match read_bounded(&pid_root.join("stat"), STAT_LIMIT) {
        Ok(stat) if !stat.truncated => stat,
        Ok(_) => {
            warnings.push(format!("process {pid} stat exceeded the read limit"));
            return ProcessUsage::default();
        }
        Err(error) if is_proc_race(&error) => return ProcessUsage::default(),
        Err(error) => {
            warnings.push(format!("process {pid} stat read failed: {error}"));
            return ProcessUsage::default();
        }
    };
    match parse_proc_stat(&stat.text) {
        Ok((cpu_time_ticks, start_time_ticks)) => ProcessUsage {
            cpu_time_us: Some(ticks_to_microseconds(
                cpu_time_ticks,
                clock_ticks_per_second,
            )),
            start_time_ticks: Some(start_time_ticks),
        },
        Err(error) => {
            warnings.push(format!("process {pid} stat parse failed: {error}"));
            ProcessUsage::default()
        }
    }
}

fn read_schedstat_cpu_time_us(pid_root: &Path) -> Option<u64> {
    let schedstat = read_bounded(&pid_root.join("schedstat"), STAT_LIMIT).ok()?;
    if schedstat.truncated {
        return None;
    }
    let runtime_ns = schedstat
        .text
        .split_whitespace()
        .next()?
        .parse::<u128>()
        .ok()?;
    u64::try_from(runtime_ns / 1_000).ok()
}

fn parse_proc_stat(stat: &str) -> io::Result<(u64, u64)> {
    let (_, fields) = stat.rsplit_once(')').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "stat command delimiter is missing",
        )
    })?;
    let mut fields = fields.split_whitespace();
    let parse = |value: Option<&str>, name: &str| {
        value
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("stat {name} is missing"),
                )
            })
    };
    let user_ticks = parse(fields.nth(11), "utime")?;
    let system_ticks = parse(fields.next(), "stime")?;
    let start_time_ticks = parse(fields.nth(6), "starttime")?;
    let cpu_time_ticks = user_ticks
        .checked_add(system_ticks)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "stat CPU time overflowed"))?;
    Ok((cpu_time_ticks, start_time_ticks))
}

fn ticks_to_microseconds(ticks: u64, clock_ticks_per_second: u64) -> u64 {
    let micros = u128::from(ticks)
        .saturating_mul(1_000_000)
        .checked_div(u128::from(clock_ticks_per_second))
        .unwrap_or_default();
    u64::try_from(micros).unwrap_or(u64::MAX)
}

#[cfg(unix)]
fn clock_ticks_per_second() -> Option<u64> {
    // SAFETY: `sysconf` reads the fixed process clock-tick configuration and does not dereference pointers.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).ok().filter(|ticks| *ticks > 0)
}

#[cfg(not(unix))]
fn clock_ticks_per_second() -> Option<u64> {
    None
}

#[derive(Default)]
struct BoundedText {
    text: String,
    truncated: bool,
}

fn read_bounded(path: &Path, limit: usize) -> io::Result<BoundedText> {
    let mut bytes = Vec::with_capacity(limit.min(4096) + 1);
    File::open(path)?
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok(BoundedText {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    })
}

fn complete_nonempty_lines(text: &str, truncated: bool) -> Vec<String> {
    let complete = if truncated {
        text.rsplit_once('\n').map_or("", |(complete, _)| complete)
    } else {
        text
    };
    complete
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

struct ProcessStatus {
    namespace_pid: u32,
    parent_pid: u32,
    state: char,
    resident_memory_bytes: Option<u64>,
}

#[derive(Default)]
struct DaemonStatus {
    name: Option<String>,
    state: Option<String>,
    virtual_memory_bytes: Option<u64>,
    resident_memory_bytes: Option<u64>,
    peak_resident_memory_bytes: Option<u64>,
    anonymous_memory_bytes: Option<u64>,
    file_memory_bytes: Option<u64>,
    shared_memory_bytes: Option<u64>,
    data_memory_bytes: Option<u64>,
    swap_bytes: Option<u64>,
    thread_count: Option<u64>,
    voluntary_context_switches: Option<u64>,
    involuntary_context_switches: Option<u64>,
}

fn parse_daemon_status(status: &str) -> DaemonStatus {
    let mut parsed = DaemonStatus::default();
    for line in status.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "Name" => parsed.name = nonempty(value),
            "State" => parsed.state = nonempty(value),
            "VmSize" => parsed.virtual_memory_bytes = parse_kibibytes(value),
            "VmRSS" => parsed.resident_memory_bytes = parse_kibibytes(value),
            "VmHWM" => parsed.peak_resident_memory_bytes = parse_kibibytes(value),
            "RssAnon" => parsed.anonymous_memory_bytes = parse_kibibytes(value),
            "RssFile" => parsed.file_memory_bytes = parse_kibibytes(value),
            "RssShmem" => parsed.shared_memory_bytes = parse_kibibytes(value),
            "VmData" => parsed.data_memory_bytes = parse_kibibytes(value),
            "VmSwap" => parsed.swap_bytes = parse_kibibytes(value),
            "Threads" => parsed.thread_count = parse_u64(value),
            "voluntary_ctxt_switches" => {
                parsed.voluntary_context_switches = parse_u64(value);
            }
            "nonvoluntary_ctxt_switches" => {
                parsed.involuntary_context_switches = parse_u64(value);
            }
            _ => {}
        }
    }
    parsed
}

#[derive(Default)]
struct DaemonSmaps {
    proportional_set_size_bytes: Option<u64>,
    unique_set_size_bytes: Option<u64>,
    private_dirty_bytes: Option<u64>,
    anonymous_huge_pages_bytes: Option<u64>,
}

fn read_daemon_smaps(pid_root: &Path, warnings: &mut WarningBuffer) -> DaemonSmaps {
    let smaps = match read_bounded(&pid_root.join("smaps_rollup"), SMAPS_ROLLUP_LIMIT) {
        Ok(smaps) if !smaps.truncated => smaps,
        Ok(_) => {
            warnings.push("daemon smaps_rollup exceeded the read limit".to_owned());
            return DaemonSmaps::default();
        }
        Err(error) => {
            warnings.push(format!("daemon smaps_rollup read failed: {error}"));
            return DaemonSmaps::default();
        }
    };
    let mut proportional_set_size_bytes = None;
    let mut unique_set_size_bytes = Some(0_u64);
    let mut private_dirty_bytes = None;
    let mut anonymous_huge_pages_bytes = None;
    for line in smaps.text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "Pss" => proportional_set_size_bytes = parse_kibibytes(value),
            "Private_Clean" | "Private_Dirty" | "Private_Hugetlb" => {
                if key == "Private_Dirty" {
                    private_dirty_bytes = parse_kibibytes(value);
                }
                unique_set_size_bytes = unique_set_size_bytes
                    .zip(parse_kibibytes(value))
                    .and_then(|(total, bytes)| total.checked_add(bytes));
            }
            "AnonHugePages" => anonymous_huge_pages_bytes = parse_kibibytes(value),
            _ => {}
        }
    }
    DaemonSmaps {
        proportional_set_size_bytes,
        unique_set_size_bytes,
        private_dirty_bytes,
        anonymous_huge_pages_bytes,
    }
}

#[derive(Default)]
struct DaemonIo {
    read_bytes: Option<u64>,
    write_bytes: Option<u64>,
    read_syscalls: Option<u64>,
    write_syscalls: Option<u64>,
}

fn read_daemon_io(pid_root: &Path, warnings: &mut WarningBuffer) -> DaemonIo {
    let input = match read_bounded(&pid_root.join("io"), IO_LIMIT) {
        Ok(input) if !input.truncated => input,
        Ok(_) => {
            warnings.push("daemon io exceeded the read limit".to_owned());
            return DaemonIo::default();
        }
        Err(error) => {
            warnings.push(format!("daemon io read failed: {error}"));
            return DaemonIo::default();
        }
    };
    let mut parsed = DaemonIo::default();
    for line in input.text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "read_bytes" => parsed.read_bytes = parse_u64(value),
            "write_bytes" => parsed.write_bytes = parse_u64(value),
            "syscr" => parsed.read_syscalls = parse_u64(value),
            "syscw" => parsed.write_syscalls = parse_u64(value),
            _ => {}
        }
    }
    parsed
}

fn count_file_descriptors(pid_root: &Path, warnings: &mut WarningBuffer) -> Option<u64> {
    let entries = match std::fs::read_dir(pid_root.join("fd")) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("daemon file descriptor read failed: {error}"));
            return None;
        }
    };
    let mut count = 0_u64;
    for entry in entries {
        if entry.is_err() {
            warnings.push("daemon file descriptor entry disappeared".to_owned());
            continue;
        }
        if count == FD_COUNT_LIMIT as u64 {
            warnings.push(format!(
                "daemon file descriptor count truncated at {FD_COUNT_LIMIT}"
            ));
            return None;
        }
        count += 1;
    }
    Some(count)
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn parse_u64(value: &str) -> Option<u64> {
    value.trim().parse().ok()
}

fn parse_status(status: &str) -> io::Result<ProcessStatus> {
    let mut namespace_pid = None;
    let mut parent_pid = None;
    let mut state = None;
    let mut resident_memory_bytes = None;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("NSpid:") {
            namespace_pid = value
                .split_whitespace()
                .filter_map(|value| value.parse::<u32>().ok())
                .next_back();
        } else if let Some(value) = line.strip_prefix("PPid:") {
            parent_pid = value.trim().parse::<u32>().ok();
        } else if let Some(value) = line.strip_prefix("State:") {
            state = value.trim().chars().next();
        } else if let Some(value) = line.strip_prefix("VmRSS:") {
            resident_memory_bytes = parse_kibibytes(value);
        }
    }
    match (namespace_pid, parent_pid, state) {
        (Some(namespace_pid), Some(parent_pid), Some(state)) => Ok(ProcessStatus {
            namespace_pid,
            parent_pid,
            state,
            resident_memory_bytes,
        }),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "required status fields are missing",
        )),
    }
}

fn parse_kibibytes(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let kibibytes = fields.next()?.parse::<u64>().ok()?;
    (fields.next()? == "kB")
        .then(|| kibibytes.checked_mul(1024))
        .flatten()
}

#[cfg(unix)]
fn namespace_identity(path: &Path) -> io::Result<NamespaceIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path)?;
    Ok(NamespaceIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn namespace_identity(_path: &Path) -> io::Result<NamespaceIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "proc namespace identity requires Unix metadata",
    ))
}

fn is_proc_race(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(3)
}

#[derive(Default)]
struct WarningBuffer {
    warnings: Vec<String>,
    omitted: usize,
}

impl WarningBuffer {
    fn push(&mut self, warning: String) {
        if self.warnings.len() < WARNING_LIMIT {
            self.warnings.push(warning);
        } else {
            self.omitted += 1;
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.omitted > 0 {
            let summary = format!("{} additional collection warnings omitted", self.omitted);
            if self.warnings.len() == WARNING_LIMIT {
                self.warnings[WARNING_LIMIT - 1] = summary;
            } else {
                self.warnings.push(summary);
            }
        }
        self.warnings
    }
}
