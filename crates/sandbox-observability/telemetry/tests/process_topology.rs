use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_observability_telemetry::collect::process_topology::{
    NamespaceIdentity, NamespaceIdentityReader, WorkspaceProcessInput, WorkspaceProcessKind,
    WorkspaceProcessState, WorkspaceProcessTopology,
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
    let pid_root = root.join(pid.to_string());
    std::fs::create_dir_all(&pid_root).expect("proc pid directory");
    std::fs::write(
        pid_root.join("status"),
        format!("Name:\t{name}\nState:\t{state} (state)\nPPid:\t{parent_pid}\nNSpid:\t{pid} {namespace_pid}\n"),
    )
    .expect("status");
    std::fs::write(pid_root.join("comm"), format!("{name}\n")).expect("comm");
    if let Some(cgroup) = cgroup {
        std::fs::write(pid_root.join("cgroup"), cgroup).expect("cgroup");
    }
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
