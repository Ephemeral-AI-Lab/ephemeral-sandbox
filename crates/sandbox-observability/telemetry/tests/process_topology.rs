use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_observability_telemetry::collect::process_topology::{
    DaemonProcessMetrics, NamespaceIdentity, NamespaceIdentityReader, ProcEntryReader,
    WorkspaceProcessInput, WorkspaceProcessKind, WorkspaceProcessState, WorkspaceProcessTopology,
};

#[derive(Default)]
struct FakeNamespaceReader {
    identities: HashMap<PathBuf, NamespaceIdentity>,
    diagnostics: HashMap<PathBuf, String>,
    errors: HashMap<PathBuf, io::ErrorKind>,
}

impl FakeNamespaceReader {
    fn holder(&mut self, root: &Path, pid: u32, pid_ns: u64, mount_ns: u64) {
        self.namespace(
            root.join(pid.to_string()).join("ns/pid_for_children"),
            1,
            pid_ns,
            format!("pid:[{pid_ns}]"),
        );
        self.namespace(
            root.join(pid.to_string()).join("ns/mnt"),
            2,
            mount_ns,
            format!("mnt:[{mount_ns}]"),
        );
    }

    fn process(&mut self, root: &Path, pid: u32, pid_ns: u64, mount_ns: u64) {
        self.namespace(root.join(pid.to_string()).join("ns/pid"), 1, pid_ns, "");
        self.namespace(root.join(pid.to_string()).join("ns/mnt"), 2, mount_ns, "");
    }

    fn namespace(&mut self, path: PathBuf, device: u64, inode: u64, diagnostic: impl Into<String>) {
        self.identities
            .insert(path.clone(), NamespaceIdentity { device, inode });
        let diagnostic = diagnostic.into();
        if !diagnostic.is_empty() {
            self.diagnostics.insert(path, diagnostic);
        }
    }
}

impl NamespaceIdentityReader for FakeNamespaceReader {
    fn identity(&self, path: &Path) -> io::Result<NamespaceIdentity> {
        if let Some(kind) = self.errors.get(path) {
            return Err(io::Error::new(*kind, "injected namespace stat failure"));
        }
        self.identities
            .get(path)
            .copied()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "namespace disappeared"))
    }

    fn diagnostic(&self, path: &Path) -> Option<String> {
        self.diagnostics.get(path).cloned()
    }
}

struct CountingProcEntryReader {
    calls: AtomicU64,
    pids: Vec<u32>,
}

impl CountingProcEntryReader {
    fn new(pids: Vec<u32>) -> Self {
        Self {
            calls: AtomicU64::new(0),
            pids,
        }
    }
}

impl ProcEntryReader for CountingProcEntryReader {
    fn numeric_entries(&self, _proc_root: &Path) -> io::Result<Vec<u32>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.pids.clone())
    }
}

#[test]
fn empty_or_all_invalid_holder_indices_skip_numeric_proc_enumeration() {
    let root = fixture("empty-index-fast-path");
    let empty_entries = CountingProcEntryReader::new(vec![1, 2, 3]);

    let empty = WorkspaceProcessTopology::collect_with_readers(
        &root,
        Vec::new(),
        &FakeNamespaceReader::default(),
        &empty_entries,
    );

    assert!(empty.available);
    assert!(empty.workspaces.is_empty());
    assert_eq!(empty_entries.calls.load(Ordering::SeqCst), 0);

    let invalid_entries = CountingProcEntryReader::new(vec![1, 2, 3]);
    let invalid = WorkspaceProcessTopology::collect_with_readers(
        &root,
        (0..20)
            .map(|index| workspace(&format!("workspace-{index:02}"), 100 + index))
            .collect(),
        &FakeNamespaceReader::default(),
        &invalid_entries,
    );

    assert!(invalid.available);
    assert_eq!(invalid.workspaces.len(), 20);
    assert!(invalid
        .workspaces
        .iter()
        .all(|workspace| workspace.state == WorkspaceProcessState::Partial));
    assert!(invalid
        .workspaces
        .iter()
        .all(|workspace| workspace.processes.is_empty()));
    assert!(invalid.warnings.len() <= 16);
    assert_eq!(invalid_entries.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn valid_and_mixed_holder_indices_enumerate_numeric_proc_entries_once() {
    let root = fixture("single-proc-enumeration");
    let mut namespaces = FakeNamespaceReader::default();
    namespaces.holder(&root, 10, 101, 201);
    namespaces.process(&root, 11, 101, 201);
    write_process(&root, 11, 1, 10, "ns-init", 'S', Some("0::/\n"));

    let valid_entries = CountingProcEntryReader::new(vec![11]);
    let valid = WorkspaceProcessTopology::collect_with_readers(
        &root,
        vec![workspace("workspace-valid", 10)],
        &namespaces,
        &valid_entries,
    );

    assert!(valid.available);
    assert_eq!(valid.workspaces[0].processes.len(), 1);
    assert_eq!(valid_entries.calls.load(Ordering::SeqCst), 1);

    let mixed_entries = CountingProcEntryReader::new(vec![11]);
    let mixed = WorkspaceProcessTopology::collect_with_readers(
        &root,
        vec![
            workspace("workspace-invalid", 99),
            workspace("workspace-valid", 10),
        ],
        &namespaces,
        &mixed_entries,
    );

    assert!(mixed.available);
    assert_eq!(mixed.workspaces.len(), 2);
    assert_eq!(mixed.workspaces[0].state, WorkspaceProcessState::Partial);
    assert_eq!(mixed.workspaces[1].state, WorkspaceProcessState::Idle);
    assert_eq!(mixed.workspaces[1].processes.len(), 1);
    assert_eq!(mixed_entries.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn zero_workspaces_is_available() {
    let root = fixture("zero");

    let topology = WorkspaceProcessTopology::collect_with_reader(
        &root,
        Vec::new(),
        &FakeNamespaceReader::default(),
    );

    assert_eq!(topology.schema_version, 2);
    assert!(topology.available);
    assert_eq!(topology.source.as_deref(), Some("proc_namespaces"));
    assert!(topology.workspaces.is_empty());
}

#[test]
fn idle_workspace_includes_only_namespace_init() {
    let root = fixture("idle");
    let mut namespaces = FakeNamespaceReader::default();
    namespaces.holder(&root, 10, 101, 201);
    namespaces.process(&root, 11, 101, 201);
    write_process(&root, 11, 1, 10, "ns-init", 'S', Some("0::/\n"));

    let topology = collect(&root, vec![workspace("workspace-b", 10)], &namespaces);

    let workspace = &topology.workspaces[0];
    assert_eq!(workspace.state, WorkspaceProcessState::Idle);
    assert_eq!(workspace.pid_namespace.as_deref(), Some("pid:[101]"));
    assert_eq!(workspace.mount_namespace.as_deref(), Some("mnt:[201]"));
    assert_eq!(workspace.processes.len(), 1);
    assert_eq!(
        workspace.processes[0].kind,
        WorkspaceProcessKind::NamespaceInit
    );
    assert_eq!(workspace.processes[0].namespace_pid, 1);
}

#[test]
fn two_workspaces_require_both_namespace_identities_and_keep_v1_v2_memberships() {
    let root = fixture("two-workspaces");
    let mut namespaces = FakeNamespaceReader::default();
    namespaces.holder(&root, 20, 102, 202);
    namespaces.holder(&root, 30, 103, 203);
    namespaces.process(&root, 201, 102, 202);
    namespaces.process(&root, 301, 103, 203);
    namespaces.process(&root, 999, 102, 203);
    write_process(
        &root,
        201,
        2,
        101,
        "worker-a",
        'S',
        Some("12:cpu:/shared\n11:memory:/shared\n"),
    );
    write_process(&root, 301, 2, 101, "worker-b", 'R', Some("0::/\n"));
    write_process(&root, 999, 4, 1, "mount-mismatch", 'S', Some("0::/\n"));

    let topology = collect(
        &root,
        vec![workspace("workspace-b", 30), workspace("workspace-a", 20)],
        &namespaces,
    );

    assert_eq!(
        topology
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["workspace-a", "workspace-b"]
    );
    assert_eq!(topology.workspaces[0].processes[0].pid, 201);
    assert_eq!(
        topology.workspaces[0].processes[0].cgroup_memberships,
        ["12:cpu:/shared", "11:memory:/shared"]
    );
    assert_eq!(topology.workspaces[1].processes[0].pid, 301);
    assert_eq!(
        topology.workspaces[1].processes[0].cgroup_memberships,
        ["0::/"]
    );
    assert!(topology
        .workspaces
        .iter()
        .all(|workspace| workspace.processes.iter().all(|process| process.pid != 999)));
}

#[test]
fn forked_descendants_remain_assigned_and_sorted() {
    let root = fixture("descendants");
    let mut namespaces = FakeNamespaceReader::default();
    namespaces.holder(&root, 40, 104, 204);
    for pid in [403, 401, 402] {
        namespaces.process(&root, pid, 104, 204);
    }
    write_process(&root, 403, 4, 402, "sleep", 'S', None);
    write_process(&root, 401, 1, 40, "ns-init", 'S', None);
    write_process(&root, 402, 3, 401, "sh", 'S', None);

    let topology = collect(&root, vec![workspace("workspace-a", 40)], &namespaces);

    assert_eq!(topology.workspaces[0].state, WorkspaceProcessState::Active);
    assert_eq!(
        topology.workspaces[0]
            .processes
            .iter()
            .map(|process| process.pid)
            .collect::<Vec<_>>(),
        [401, 402, 403]
    );
    assert_eq!(topology.workspaces[0].processes[2].parent_pid, 402);
}

#[test]
fn process_exit_races_and_missing_cgroup_membership_are_nonfatal() {
    let root = fixture("races");
    let mut namespaces = FakeNamespaceReader::default();
    namespaces.holder(&root, 50, 105, 205);
    namespaces.process(&root, 501, 105, 205);
    namespaces.process(&root, 502, 105, 205);
    std::fs::create_dir_all(root.join("501")).expect("racing proc entry");
    write_process(&root, 502, 1, 50, "ns-init", 'S', None);

    let topology = collect(&root, vec![workspace("workspace-a", 50)], &namespaces);

    assert!(topology.available);
    assert_eq!(topology.workspaces[0].processes.len(), 1);
    assert_eq!(topology.workspaces[0].processes[0].pid, 502);
    assert!(topology.workspaces[0].processes[0]
        .cgroup_memberships
        .is_empty());
}

#[test]
fn oversized_process_status_is_bounded_and_reported_as_partial() {
    let root = fixture("oversized-status");
    let mut namespaces = FakeNamespaceReader::default();
    namespaces.holder(&root, 57, 157, 257);
    namespaces.process(&root, 571, 157, 257);
    write_process(&root, 571, 1, 57, "ns-init", 'S', Some("0::/\n"));
    std::fs::write(root.join("571/status"), vec![b'x'; 64 * 1024 + 1])
        .expect("oversized status fixture");

    let topology = collect(&root, vec![workspace("workspace-a", 57)], &namespaces);

    assert!(topology.available);
    assert_eq!(topology.workspaces[0].state, WorkspaceProcessState::Partial);
    assert!(topology.workspaces[0].processes.is_empty());
    assert!(topology
        .warnings
        .iter()
        .any(|warning| warning.contains("status exceeded the read limit")));
    assert!(topology.warnings.len() <= 16);
}

#[test]
fn process_resource_estimates_include_rss_cpu_and_start_identity() {
    let root = fixture("resource-estimates");
    let mut namespaces = FakeNamespaceReader::default();
    namespaces.holder(&root, 55, 155, 255);
    namespaces.process(&root, 551, 155, 255);
    write_process_with_usage(&root, 551, 2, 55, "worker) name", 'R', 1_536, 25, 15, 900);

    let topology = collect(&root, vec![workspace("workspace-a", 55)], &namespaces);

    let process = &topology.workspaces[0].processes[0];
    assert_eq!(process.resident_memory_bytes, Some(1_536 * 1_024));
    assert!(process.cpu_time_us.is_some_and(|value| value > 0));
    assert_eq!(process.start_time_ticks, Some(900));
}

#[test]
fn missing_process_stat_omits_cpu_estimate_without_hiding_process() {
    let root = fixture("missing-stat");
    let mut namespaces = FakeNamespaceReader::default();
    namespaces.holder(&root, 56, 156, 256);
    namespaces.process(&root, 561, 156, 256);
    write_process(&root, 561, 1, 56, "ns-init", 'S', None);
    std::fs::remove_file(root.join("561/stat")).expect("remove stat fixture");

    let topology = collect(&root, vec![workspace("workspace-a", 56)], &namespaces);

    assert!(topology.available);
    let process = &topology.workspaces[0].processes[0];
    assert_eq!(process.pid, 561);
    assert_eq!(process.resident_memory_bytes, Some(256 * 1_024));
    assert_eq!(process.cpu_time_us, None);
    assert_eq!(process.start_time_ticks, None);
}

#[test]
fn daemon_metrics_collect_memory_cpu_io_threads_fds_and_cgroup() {
    let root = fixture("daemon-metrics");
    let pid = 42;
    write_process_with_usage(&root, pid, 1, 1, "sandbox-daemon", 'S', 30_000, 25, 15, 900);
    let pid_root = root.join(pid.to_string());
    std::fs::write(
        pid_root.join("status"),
        "Name:\tsandbox-daemon\nState:\tS (sleeping)\nVmSize:\t120000 kB\nVmHWM:\t32000 kB\nVmRSS:\t30000 kB\nRssAnon:\t26000 kB\nRssFile:\t3900 kB\nRssShmem:\t100 kB\nVmData:\t28000 kB\nVmSwap:\t20 kB\nThreads:\t37\nvoluntary_ctxt_switches:\t120\nnonvoluntary_ctxt_switches:\t3\n",
    )
    .expect("daemon status");
    std::fs::write(
        pid_root.join("smaps_rollup"),
        "00400000-00401000 r--p 00000000 00:00 0 [rollup]\nPss:\t28000 kB\nPrivate_Clean:\t1000 kB\nPrivate_Dirty:\t25000 kB\nPrivate_Hugetlb:\t0 kB\nAnonHugePages:\t2048 kB\n",
    )
    .expect("daemon smaps rollup");
    std::fs::write(
        pid_root.join("io"),
        "rchar: 12000\nwchar: 8000\nsyscr: 41\nsyscw: 17\nread_bytes: 4096\nwrite_bytes: 8192\n",
    )
    .expect("daemon io");
    std::fs::write(pid_root.join("schedstat"), "400123000 0 0\n").expect("daemon schedstat");
    std::fs::write(pid_root.join("cgroup"), "0::/_daemon\n").expect("daemon cgroup");
    std::fs::create_dir(pid_root.join("fd")).expect("daemon fd directory");
    std::fs::write(pid_root.join("fd/3"), "").expect("fd 3");
    std::fs::write(pid_root.join("fd/4"), "").expect("fd 4");

    let metrics = DaemonProcessMetrics::collect(&root, pid);

    assert!(metrics.available);
    assert_eq!(metrics.name.as_deref(), Some("sandbox-daemon"));
    assert_eq!(metrics.resident_memory_bytes, Some(30_000 * 1_024));
    assert_eq!(metrics.proportional_set_size_bytes, Some(28_000 * 1_024));
    assert_eq!(metrics.unique_set_size_bytes, Some(26_000 * 1_024));
    assert_eq!(metrics.private_dirty_bytes, Some(25_000 * 1_024));
    assert_eq!(metrics.anonymous_huge_pages_bytes, Some(2_048 * 1_024));
    assert_eq!(metrics.thread_count, Some(37));
    assert_eq!(metrics.file_descriptor_count, Some(2));
    assert_eq!(metrics.io_read_bytes, Some(4_096));
    assert_eq!(metrics.io_write_bytes, Some(8_192));
    assert_eq!(metrics.read_syscalls, Some(41));
    assert_eq!(metrics.write_syscalls, Some(17));
    assert_eq!(metrics.cgroup_memberships, ["0::/_daemon"]);
    assert_eq!(metrics.cgroup_path.as_deref(), Some("/_daemon"));
    assert_eq!(metrics.cpu_time_us, Some(400_123));
    assert!(metrics.warnings.is_empty());
}

#[test]
fn daemon_metrics_fall_back_to_stat_when_schedstat_is_unavailable() {
    let root = fixture("daemon-metrics-stat-fallback");
    let pid = 43;
    write_process_with_usage(&root, pid, 1, 1, "sandbox-daemon", 'S', 30_000, 25, 15, 900);
    assert!(!root.join(pid.to_string()).join("schedstat").exists());

    let metrics = DaemonProcessMetrics::collect(&root, pid);

    assert!(metrics.available);
    assert!(metrics.cpu_time_us.is_some_and(|value| value > 0));
}

#[test]
fn missing_holder_is_partial_without_hiding_the_workspace() {
    let root = fixture("missing-holder");

    let topology = collect(
        &root,
        vec![workspace("workspace-a", 60)],
        &FakeNamespaceReader::default(),
    );

    assert!(topology.available);
    assert_eq!(topology.workspaces[0].state, WorkspaceProcessState::Partial);
    assert!(topology.workspaces[0].processes.is_empty());
    assert_eq!(topology.warnings.len(), 1);
}

#[test]
fn process_rows_are_capped_at_512_and_warnings_are_bounded() {
    let root = fixture("bounds");
    let mut namespaces = FakeNamespaceReader::default();
    namespaces.holder(&root, 9_000, 109, 209);
    for pid in 1..=513 {
        namespaces.process(&root, pid, 109, 209);
        write_process(&root, pid, pid, 0, "worker", 'S', Some("0::/\n"));
    }
    for pid in 700..720 {
        std::fs::create_dir_all(root.join(pid.to_string())).expect("bad proc entry");
        namespaces.errors.insert(
            root.join(pid.to_string()).join("ns/pid"),
            io::ErrorKind::PermissionDenied,
        );
    }

    let first = collect(&root, vec![workspace("workspace-a", 9_000)], &namespaces);
    let second = collect(&root, vec![workspace("workspace-a", 9_000)], &namespaces);

    assert_eq!(first, second);
    assert!(first.truncated);
    assert_eq!(first.workspaces[0].state, WorkspaceProcessState::Partial);
    assert_eq!(first.workspaces[0].processes.len(), 512);
    assert_eq!(first.workspaces[0].processes[0].pid, 1);
    assert_eq!(first.workspaces[0].processes[511].pid, 512);
    assert!(first.warnings.len() <= 16);
}

fn collect(
    root: &Path,
    workspaces: Vec<WorkspaceProcessInput>,
    namespaces: &FakeNamespaceReader,
) -> WorkspaceProcessTopology {
    WorkspaceProcessTopology::collect_with_reader(root, workspaces, namespaces)
}

fn workspace(workspace_id: &str, holder_pid: u32) -> WorkspaceProcessInput {
    WorkspaceProcessInput {
        workspace_id: workspace_id.to_owned(),
        holder_pid,
        cgroup_path: Some(format!("/eos/workspace-{workspace_id}")),
        applied_cgroup_limits: Some(Default::default()),
        workload_cgroup_state: "applied".to_owned(),
        workload_cgroup_reason: None,
    }
}

fn write_process(
    root: &Path,
    pid: u32,
    namespace_pid: u32,
    parent_pid: u32,
    name: &str,
    state: char,
    cgroup: Option<&str>,
) {
    write_process_with_usage(
        root,
        pid,
        namespace_pid,
        parent_pid,
        name,
        state,
        256,
        10,
        5,
        u64::from(pid) * 100,
    );
    if let Some(cgroup) = cgroup {
        std::fs::write(root.join(pid.to_string()).join("cgroup"), cgroup).expect("cgroup");
    }
}

#[allow(clippy::too_many_arguments)]
fn write_process_with_usage(
    root: &Path,
    pid: u32,
    namespace_pid: u32,
    parent_pid: u32,
    name: &str,
    state: char,
    resident_kibibytes: u64,
    user_ticks: u64,
    system_ticks: u64,
    start_time_ticks: u64,
) {
    let pid_root = root.join(pid.to_string());
    std::fs::create_dir_all(&pid_root).expect("proc pid directory");
    std::fs::write(
        pid_root.join("status"),
        format!("Name:\t{name}\nState:\t{state} (state)\nPPid:\t{parent_pid}\nNSpid:\t{pid} {namespace_pid}\nVmRSS:\t{resident_kibibytes} kB\n"),
    )
    .expect("status");
    std::fs::write(pid_root.join("comm"), format!("{name}\n")).expect("comm");
    std::fs::write(
        pid_root.join("stat"),
        format!(
            "{pid} ({name}) {state} {parent_pid} 0 0 0 0 0 0 0 0 0 {user_ticks} {system_ticks} 0 0 0 0 1 0 {start_time_ticks}\n"
        ),
    )
    .expect("stat");
}

fn fixture(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "sandbox-process-topology-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).expect("fixture root");
    root
}
