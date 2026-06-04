//! The persistent private session: enter/exit lifecycle and control-plane ports.
//!
//! `IsolatedSession` owns the per-agent persistent workspace. `enter` acquires a
//! layer-stack snapshot/lease, allocates scratch (upper/work), wires the
//! namespace (ns-holder spawn -> ns FDs -> overlay mount -> DNS -> net-ready),
//! and persists the handle. `exit` tears down the namespace + network + cgroup,
//! releases the lease, and DISCARDS the upperdir (writes are captured for audit
//! only, never published). The agent-facing background-session guard lives in
//! `eos-tools`; daemon enter/exit callers may still run command-session cleanup
//! before mutating lifecycle state.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use crate::audit::AuditSink;
use crate::caps::ResourceCaps;
use crate::error::IsolatedError;
use crate::network::{IsolatedNetwork, VethAllocation};
use serde_json::{json, Value};

use self::support::{directory_file_bytes, monotonic_seconds, next_handle_id};

mod capacity;
mod gc;
mod lifecycle;
mod persistence;
#[path = "../tests/session/support.rs"]
mod support;
#[cfg(test)]
#[path = "../tests/session/mod.rs"]
mod tests;

/// Newtype for an agent identity (the enter/exit key).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentId(pub String);

/// Newtype for a per-workspace handle id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkspaceHandleId(pub String);

/// A snapshot lease borrowed from the layer stack (snapshot/lease HINGE only).
///
/// Mirrors the `acquire_snapshot` result the isolated pipeline consumes; it
/// carries the lease id, manifest coordinates, and the lower-layer paths the
/// overlay mounts. NEVER a publish transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotLease {
    /// Lease id to release on exit/rollback.
    pub lease_id: String,
    /// Active manifest version captured at acquire time.
    pub manifest_version: i64,
    /// Active manifest root hash captured at acquire time.
    pub root_hash: String,
    /// Lower-layer paths to feed the overlay mount (newest-first).
    pub layer_paths: Vec<String>,
}

/// Per-workspace state. Not a subclass of any overlay handle (C1).
#[derive(Debug, Clone)]
pub struct WorkspaceHandle {
    /// Stable handle id (also the scratch dir / veth-name seed).
    pub workspace_handle_id: WorkspaceHandleId,
    /// Owning agent.
    pub agent_id: AgentId,
    /// Snapshot lease borrowed from the layer stack.
    pub lease_id: String,
    /// Manifest version captured at acquire time.
    pub manifest_version: i64,
    /// Manifest root hash captured at acquire time.
    pub manifest_root_hash: String,
    /// Visible EOS workspace mount target inside the namespace.
    pub workspace_root: String,
    /// Scratch directory root (parent of upper/work).
    pub scratch_dir: PathBuf,
    /// Overlay upperdir (DISCARDED on exit — never published).
    pub upperdir: PathBuf,
    /// Overlay workdir.
    pub workdir: PathBuf,
    /// Lower-layer paths pinned by the snapshot lease.
    pub layer_paths: Vec<String>,
    /// Open namespace FDs by name (`user`/`mnt`/`pid`/`net`).
    pub ns_fds: HashMap<String, i32>,
    /// ns-holder PID (`0` = not spawned).
    pub holder_pid: i32,
    /// Readiness-pipe FD (`-1` = not opened).
    pub readiness_fd: i32,
    /// Control-pipe FD (`-1` = not opened).
    pub control_fd: i32,
    /// veth allocation, if networking is wired.
    pub veth: Option<VethAllocation>,
    /// Per-workspace cgroup path, if created.
    pub cgroup_path: Option<PathBuf>,
    /// Monotonic create time.
    pub created_at: f64,
    /// Monotonic last-activity time (TTL input).
    pub last_activity: f64,
}

/// Snapshot/lease HINGE port — the ONLY layer-stack surface isolated models.
///
/// Defined here as an inverted port (`eos-daemon` injects the layer-stack-backed
/// implementation). It exposes snapshot/lease + read methods ONLY — never the
/// publish-transaction half — so this crate needs neither a direct
/// `eos-layerstack` nor an `eos-occ` dependency.
pub trait LayerStackSnapshotPort {
    /// Acquire a read snapshot + lease for `request_id`.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the layer-stack snapshot or lease cannot
    /// be acquired.
    fn acquire_snapshot(&self, request_id: &str) -> Result<SnapshotLease, IsolatedError>;

    /// Release the lease held by `lease_id`. Returns whether it was held.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the layer-stack lease release cannot be
    /// checked or completed.
    fn release_lease(&self, lease_id: &str) -> Result<bool, IsolatedError>;

    /// Optional daemon-local diagnostic count for active leases owned by this
    /// port instance. This is intentionally diagnostic-only and exposes no
    /// publish surface.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the backing diagnostic state cannot be
    /// inspected.
    fn active_lease_count(&self) -> Result<Option<usize>, IsolatedError> {
        Ok(None)
    }
}

/// Kernel-touching namespace operations the pipeline delegates to.
///
/// Inverted port: the concrete implementation spawns `eosd ns-holder` (the
/// long-lived pidns PID 1) and drives `setns` mounts/exec via `eosd ns-runner`.
/// Both are syscall-only single-threaded crates; this trait keeps the
/// orchestration here free of those edges' details.
pub trait NamespaceRuntimePort {
    /// Spawn `eosd ns-holder` under `unshare(--user --net --pid --mount ...)`,
    /// wait for the `ns-up` handshake token, and return its PID.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when holder launch or readiness signaling
    /// fails.
    fn spawn_ns_holder(
        &self,
        handle: &mut WorkspaceHandle,
        setup_timeout_s: f64,
    ) -> Result<i32, IsolatedError>;

    /// Open `/proc/<pid>/ns/{user,mnt,pid,net}` FDs for `holder_pid`.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when namespace FDs cannot be opened.
    fn open_ns_fds(&self, holder_pid: i32) -> Result<HashMap<String, i32>, IsolatedError>;

    /// Mount the overlay inside the namespace (via `eosd ns-runner` setns helper).
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the setns overlay mount helper fails.
    fn mount_overlay(
        &self,
        handle: &WorkspaceHandle,
        layer_paths: &[String],
    ) -> Result<(), IsolatedError>;

    /// Configure DNS inside the namespace; returns whether the fallback applied.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when DNS configuration cannot be applied or
    /// inspected.
    fn configure_dns(
        &self,
        handle: &WorkspaceHandle,
        fallback_dns: &str,
    ) -> Result<bool, IsolatedError>;

    /// Send `net-ready` and await the `ready` token (handshake steps 2-3).
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the holder control/readiness handshake
    /// fails or times out.
    fn signal_net_ready(
        &self,
        handle: &WorkspaceHandle,
        setup_timeout_s: f64,
    ) -> Result<(), IsolatedError>;

    /// Create the per-workspace cgroup and return its path.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when cgroup creation fails.
    fn create_cgroup(&self, handle: &WorkspaceHandle) -> Result<PathBuf, IsolatedError>;

    /// SIGTERM (then SIGKILL after `grace_s`) the ns-holder and reap children.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when holder teardown fails.
    fn kill_holder(&self, holder_pid: i32, grace_s: f64) -> Result<(), IsolatedError>;
}

/// Owns the isolated-workspace lifecycle, namespace runtime, capacity, TTL, GC.
///
/// Generic over the injected snapshot/lease + namespace ports and audit sink so
/// `eos-daemon` wires the kernel-backed implementations and tests inject
/// doubles. Holds the per-agent / per-handle maps and the shared network state.
pub struct IsolatedSession<S, R, A>
where
    S: LayerStackSnapshotPort,
    R: NamespaceRuntimePort,
    A: AuditSink,
{
    caps: ResourceCaps,
    layer_stack: S,
    runtime: R,
    audit: A,
    network: IsolatedNetwork,
    scratch_root: PathBuf,
    handles: HashMap<WorkspaceHandleId, WorkspaceHandle>,
    by_agent: HashMap<AgentId, WorkspaceHandleId>,
}

impl<S, R, A> IsolatedSession<S, R, A>
where
    S: LayerStackSnapshotPort,
    R: NamespaceRuntimePort,
    A: AuditSink,
{
    /// Construct a session with injected ports, caps, and audit sink.
    #[must_use]
    pub fn new(caps: ResourceCaps, layer_stack: S, runtime: R, audit: A) -> Self {
        Self::with_scratch_root(
            caps,
            layer_stack,
            runtime,
            audit,
            PathBuf::from(eos_overlay::OVERLAY_WRITABLE_ROOT),
        )
    }

    /// Construct a session with an explicit scratch root.
    ///
    /// The daemon uses the canonical `/eos/mount` root in Docker. Focused
    /// unit tests inject a temporary scratch root through this constructor so
    /// lifecycle behavior can be verified without depending on host `/eos`.
    #[must_use]
    pub fn with_scratch_root(
        caps: ResourceCaps,
        layer_stack: S,
        runtime: R,
        audit: A,
        scratch_root: PathBuf,
    ) -> Self {
        let network = IsolatedNetwork::new(caps.rfc1918_egress);
        Self {
            caps,
            layer_stack,
            runtime,
            audit,
            network,
            scratch_root,
            handles: HashMap::new(),
            by_agent: HashMap::new(),
        }
    }

    /// Reconcile persisted handles + IP pool at startup before serving enters.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the feature is disabled, network setup
    /// fails, or the session scratch root cannot be created.
    pub fn initialize(&mut self) -> Result<(), IsolatedError> {
        if !self.caps.enabled {
            return Err(IsolatedError::FeatureDisabled);
        }
        self.network.initialize()?;
        std::fs::create_dir_all(self.session_scratch_root()).map_err(|err| {
            IsolatedError::SetupFailed {
                step: format!("scratch_root: {err}"),
            }
        })?;
        self.reap_startup_orphans()?;
        Ok(())
    }

    /// Enter (or reject) the isolated workspace for `agent_id`.
    ///
    /// Acquires the snapshot/lease, allocates scratch, wires the namespace, and
    /// registers the handle. Rolls back partial state (and releases the lease)
    /// on any wiring failure.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the feature is disabled, `agent_id` is
    /// invalid, capacity is exhausted, snapshot acquisition fails, or namespace
    /// wiring fails.
    pub fn enter(&mut self, agent_id: &AgentId) -> Result<WorkspaceHandle, IsolatedError> {
        if !self.caps.enabled {
            return Err(IsolatedError::FeatureDisabled);
        }
        if agent_id.0.trim().is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "agent_id is required".to_owned(),
            ));
        }
        let workspace_root = self.validated_workspace_root()?;
        if self.by_agent.contains_key(agent_id) {
            let existing = self
                .by_agent
                .get(agent_id)
                .and_then(|handle_id| self.handles.get(handle_id))
                .ok_or_else(|| IsolatedError::SetupFailed {
                    step: "agent handle index is inconsistent".to_owned(),
                })?;
            return Err(IsolatedError::AlreadyOpen {
                created_at: existing.created_at,
                last_activity: existing.last_activity,
            });
        }
        let total_cap = usize::try_from(self.caps.total_cap).unwrap_or(usize::MAX);
        if self.handles.len() >= total_cap {
            return Err(IsolatedError::QuotaExceeded {
                total_cap: self.caps.total_cap,
            });
        }
        self.check_host_capacity()?;

        let workspace_handle_id = WorkspaceHandleId(next_handle_id());
        let snapshot = self
            .layer_stack
            .acquire_snapshot(&format!("isolated-{}", workspace_handle_id.0))?;
        let scratch_dir = self.session_scratch_root().join(&workspace_handle_id.0);
        let upperdir = scratch_dir.join("upper");
        let workdir = scratch_dir.join("work");
        std::fs::create_dir_all(&upperdir).map_err(|err| IsolatedError::SetupFailed {
            step: format!("upperdir: {err}"),
        })?;
        std::fs::create_dir_all(&workdir).map_err(|err| IsolatedError::SetupFailed {
            step: format!("workdir: {err}"),
        })?;

        let now = monotonic_seconds();
        let mut handle = WorkspaceHandle {
            workspace_handle_id: workspace_handle_id.clone(),
            agent_id: agent_id.clone(),
            lease_id: snapshot.lease_id.clone(),
            manifest_version: snapshot.manifest_version,
            manifest_root_hash: snapshot.root_hash.clone(),
            workspace_root,
            scratch_dir,
            upperdir,
            workdir,
            layer_paths: snapshot.layer_paths.clone(),
            ns_fds: HashMap::new(),
            holder_pid: 0,
            readiness_fd: -1,
            control_fd: -1,
            veth: None,
            cgroup_path: None,
            created_at: now,
            last_activity: now,
        };

        let enter_timer = Instant::now();
        let phases_ms = match self.wire_handle(&mut handle) {
            Ok(phases_ms) => phases_ms,
            Err(err) => {
                self.rollback_partial(&handle);
                let _ = self.layer_stack.release_lease(&snapshot.lease_id);
                return Err(err);
            }
        };
        let total_ms = enter_timer.elapsed().as_secs_f64() * 1000.0;

        self.by_agent
            .insert(agent_id.clone(), workspace_handle_id.clone());
        self.handles
            .insert(workspace_handle_id.clone(), handle.clone());
        let _ = self.persist_handles();
        let _ = self.audit.emit(
            "sandbox_isolated_workspace_enter",
            json!({
                "workspace_handle_id": workspace_handle_id.0,
                "agent_id": agent_id.0,
                "manifest_version": handle.manifest_version,
                "manifest_root_hash": handle.manifest_root_hash,
                "lease_id": handle.lease_id,
                "lowerdir_layer_count": handle.layer_paths.len(),
                "workspace_root": handle.workspace_root,
                "upperdir": handle.upperdir.to_string_lossy(),
                "workdir": handle.workdir.to_string_lossy(),
                "veth_host_name": handle.veth.as_ref().map(|veth| veth.host_name.as_str()),
                "veth_ns_name": handle.veth.as_ref().map(|veth| veth.ns_name.as_str()),
                "ns_ip": handle.veth.as_ref().map(|veth| veth.ns_ip.to_string()),
                "tree-copy": false,
                "total_ms": total_ms,
                "phases_ms": phases_ms,
            }),
        );
        Ok(handle)
    }

    fn validated_workspace_root(&self) -> Result<String, IsolatedError> {
        let workspace_root = self.caps.eos_workspace_root.trim();
        if workspace_root.is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "eos_workspace_root is required".to_owned(),
            ));
        }
        if !std::path::Path::new(workspace_root).is_absolute() {
            return Err(IsolatedError::InvalidArgument(format!(
                "eos_workspace_root must be absolute: {workspace_root}"
            )));
        }
        Ok(workspace_root.to_owned())
    }

    /// Exit the isolated workspace for `agent_id`.
    ///
    /// Tears down namespace/network/cgroup, releases the lease, and DISCARDS
    /// the upperdir (no publish).
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when `agent_id` is invalid or no isolated
    /// workspace is open for the agent.
    pub fn exit(
        &mut self,
        agent_id: &AgentId,
        grace_s: Option<f64>,
    ) -> Result<Value, IsolatedError> {
        if agent_id.0.trim().is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "agent_id is required".to_owned(),
            ));
        }
        let Some(handle_id) = self.by_agent.remove(agent_id) else {
            return Err(IsolatedError::NotOpen);
        };
        let Some(handle) = self.handles.remove(&handle_id) else {
            return Err(IsolatedError::NotOpen);
        };
        let timer = Instant::now();
        let upperdir_bytes = directory_file_bytes(&handle.upperdir);
        let (inspection, phases_ms) =
            self.teardown_handle(&handle, grace_s.unwrap_or(self.caps.exit_grace_s));
        let _ = self.persist_handles();
        let lifetime_s = (monotonic_seconds() - handle.created_at).max(0.0);
        let total_ms = timer.elapsed().as_secs_f64() * 1000.0;
        let _ = self.audit.emit(
            "sandbox_isolated_workspace_exit",
            json!({
                "workspace_handle_id": handle.workspace_handle_id.0,
                "agent_id": agent_id.0,
                "reason": "explicit",
                "lifetime_s": lifetime_s,
                "upperdir_bytes_discarded": upperdir_bytes,
                "total_ms": total_ms,
                "phases_ms": phases_ms.clone(),
                "scratch_removed": !handle.scratch_dir.exists(),
                "inspection": inspection,
            }),
        );
        Ok(json!({
            "success": true,
            "evicted_upperdir_bytes": upperdir_bytes,
            "lifetime_s": lifetime_s,
            "total_ms": total_ms,
            "phases_ms": phases_ms,
            "inspection": inspection,
        }))
    }

    /// Evict idle handles whose last activity exceeds the configured TTL.
    ///
    /// Agents listed in `active_agents` are skipped because the daemon still
    /// owns at least one live command session for them.
    pub fn ttl_sweep(&mut self, active_agents: &HashSet<String>) -> usize {
        if self.caps.ttl_s <= 0.0 {
            return 0;
        }
        let now = monotonic_seconds();
        let stale = self
            .handles
            .values()
            .filter(|handle| now - handle.last_activity > self.caps.ttl_s)
            .filter(|handle| !active_agents.contains(&handle.agent_id.0))
            .cloned()
            .collect::<Vec<_>>();
        let mut evicted = 0;
        for handle in stale {
            let Ok(stats) = self.exit(&handle.agent_id, None) else {
                continue;
            };
            let upperdir_bytes = stats
                .get("evicted_upperdir_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let lifetime_s = stats
                .get("lifetime_s")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let total_ms = stats.get("total_ms").and_then(Value::as_f64).unwrap_or(0.0);
            let phases_ms = stats.get("phases_ms").cloned().unwrap_or_else(|| json!({}));
            let _ = self.audit.emit(
                "sandbox_isolated_workspace_evicted",
                json!({
                    "workspace_handle_id": handle.workspace_handle_id.0,
                    "agent_id": handle.agent_id.0,
                    "reason": "ttl",
                    "lifetime_s": lifetime_s,
                    "upperdir_bytes_discarded": upperdir_bytes,
                    "total_ms": total_ms,
                    "phases_ms": phases_ms,
                }),
            );
            evicted += 1;
        }
        evicted
    }

    /// Return a copy of the active handle for `agent_id`, if any.
    pub fn get_handle(&self, agent_id: &AgentId) -> Option<WorkspaceHandle> {
        self.by_agent
            .get(agent_id)
            .and_then(|handle_id| self.handles.get(handle_id))
            .cloned()
    }

    /// Return every agent with an open handle.
    pub fn list_open_agents(&self) -> Vec<String> {
        self.by_agent.keys().map(|agent| agent.0.clone()).collect()
    }

    /// Emit an isolated tool-call audit event for an active handle.
    pub fn record_tool_call(&mut self, agent_id: &AgentId, mut payload: Value) {
        let Some(handle_id) = self.by_agent.get(agent_id).cloned() else {
            return;
        };
        let Some(handle) = self.handles.get_mut(&handle_id) else {
            return;
        };
        handle.last_activity = monotonic_seconds();
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "workspace_handle_id".to_owned(),
                json!(handle.workspace_handle_id.0),
            );
            object.insert("agent_id".to_owned(), json!(agent_id.0));
        }
        let _ = self
            .audit
            .emit("sandbox_isolated_workspace_tool_call", payload);
    }

    /// Sweep naming-convention resources that no longer have persisted rows.
    ///
    /// Test reset and daemon startup call this before accepting new handles.
    /// On a fresh daemon there are no live handles, so every `eos-iws-*`
    /// resource left in the host namespace is an orphan candidate.
    pub fn reap_orphan_resources(&mut self) {
        self.reap_named_orphans();
    }
}
