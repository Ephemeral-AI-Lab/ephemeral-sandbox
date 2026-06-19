use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::isolated_network_setup::{IsolatedNetwork, VethAllocation};
use crate::lifecycle::remount::WorkspaceRemountState;
use crate::model::NetworkMode;
use crate::namespace::{HolderKillReport, NamespacePlan, NamespaceRuntime};
use crate::overlay::dirs::OverlayDirs;
use crate::profile::host_compatible::HostCompatibleProfile;
use crate::profile::isolated::IsolatedProfile;
use crate::profile::manager::IsolatedNetworkError;
use crate::profile::resource_control;
use crate::profile::{WorkspaceModeHandle, WorkspaceModeId};

pub(crate) trait ProfileHooks {
    fn kind(&self) -> NetworkMode;

    fn namespace_plan(&self) -> NamespacePlan;

    fn setup_network_after_namespace(
        &mut self,
        _runtime: &NamespaceRuntime,
        _context: &mut WorkspaceProfileNetworkContext<'_>,
        _phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), IsolatedNetworkError> {
        Ok(())
    }

    fn setup_network_after_mount(
        &mut self,
        _runtime: &NamespaceRuntime,
        _context: &mut WorkspaceProfileNetworkContext<'_>,
        _phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), IsolatedNetworkError> {
        Ok(())
    }

    fn teardown_network(
        &mut self,
        _runtime: &NamespaceRuntime,
        _context: &WorkspaceProfileNetworkTeardownContext<'_>,
        _phases_ms: &mut HashMap<String, f64>,
    ) {
    }
}

pub(crate) enum WorkspaceProfile<'a> {
    HostCompatible(HostCompatibleProfile),
    Isolated(IsolatedProfile<'a>),
}

impl<'a> WorkspaceProfile<'a> {
    pub(crate) fn for_mode(
        network_mode: NetworkMode,
        network: &'a mut IsolatedNetwork,
        fallback_dns: &'a str,
        setup_timeout_s: f64,
    ) -> Self {
        match network_mode {
            NetworkMode::Host => Self::HostCompatible(HostCompatibleProfile),
            NetworkMode::Isolated => {
                Self::Isolated(IsolatedProfile::new(network, fallback_dns, setup_timeout_s))
            }
        }
    }
}

impl ProfileHooks for WorkspaceProfile<'_> {
    fn kind(&self) -> NetworkMode {
        match self {
            Self::HostCompatible(profile) => profile.kind(),
            Self::Isolated(profile) => profile.kind(),
        }
    }

    fn namespace_plan(&self) -> NamespacePlan {
        match self {
            Self::HostCompatible(profile) => profile.namespace_plan(),
            Self::Isolated(profile) => profile.namespace_plan(),
        }
    }

    fn setup_network_after_namespace(
        &mut self,
        runtime: &NamespaceRuntime,
        context: &mut WorkspaceProfileNetworkContext<'_>,
        phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), IsolatedNetworkError> {
        match self {
            Self::HostCompatible(profile) => {
                profile.setup_network_after_namespace(runtime, context, phases_ms)
            }
            Self::Isolated(profile) => {
                profile.setup_network_after_namespace(runtime, context, phases_ms)
            }
        }
    }

    fn setup_network_after_mount(
        &mut self,
        runtime: &NamespaceRuntime,
        context: &mut WorkspaceProfileNetworkContext<'_>,
        phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), IsolatedNetworkError> {
        match self {
            Self::HostCompatible(profile) => {
                profile.setup_network_after_mount(runtime, context, phases_ms)
            }
            Self::Isolated(profile) => {
                profile.setup_network_after_mount(runtime, context, phases_ms)
            }
        }
    }

    fn teardown_network(
        &mut self,
        runtime: &NamespaceRuntime,
        context: &WorkspaceProfileNetworkTeardownContext<'_>,
        phases_ms: &mut HashMap<String, f64>,
    ) {
        match self {
            Self::HostCompatible(profile) => profile.teardown_network(runtime, context, phases_ms),
            Self::Isolated(profile) => profile.teardown_network(runtime, context, phases_ms),
        }
    }
}

pub(crate) struct WorkspaceProfileNetworkContext<'a> {
    handle: &'a mut WorkspaceModeHandle,
}

impl WorkspaceProfileNetworkContext<'_> {
    #[must_use]
    pub(crate) fn workspace_id(&self) -> &WorkspaceModeId {
        &self.handle.workspace_id
    }

    #[must_use]
    pub(crate) fn holder_pid(&self) -> i32 {
        self.handle.holder_pid
    }

    pub(crate) fn set_veth(&mut self, veth: VethAllocation) {
        self.handle.veth = Some(veth);
    }

    pub(crate) fn configure_dns(
        &mut self,
        runtime: &NamespaceRuntime,
        fallback_dns: &str,
        setup_timeout_s: f64,
    ) -> Result<(), IsolatedNetworkError> {
        self.handle.dns_configuration =
            runtime.configure_dns(self.handle, fallback_dns, setup_timeout_s)?;
        Ok(())
    }

    pub(crate) fn signal_net_ready(
        &self,
        runtime: &NamespaceRuntime,
        setup_timeout_s: f64,
    ) -> Result<(), IsolatedNetworkError> {
        runtime.signal_net_ready(self.handle, setup_timeout_s)
    }
}

pub(crate) struct WorkspaceProfileNetworkTeardownContext<'a> {
    handle: &'a WorkspaceModeHandle,
}

impl WorkspaceProfileNetworkTeardownContext<'_> {
    #[must_use]
    pub(crate) fn veth(&self) -> Option<&VethAllocation> {
        self.handle.veth.as_ref()
    }
}

pub(crate) struct WorkspaceHandleSpec {
    pub workspace_id: WorkspaceModeId,
    pub network: NetworkMode,
    pub caller_id: String,
    pub lease_id: String,
    pub manifest_version: i64,
    pub manifest_root_hash: String,
    pub workspace_root: String,
    pub dirs: OverlayDirs,
    pub layer_paths: Vec<PathBuf>,
    pub created_at: f64,
    pub last_activity: f64,
}

#[must_use]
pub(crate) fn new_workspace_handle(spec: WorkspaceHandleSpec) -> WorkspaceModeHandle {
    WorkspaceModeHandle {
        workspace_id: spec.workspace_id,
        network: spec.network,
        caller_id: spec.caller_id,
        lease_id: spec.lease_id,
        manifest_version: spec.manifest_version,
        manifest_root_hash: spec.manifest_root_hash,
        workspace_root: spec.workspace_root,
        dirs: spec.dirs,
        layer_paths: spec.layer_paths,
        ns_fds: HashMap::new(),
        holder_pid: 0,
        readiness_fd: -1,
        control_fd: -1,
        veth: None,
        cgroup_path: None,
        dns_configuration: Default::default(),
        remount_state: WorkspaceRemountState::Active,
        created_at: spec.created_at,
        last_activity: spec.last_activity,
    }
}

pub(crate) fn wire_workspace(
    runtime: &NamespaceRuntime,
    handle: &mut WorkspaceModeHandle,
    layer_paths: &[PathBuf],
    setup_timeout_s: f64,
    hooks: &mut impl ProfileHooks,
) -> Result<HashMap<String, f64>, IsolatedNetworkError> {
    let mut phases_ms = HashMap::new();
    if hooks.kind() != handle.network {
        return Err(IsolatedNetworkError::InvalidArgument(format!(
            "profile {:?} cannot wire {:?} workspace",
            hooks.kind(),
            handle.network
        )));
    }
    let namespace_plan = hooks.namespace_plan();
    let mut phase_start = Instant::now();
    handle.holder_pid = runtime.spawn_ns_holder(handle, setup_timeout_s, namespace_plan)?;
    record_phase_ms(&mut phases_ms, "spawn_ns_holder", phase_start);
    phase_start = Instant::now();
    handle.ns_fds = runtime.open_ns_fds(handle.holder_pid, namespace_plan)?;
    record_phase_ms(&mut phases_ms, "open_ns_fds", phase_start);
    hooks.setup_network_after_namespace(
        runtime,
        &mut WorkspaceProfileNetworkContext { handle },
        &mut phases_ms,
    )?;
    phase_start = Instant::now();
    runtime.mount_overlay(handle, layer_paths, setup_timeout_s)?;
    record_phase_ms(&mut phases_ms, "mount_overlay", phase_start);
    hooks.setup_network_after_mount(
        runtime,
        &mut WorkspaceProfileNetworkContext { handle },
        &mut phases_ms,
    )?;
    resource_control::create_cgroup(runtime, handle, &mut phases_ms)?;
    Ok(phases_ms)
}

pub(crate) struct TeardownReport {
    pub holder_kill_report: HolderKillReport,
    pub holder_kill_error: Option<String>,
    pub phases_ms: HashMap<String, f64>,
}

pub(crate) fn teardown_workspace(
    runtime: &NamespaceRuntime,
    handle: &WorkspaceModeHandle,
    hooks: &mut impl ProfileHooks,
    grace_s: f64,
) -> TeardownReport {
    let mut phases_ms = HashMap::new();
    let phase_start = Instant::now();
    let (holder_kill_report, holder_kill_error) = if handle.holder_pid > 0 {
        match runtime.kill_holder(handle.holder_pid, grace_s) {
            Ok(report) => (report, None),
            Err(err) => (HolderKillReport::default(), Some(err.to_string())),
        }
    } else {
        (HolderKillReport::default(), None)
    };
    record_phase_ms(&mut phases_ms, "kill_holder", phase_start);
    close_handle_fds(handle);
    hooks.teardown_network(
        runtime,
        &WorkspaceProfileNetworkTeardownContext { handle },
        &mut phases_ms,
    );
    let _ = resource_control::remove_cgroup(handle, &mut phases_ms);
    let phase_start = Instant::now();
    let _ = std::fs::remove_dir_all(&handle.dirs.run_dir);
    record_phase_ms(&mut phases_ms, "rmtree_scratch", phase_start);
    TeardownReport {
        holder_kill_report,
        holder_kill_error,
        phases_ms,
    }
}

pub(crate) fn close_handle_fds(handle: &WorkspaceModeHandle) {
    for fd in handle.ns_fds.values().copied() {
        close_fd(fd);
    }
    close_fd(handle.readiness_fd);
    close_fd(handle.control_fd);
}

pub(crate) fn close_fd(fd: i32) {
    if fd >= 0 {
        let _ = nix::unistd::close(fd);
    }
}

pub(crate) fn record_phase_ms(
    phases_ms: &mut HashMap<String, f64>,
    phase: &str,
    started_at: Instant,
) {
    phases_ms.insert(
        phase.to_owned(),
        started_at.elapsed().as_secs_f64() * 1000.0,
    );
}
