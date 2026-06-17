use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::isolated_workspace::{
    IsolatedWorkspaceBinding, WorkspaceHandle as IsolatedWorkspaceHandle,
};
use crate::overlay::tree::TreeResourceStats;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkspaceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallerId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseRevision {
    pub version: i64,
    pub root_hash: String,
    pub layer_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    Host,
    Isolated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceHandle {
    pub id: WorkspaceId,
    pub owner: CallerId,
    pub workspace_root: PathBuf,
    pub network: NetworkMode,
    pub base_revision: BaseRevision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorkspaceRequest {
    pub owner: CallerId,
    pub workspace_root: PathBuf,
    pub network: NetworkMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunCommandRequest {
    pub invocation_id: String,
    pub cmd: String,
    pub cwd: Option<PathBuf>,
    pub timeout_seconds: Option<f64>,
    pub yield_time_ms: u64,
    pub remountable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    Running,
    Ok,
    Cancelled,
    Error,
    TimedOut,
}

impl CommandStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
            Self::TimedOut => "timed_out",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunCommandResult {
    pub status: CommandStatus,
    pub command_id: Option<String>,
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
    pub changed_paths: Vec<String>,
    pub base_revision: BaseRevision,
    pub published: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureChangesRequest {
    pub materialize_payloads: bool,
    pub include_stats: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedPathKind {
    Write,
    Delete,
    Symlink,
    OpaqueDir,
}

impl From<&layerstack::LayerChange> for ChangedPathKind {
    fn from(change: &layerstack::LayerChange) -> Self {
        match change {
            layerstack::LayerChange::Write { .. } | layerstack::LayerChange::WriteFile { .. } => {
                Self::Write
            }
            layerstack::LayerChange::Delete { .. } => Self::Delete,
            layerstack::LayerChange::Symlink { .. } => Self::Symlink,
            layerstack::LayerChange::OpaqueDir { .. } => Self::OpaqueDir,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedPathDropReason {
    UnsupportedSpecialFile,
    InvalidLayerPath,
}

impl From<layerstack::ProtectedPathDropReason> for ProtectedPathDropReason {
    fn from(reason: layerstack::ProtectedPathDropReason) -> Self {
        match reason {
            layerstack::ProtectedPathDropReason::UnsupportedSpecialFile => {
                Self::UnsupportedSpecialFile
            }
            layerstack::ProtectedPathDropReason::InvalidLayerPath => Self::InvalidLayerPath,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedPathDrop {
    pub path: String,
    pub reason: ProtectedPathDropReason,
}

impl From<&layerstack::ProtectedPathDrop> for ProtectedPathDrop {
    fn from(drop: &layerstack::ProtectedPathDrop) -> Self {
        Self {
            path: drop.path.as_str().to_owned(),
            reason: ProtectedPathDropReason::from(drop.reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureChangesResult {
    pub workspace_id: WorkspaceId,
    pub base_revision: BaseRevision,
    pub changed_paths: Vec<String>,
    pub changed_path_kinds: BTreeMap<String, ChangedPathKind>,
    pub protected_drops: Vec<ProtectedPathDrop>,
    pub stats: Option<TreeResourceStats>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DestroyWorkspaceRequest {
    pub grace_s: Option<f64>,
    pub cancel_commands: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DestroyWorkspaceResult {
    pub workspace_id: WorkspaceId,
    pub owner: CallerId,
    pub cancelled_commands: usize,
    pub evicted_upperdir_bytes: u64,
    pub lifetime_s: f64,
    pub lease_released: Option<bool>,
    pub lease_release_error: Option<String>,
    pub active_leases_after: usize,
}

impl From<&IsolatedWorkspaceHandle> for WorkspaceHandle {
    fn from(handle: &IsolatedWorkspaceHandle) -> Self {
        Self {
            id: WorkspaceId(handle.workspace_id.0.clone()),
            owner: CallerId(handle.caller_id.clone()),
            workspace_root: PathBuf::from(&handle.workspace_root),
            network: NetworkMode::Isolated,
            base_revision: BaseRevision {
                version: handle.manifest_version,
                root_hash: handle.manifest_root_hash.clone(),
                layer_count: handle.layer_paths.len(),
            },
        }
    }
}

impl From<IsolatedWorkspaceHandle> for WorkspaceHandle {
    fn from(handle: IsolatedWorkspaceHandle) -> Self {
        Self {
            id: WorkspaceId(handle.workspace_id.0),
            owner: CallerId(handle.caller_id),
            workspace_root: PathBuf::from(handle.workspace_root),
            network: NetworkMode::Isolated,
            base_revision: BaseRevision {
                version: handle.manifest_version,
                root_hash: handle.manifest_root_hash,
                layer_count: handle.layer_paths.len(),
            },
        }
    }
}

impl From<&IsolatedWorkspaceBinding> for WorkspaceHandle {
    fn from(binding: &IsolatedWorkspaceBinding) -> Self {
        Self {
            id: WorkspaceId(binding.workspace_handle_id.clone()),
            owner: CallerId(binding.caller_id.clone()),
            workspace_root: binding.workspace_root.clone(),
            network: NetworkMode::Isolated,
            base_revision: BaseRevision {
                version: binding.manifest_version,
                root_hash: binding.manifest_root_hash.clone(),
                layer_count: binding.layer_paths.len(),
            },
        }
    }
}

impl From<IsolatedWorkspaceBinding> for WorkspaceHandle {
    fn from(binding: IsolatedWorkspaceBinding) -> Self {
        Self {
            id: WorkspaceId(binding.workspace_handle_id),
            owner: CallerId(binding.caller_id),
            workspace_root: binding.workspace_root,
            network: NetworkMode::Isolated,
            base_revision: BaseRevision {
                version: binding.manifest_version,
                root_hash: binding.manifest_root_hash,
                layer_count: binding.layer_paths.len(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use crate::isolated_workspace::{DnsConfiguration, IsolatedWorkspaceId, WorkspaceRemountState};
    use crate::overlay::dirs::OverlayDirs;

    use super::*;

    fn isolated_handle() -> IsolatedWorkspaceHandle {
        IsolatedWorkspaceHandle {
            workspace_id: IsolatedWorkspaceId("isolated-handle".to_owned()),
            caller_id: "caller-1".to_owned(),
            lease_id: "lease-1".to_owned(),
            manifest_version: 42,
            manifest_root_hash: "root-hash".to_owned(),
            workspace_root: "/workspace".to_owned(),
            dirs: OverlayDirs {
                run_dir: "/tmp/eos/run".into(),
                upperdir: "/tmp/eos/upper".into(),
                workdir: "/tmp/eos/work".into(),
            },
            layer_paths: vec!["/lower/one".into(), "/lower/two".into()],
            ns_fds: HashMap::from([("mnt".to_owned(), 11), ("pid".to_owned(), 12)]),
            holder_pid: 1234,
            readiness_fd: 13,
            control_fd: 14,
            veth: None,
            cgroup_path: Some("/sys/fs/cgroup/eos".into()),
            dns_configuration: DnsConfiguration {
                fallback_applied: true,
                previous_first_nameserver: Some("127.0.0.53".to_owned()),
            },
            remount_state: WorkspaceRemountState::Pending,
            created_at: 1.0,
            last_activity: 2.0,
        }
    }

    fn isolated_binding() -> IsolatedWorkspaceBinding {
        IsolatedWorkspaceBinding {
            caller_id: "caller-2".to_owned(),
            workspace_handle_id: "binding-handle".to_owned(),
            layer_stack_root: "/layer-stack".into(),
            manifest_version: 7,
            manifest_root_hash: "binding-root-hash".to_owned(),
            workspace_root: "/workspace".into(),
            scratch_dir: "/tmp/eos/run".into(),
            upperdir: "/tmp/eos/upper".into(),
            workdir: "/tmp/eos/work".into(),
            layer_paths: vec![
                "/lower/one".into(),
                "/lower/two".into(),
                "/lower/three".into(),
            ],
            ns_fds: HashMap::from([("mnt".to_owned(), 21), ("pid".to_owned(), 22)]),
            cgroup_path: Some("/sys/fs/cgroup/eos".into()),
        }
    }

    fn assert_isolated_handle_public(public: &WorkspaceHandle) {
        assert_eq!(public.id, WorkspaceId("isolated-handle".to_owned()));
        assert_eq!(public.owner, CallerId("caller-1".to_owned()));
        assert_eq!(public.workspace_root, PathBuf::from("/workspace"));
        assert_eq!(public.network, NetworkMode::Isolated);
        assert_eq!(
            public.base_revision,
            BaseRevision {
                version: 42,
                root_hash: "root-hash".to_owned(),
                layer_count: 2,
            }
        );
    }

    fn assert_isolated_binding_public(public: &WorkspaceHandle) {
        assert_eq!(public.id, WorkspaceId("binding-handle".to_owned()));
        assert_eq!(public.owner, CallerId("caller-2".to_owned()));
        assert_eq!(public.workspace_root, PathBuf::from("/workspace"));
        assert_eq!(public.network, NetworkMode::Isolated);
        assert_eq!(
            public.base_revision,
            BaseRevision {
                version: 7,
                root_hash: "binding-root-hash".to_owned(),
                layer_count: 3,
            }
        );
    }

    #[test]
    fn converts_borrowed_and_owned_isolated_handle_to_public_handle() {
        let handle = isolated_handle();

        assert_isolated_handle_public(&WorkspaceHandle::from(&handle));
        assert_isolated_handle_public(&WorkspaceHandle::from(handle));
    }

    #[test]
    fn converts_borrowed_and_owned_isolated_binding_to_public_handle() {
        let binding = isolated_binding();

        assert_isolated_binding_public(&WorkspaceHandle::from(&binding));
        assert_isolated_binding_public(&WorkspaceHandle::from(binding));
    }

    #[test]
    fn public_handle_debug_does_not_expose_internal_storage_or_namespace_fields() {
        let public = WorkspaceHandle::from(&isolated_handle());
        let debug = format!("{public:?}");

        assert_no_internal_fields(&debug);
    }

    #[test]
    fn public_dto_debug_does_not_expose_internal_storage_or_namespace_fields() {
        let base_revision = BaseRevision {
            version: 1,
            root_hash: "root".to_owned(),
            layer_count: 1,
        };
        let dtos = [
            format!(
                "{:?}",
                CreateWorkspaceRequest {
                    owner: CallerId("caller".to_owned()),
                    workspace_root: "/workspace".into(),
                    network: NetworkMode::Host,
                }
            ),
            format!(
                "{:?}",
                WorkspaceHandle {
                    id: WorkspaceId("workspace".to_owned()),
                    owner: CallerId("caller".to_owned()),
                    workspace_root: "/workspace".into(),
                    network: NetworkMode::Host,
                    base_revision: base_revision.clone(),
                }
            ),
            format!(
                "{:?}",
                RunCommandRequest {
                    invocation_id: "invocation".to_owned(),
                    cmd: "true".to_owned(),
                    cwd: Some("/workspace".into()),
                    timeout_seconds: Some(1.0),
                    yield_time_ms: 1_000,
                    remountable: false,
                }
            ),
            format!(
                "{:?}",
                RunCommandResult {
                    status: CommandStatus::Ok,
                    command_id: Some("command".to_owned()),
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                    changed_paths: Vec::new(),
                    base_revision: base_revision.clone(),
                    published: false,
                }
            ),
            format!(
                "{:?}",
                CaptureChangesRequest {
                    materialize_payloads: false,
                    include_stats: true,
                }
            ),
            format!(
                "{:?}",
                CaptureChangesResult {
                    workspace_id: WorkspaceId("workspace".to_owned()),
                    base_revision,
                    changed_paths: Vec::new(),
                    changed_path_kinds: BTreeMap::new(),
                    protected_drops: Vec::new(),
                    stats: None,
                }
            ),
            format!(
                "{:?}",
                DestroyWorkspaceRequest {
                    grace_s: Some(1.0),
                    cancel_commands: true,
                }
            ),
            format!(
                "{:?}",
                DestroyWorkspaceResult {
                    workspace_id: WorkspaceId("workspace".to_owned()),
                    owner: CallerId("caller".to_owned()),
                    cancelled_commands: 0,
                    evicted_upperdir_bytes: 0,
                    lifetime_s: 0.0,
                    lease_released: Some(true),
                    lease_release_error: None,
                    active_leases_after: 0,
                }
            ),
        ];

        for debug in dtos {
            assert_no_internal_fields(&debug);
        }
    }

    fn assert_no_internal_fields(debug: &str) {
        for forbidden in [
            "layer_stack_root:",
            "upperdir:",
            "workdir:",
            "scratch_dir:",
            "ns_fds:",
            "holder_pid:",
            "readiness_fd:",
            "control_fd:",
            "cgroup_path:",
            "veth:",
            "dns_configuration:",
        ] {
            assert!(
                !debug.contains(forbidden),
                "public DTO debug output exposed {forbidden}: {debug}"
            );
        }
    }

    #[test]
    fn public_dtos_construct_clone_and_compare() {
        let base_revision = BaseRevision {
            version: 1,
            root_hash: "root".to_owned(),
            layer_count: 1,
        };
        let create = CreateWorkspaceRequest {
            owner: CallerId("caller".to_owned()),
            workspace_root: "/workspace".into(),
            network: NetworkMode::Host,
        };
        let handle = WorkspaceHandle {
            id: WorkspaceId("workspace".to_owned()),
            owner: CallerId("caller".to_owned()),
            workspace_root: "/workspace".into(),
            network: NetworkMode::Host,
            base_revision: base_revision.clone(),
        };
        let run = RunCommandRequest {
            invocation_id: "invocation".to_owned(),
            cmd: "true".to_owned(),
            cwd: Some("/workspace".into()),
            timeout_seconds: Some(1.5),
            yield_time_ms: 1_000,
            remountable: false,
        };
        let run_result = RunCommandResult {
            status: CommandStatus::Ok,
            command_id: Some("command".to_owned()),
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            changed_paths: vec!["src/main.rs".to_owned()],
            base_revision: base_revision.clone(),
            published: false,
        };
        let capture_request = CaptureChangesRequest {
            materialize_payloads: true,
            include_stats: true,
        };
        let capture = CaptureChangesResult {
            workspace_id: WorkspaceId("workspace".to_owned()),
            base_revision: base_revision.clone(),
            changed_paths: vec!["src/main.rs".to_owned()],
            changed_path_kinds: BTreeMap::from([(
                "src/main.rs".to_owned(),
                ChangedPathKind::Write,
            )]),
            protected_drops: vec![ProtectedPathDrop {
                path: "fifo".to_owned(),
                reason: ProtectedPathDropReason::UnsupportedSpecialFile,
            }],
            stats: Some(TreeResourceStats {
                files: 1,
                ..TreeResourceStats::default()
            }),
        };
        let destroy_request = DestroyWorkspaceRequest {
            grace_s: Some(1.0),
            cancel_commands: true,
        };
        let destroy = DestroyWorkspaceResult {
            workspace_id: WorkspaceId("workspace".to_owned()),
            owner: CallerId("caller".to_owned()),
            cancelled_commands: 0,
            evicted_upperdir_bytes: 0,
            lifetime_s: 0.0,
            lease_released: Some(true),
            lease_release_error: None,
            active_leases_after: 0,
        };

        assert_eq!(create.clone(), create);
        assert_eq!(handle.clone(), handle);
        assert_eq!(run.clone(), run);
        assert_eq!(run_result.clone(), run_result);
        assert_eq!(capture_request.clone(), capture_request);
        assert_eq!(capture.clone(), capture);
        assert_eq!(destroy_request.clone(), destroy_request);
        assert_eq!(destroy.clone(), destroy);
        assert_eq!(CommandStatus::TimedOut.as_str(), "timed_out");
    }
}
