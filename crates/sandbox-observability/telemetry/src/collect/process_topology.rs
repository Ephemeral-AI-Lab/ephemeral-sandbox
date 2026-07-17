use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use serde::Serialize;

const PROCESS_LIMIT: usize = 512;
const WARNING_LIMIT: usize = 16;
const STATUS_LIMIT: usize = 64 * 1024;
const COMM_LIMIT: usize = 256;
const CGROUP_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProcessInput {
    pub workspace_id: String,
    pub holder_pid: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WorkspaceProcessTopology {
    pub schema_version: u8,
    pub available: bool,
    pub source: Option<String>,
    pub error: Option<String>,
    pub truncated: bool,
    pub warnings: Vec<String>,
    pub workspaces: Vec<WorkspaceProcesses>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceProcesses {
    pub workspace_id: String,
    pub state: WorkspaceProcessState,
    pub holder_pid: u32,
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
        mut inputs: Vec<WorkspaceProcessInput>,
        namespace_reader: &dyn NamespaceIdentityReader,
    ) -> Self {
        inputs.sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));
        let mut warnings = WarningBuffer::default();
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

        let pids = match numeric_proc_entries(proc_root) {
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
            let process = match read_process(&pid_root, pid, &mut warnings) {
                Ok(process) => process,
                Err(error) if is_proc_race(&error) => continue,
                Err(error) => {
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

        Self {
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
        }
    }
}

struct WorkspaceBuilder {
    workspace: WorkspaceProcesses,
    identity: Option<(NamespaceIdentity, NamespaceIdentity)>,
    partial: bool,
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
                pid_namespace,
                mount_namespace,
                processes: Vec::new(),
            },
            identity,
            partial,
        }
    }
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
    })
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
}

fn parse_status(status: &str) -> io::Result<ProcessStatus> {
    let mut namespace_pid = None;
    let mut parent_pid = None;
    let mut state = None;
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
        }
    }
    match (namespace_pid, parent_pid, state) {
        (Some(namespace_pid), Some(parent_pid), Some(state)) => Ok(ProcessStatus {
            namespace_pid,
            parent_pid,
            state,
        }),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "required status fields are missing",
        )),
    }
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
