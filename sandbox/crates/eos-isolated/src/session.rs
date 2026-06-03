//! The persistent private session: enter/exit lifecycle and control-plane ports.
//!
//! `IsolatedSession` owns the per-agent persistent workspace. `enter` acquires a
//! layer-stack snapshot/lease, allocates scratch (upper/work), wires the
//! namespace (ns-holder spawn -> ns FDs -> overlay mount -> DNS -> net-ready),
//! and persists the handle. `exit` tears down the namespace + network + cgroup,
//! releases the lease, and DISCARDS the upperdir (writes are captured for audit
//! only, never published). Daemon-side gates own active command-session quiescence
//! for the current Rust slice.
//! `// PORT backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py:39-260`

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::audit::AuditSink;
use crate::caps::{
    ResourceCaps, HANDLE_PREFIX, ISOLATED_WORKSPACE_ROOT, PERSISTED_HANDLES_SCHEMA_VERSION,
};
use crate::error::IsolatedError;
use crate::network::{IsolatedNetwork, VethAllocation};
use serde_json::{json, Value};

const HOST_BUDGET_FALLBACK_BYTES: u64 = 1_u64 << 62;
const KIB_BYTES: u64 = 1_024;

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
/// `// PORT backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py:66-83 — acquire_snapshot result usage`
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
/// `// PORT backend/src/sandbox/isolated_workspace/_control_plane/types.py:103-141 — IsolatedWorkspaceHandle`
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
    /// Mount target inside the namespace (`/testbed`).
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
/// `// PORT backend/src/sandbox/occ/layer_stack_adapter.py:31-67 — snapshot/lease half`
pub trait LayerStackSnapshotPort {
    /// Acquire a read snapshot + lease for `request_id`.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the layer-stack snapshot or lease cannot
    /// be acquired.
    // PORT backend/src/sandbox/occ/layer_stack_adapter.py:57 — acquire_snapshot
    fn acquire_snapshot(&self, request_id: &str) -> Result<SnapshotLease, IsolatedError>;

    /// Release the lease held by `lease_id`. Returns whether it was held.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the layer-stack lease release cannot be
    /// checked or completed.
    // PORT backend/src/sandbox/occ/layer_stack_adapter.py:66 — release_lease
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
/// `// PORT backend/src/sandbox/isolated_workspace/_control_plane/types.py:221-256 — NamespaceRuntimePort`
/// `// PORT backend/src/sandbox/isolated_workspace/_control_plane/namespace_runtime.py:65-301 — _KernelNamespaceRuntime`
pub trait NamespaceRuntimePort {
    /// Spawn `eosd ns-holder` under `unshare(--user --net --pid --mount ...)`,
    /// wait for the `ns-up` handshake token, and return its PID.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when holder launch or readiness signaling
    /// fails.
    // PORT backend/src/sandbox/isolated_workspace/_control_plane/namespace_runtime.py:79-116 — spawn_ns_holder (ns_holder.py handshake step 1)
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
    // PORT backend/src/sandbox/isolated_workspace/_control_plane/namespace_runtime.py:118-125 — open_ns_fds
    fn open_ns_fds(&self, holder_pid: i32) -> Result<HashMap<String, i32>, IsolatedError>;

    /// Mount the overlay inside the namespace (via `eosd ns-runner` setns helper).
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the setns overlay mount helper fails.
    // PORT backend/src/sandbox/isolated_workspace/_control_plane/namespace_runtime.py:127-165 — mount_overlay (setns_overlay_mount)
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
    // PORT backend/src/sandbox/isolated_workspace/_control_plane/namespace_runtime.py:167-199 — configure_dns (configure_dns_in_ns)
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
    // PORT backend/src/sandbox/isolated_workspace/_control_plane/namespace_runtime.py:201-214 — signal_net_ready (ns_holder.py handshake)
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
    // PORT backend/src/sandbox/isolated_workspace/_control_plane/namespace_runtime.py:216-219 — create_cgroup
    fn create_cgroup(&self, handle: &WorkspaceHandle) -> Result<PathBuf, IsolatedError>;

    /// SIGTERM (then SIGKILL after `grace_s`) the ns-holder and reap children.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when holder teardown fails.
    // PORT backend/src/sandbox/isolated_workspace/_control_plane/namespace_runtime.py:221-253 — kill_holder
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
    // PORT backend/src/sandbox/isolated_workspace/pipeline.py:220 — IsolatedPipeline.initialize
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
    // PORT backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py:39-130 — _WorkspaceHandleLifecycleMixin.enter
    pub fn enter(&mut self, agent_id: &AgentId) -> Result<WorkspaceHandle, IsolatedError> {
        if !self.caps.enabled {
            return Err(IsolatedError::FeatureDisabled);
        }
        if agent_id.0.trim().is_empty() {
            return Err(IsolatedError::InvalidArgument(
                "agent_id is required".to_owned(),
            ));
        }
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

        let snapshot = self
            .layer_stack
            .acquire_snapshot(&format!("isolated-{}", next_handle_id()))?;
        let workspace_handle_id = WorkspaceHandleId(next_handle_id());
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
            workspace_root: ISOLATED_WORKSPACE_ROOT.to_owned(),
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

    /// Exit the isolated workspace for `agent_id`.
    ///
    /// Tears down namespace/network/cgroup, releases the lease, and DISCARDS
    /// the upperdir (no publish).
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when `agent_id` is invalid or no isolated
    /// workspace is open for the agent.
    // PORT backend/src/sandbox/isolated_workspace/_control_plane/workspace_handle_lifecycle.py:207-260 — _WorkspaceHandleLifecycleMixin.exit
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

    fn session_scratch_root(&self) -> PathBuf {
        self.scratch_root.join("runtime").join("isolated-workspace")
    }

    fn persisted_handles_path(&self) -> PathBuf {
        self.session_scratch_root().join("manager.json")
    }

    fn persist_handles(&self) -> Result<(), IsolatedError> {
        let root = self.session_scratch_root();
        std::fs::create_dir_all(&root).map_err(|err| IsolatedError::SetupFailed {
            step: format!("manager_root: {err}"),
        })?;
        let handles: Vec<Value> = self
            .handles
            .values()
            .map(|handle| {
                json!({
                    "workspace_handle_id": handle.workspace_handle_id.0,
                    "agent_id": handle.agent_id.0,
                    "lease_id": handle.lease_id,
                    "manifest_version": handle.manifest_version,
                    "manifest_root_hash": handle.manifest_root_hash,
                    "workspace_root": handle.workspace_root,
                    "scratch_dir": handle.scratch_dir.to_string_lossy(),
                    "upperdir": handle.upperdir.to_string_lossy(),
                    "workdir": handle.workdir.to_string_lossy(),
                    "layer_paths": handle.layer_paths,
                    "holder_pid": handle.holder_pid,
                    "veth_host_name": handle.veth.as_ref().map(|veth| veth.host_name.as_str()),
                    "veth_ns_name": handle.veth.as_ref().map(|veth| veth.ns_name.as_str()),
                    "ns_ip": handle.veth.as_ref().map(|veth| veth.ns_ip.to_string()),
                    "cgroup_path": handle
                        .cgroup_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    "created_at": handle.created_at,
                    "last_activity": handle.last_activity,
                })
            })
            .collect();
        let payload = json!({
            "schema_version": PERSISTED_HANDLES_SCHEMA_VERSION,
            "handles": handles,
        });
        let path = self.persisted_handles_path();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&payload).map_err(|err| IsolatedError::SetupFailed {
                step: format!("manager_serialize: {err}"),
            })?,
        )
        .map_err(|err| IsolatedError::SetupFailed {
            step: format!("manager_write: {err}"),
        })?;
        std::fs::rename(tmp, path).map_err(|err| IsolatedError::SetupFailed {
            step: format!("manager_rename: {err}"),
        })?;
        Ok(())
    }

    fn reap_startup_orphans(&mut self) -> Result<(), IsolatedError> {
        let rows = self.read_persisted_handle_rows();
        self.handles.clear();
        self.by_agent.clear();
        for row in &rows {
            if let Some(ns_ip) = persisted_ipv4(row, "ns_ip") {
                let _ = self.network.reserve_persisted_ip(ns_ip);
            }
        }
        for row in &rows {
            self.reap_persisted_lease(row);
            self.reap_persisted_holder(row);
            self.reap_persisted_veth(row);
            self.reap_persisted_cgroup(row);
            self.reap_persisted_scratch(row);
        }
        self.reap_named_orphans();
        self.persist_handles()
    }

    /// Sweep naming-convention resources that no longer have persisted rows.
    ///
    /// Test reset and daemon startup call this before accepting new handles.
    /// On a fresh daemon there are no live handles, so every `eos-iws-*`
    /// resource left in the host namespace is an orphan candidate.
    pub fn reap_orphan_resources(&mut self) {
        self.reap_named_orphans();
    }

    fn read_persisted_handle_rows(&self) -> Vec<Value> {
        let Ok(raw) = std::fs::read(self.persisted_handles_path()) else {
            return Vec::new();
        };
        let Ok(payload) = serde_json::from_slice::<Value>(&raw) else {
            return Vec::new();
        };
        if payload.get("schema_version").and_then(Value::as_u64)
            != Some(u64::from(PERSISTED_HANDLES_SCHEMA_VERSION))
        {
            return Vec::new();
        }
        payload
            .get("handles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn reap_persisted_lease(&self, row: &Value) {
        let Some(lease_id) = persisted_string(row, "lease_id") else {
            return;
        };
        let timer = Instant::now();
        let result = self.layer_stack.release_lease(&lease_id);
        let mut extra = vec![("released", json!(result.as_ref().copied().unwrap_or(false)))];
        if let Err(error) = result {
            extra.push(("error", json!(error.to_string())));
        }
        self.emit_gc_orphan("lease", lease_id, timer, &extra);
    }

    fn reap_persisted_holder(&self, row: &Value) {
        let Some(holder_pid) = persisted_i32(row, "holder_pid") else {
            return;
        };
        if holder_pid <= 0 {
            return;
        }
        let timer = Instant::now();
        let result = self
            .runtime
            .kill_holder(holder_pid, self.caps.exit_grace_s.max(0.0));
        let mut extra = Vec::new();
        if let Err(error) = result {
            extra.push(("error", json!(error.to_string())));
        }
        self.emit_gc_orphan("holder", holder_pid.to_string(), timer, &extra);
    }

    fn reap_persisted_veth(&mut self, row: &Value) {
        let Some(host_name) = persisted_string(row, "veth_host_name") else {
            return;
        };
        let Some(ns_name) = persisted_string(row, "veth_ns_name") else {
            return;
        };
        let Some(ns_ip) = persisted_ipv4(row, "ns_ip") else {
            return;
        };
        let allocation = VethAllocation {
            host_name: host_name.clone(),
            ns_name,
            ns_ip,
        };
        let timer = Instant::now();
        self.network.teardown_veth(&allocation);
        let _ = self.network.reserve_persisted_ip(ns_ip);
        self.emit_gc_orphan("veth", host_name, timer, &[]);
    }

    fn reap_persisted_cgroup(&self, row: &Value) {
        let Some(cgroup_path) = persisted_path(row, "cgroup_path") else {
            return;
        };
        if !cgroup_path.exists() {
            return;
        }
        let timer = Instant::now();
        kill_cgroup_pids(&cgroup_path);
        let remove_result = std::fs::remove_dir(&cgroup_path);
        let mut extra = Vec::new();
        if let Err(error) = remove_result {
            extra.push(("error", json!(error.to_string())));
        }
        self.emit_gc_orphan("cgroup", path_identifier(&cgroup_path), timer, &extra);
    }

    fn reap_persisted_scratch(&self, row: &Value) {
        let Some(scratch_dir) = persisted_path(row, "scratch_dir") else {
            return;
        };
        if !scratch_dir.exists() {
            return;
        }
        let timer = Instant::now();
        let remove_result = std::fs::remove_dir_all(&scratch_dir);
        let mut extra = Vec::new();
        if let Err(error) = remove_result {
            extra.push(("error", json!(error.to_string())));
        }
        self.emit_gc_orphan("scratch", path_identifier(&scratch_dir), timer, &extra);
    }

    fn reap_named_orphans(&mut self) {
        self.reap_named_veth_orphans();
        self.reap_named_cgroup_orphans();
        self.reap_named_scratch_orphans();
    }

    fn reap_named_veth_orphans(&mut self) {
        let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(HANDLE_PREFIX) {
                continue;
            }
            let timer = Instant::now();
            self.network.teardown_host_veth(&name);
            self.emit_gc_orphan("veth", name, timer, &[]);
        }
    }

    fn reap_named_cgroup_orphans(&self) {
        let Ok(entries) = std::fs::read_dir("/sys/fs/cgroup") else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(HANDLE_PREFIX) || !path.is_dir() {
                continue;
            }
            let timer = Instant::now();
            kill_cgroup_pids(&path);
            let remove_result = std::fs::remove_dir(&path);
            let mut extra = Vec::new();
            if let Err(error) = remove_result {
                extra.push(("error", json!(error.to_string())));
            }
            self.emit_gc_orphan("cgroup", name, timer, &extra);
        }
    }

    fn reap_named_scratch_orphans(&self) {
        let Ok(entries) = std::fs::read_dir(self.session_scratch_root()) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "manager.json" || !path.is_dir() {
                continue;
            }
            let timer = Instant::now();
            let remove_result = std::fs::remove_dir_all(&path);
            let mut extra = Vec::new();
            if let Err(error) = remove_result {
                extra.push(("error", json!(error.to_string())));
            }
            self.emit_gc_orphan("scratch", name, timer, &extra);
        }
    }

    fn emit_gc_orphan(
        &self,
        kind: &str,
        identifier: String,
        timer: Instant,
        extra: &[(&str, Value)],
    ) {
        let total_ms = timer.elapsed().as_secs_f64() * 1000.0;
        let mut payload = json!({
            "kind": kind,
            "identifier": identifier,
            "total_ms": total_ms,
            "phases_ms": {"reap": total_ms},
        });
        if let Some(object) = payload.as_object_mut() {
            for (key, value) in extra {
                object.insert((*key).to_owned(), value.clone());
            }
        }
        let _ = self
            .audit
            .emit("sandbox_isolated_workspace_gc_orphan", payload);
    }

    fn check_host_capacity(&self) -> Result<(), IsolatedError> {
        check_host_capacity_against_budget(
            self.handles.len(),
            self.caps.upperdir_bytes,
            host_capacity_budget_bytes(self.caps.memavail_fraction),
        )
    }

    fn wire_handle(
        &mut self,
        handle: &mut WorkspaceHandle,
    ) -> Result<HashMap<String, f64>, IsolatedError> {
        let mut phases_ms = HashMap::new();
        let mut phase_start = Instant::now();
        handle.holder_pid = self
            .runtime
            .spawn_ns_holder(handle, self.caps.setup_timeout_s)?;
        phases_ms.insert(
            "spawn_ns_holder".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        phase_start = Instant::now();
        handle.ns_fds = self.runtime.open_ns_fds(handle.holder_pid)?;
        phases_ms.insert(
            "open_ns_fds".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        phase_start = Instant::now();
        self.network.initialize()?;
        maybe_inject_phase("install_veth")?;
        handle.veth = Some(
            self.network
                .install_veth(&handle.workspace_handle_id.0, handle.holder_pid)?,
        );
        phases_ms.insert(
            "install_veth".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        phase_start = Instant::now();
        maybe_inject_phase("mount_overlay")?;
        self.runtime.mount_overlay(handle, &handle.layer_paths)?;
        phases_ms.insert(
            "mount_overlay".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        phase_start = Instant::now();
        maybe_inject_phase("configure_dns")?;
        let _dns_fallback_applied = self
            .runtime
            .configure_dns(handle, &self.caps.fallback_dns)?;
        phases_ms.insert(
            "configure_dns".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        // signal_net_ready runs UNTIMED between the configure_dns and
        // create_cgroup phase measures, matching Python
        // workspace_handle_lifecycle.py:189 (called outside any t.measure block)
        // so the configure_dns phase budget is not inflated by the net-ready wait.
        self.runtime
            .signal_net_ready(handle, self.caps.setup_timeout_s)?;
        phase_start = Instant::now();
        let cgroup_path = self.runtime.create_cgroup(handle)?;
        phases_ms.insert(
            "create_cgroup".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        if !cgroup_path.as_os_str().is_empty() {
            handle.cgroup_path = Some(cgroup_path);
        }
        Ok(phases_ms)
    }

    fn rollback_partial(&mut self, handle: &WorkspaceHandle) {
        close_handle_fds(handle);
        if let Some(veth) = handle.veth.as_ref() {
            self.network.teardown_veth(veth);
        }
        if handle.holder_pid > 0 {
            let _ = self.runtime.kill_holder(handle.holder_pid, 1.0);
        }
        let _ = std::fs::remove_dir_all(&handle.scratch_dir);
    }

    fn teardown_handle(
        &mut self,
        handle: &WorkspaceHandle,
        grace_s: f64,
    ) -> (Value, HashMap<String, f64>) {
        let mut phases_ms = HashMap::new();
        let phase_start = Instant::now();
        let holder_kill_error = if handle.holder_pid > 0 {
            self.runtime
                .kill_holder(handle.holder_pid, grace_s)
                .err()
                .map(|err| err.to_string())
        } else {
            None
        };
        phases_ms.insert(
            "kill_holder".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        let phase_start = Instant::now();
        close_handle_fds(handle);
        let _close_fds_ms = phase_start.elapsed().as_secs_f64() * 1000.0;
        let phase_start = Instant::now();
        if let Some(veth) = handle.veth.as_ref() {
            self.network.teardown_veth(veth);
        }
        phases_ms.insert(
            "teardown_veth".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        let phase_start = Instant::now();
        let lease_released = self.layer_stack.release_lease(&handle.lease_id).ok();
        phases_ms.insert(
            "release_snapshot".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        let phase_start = Instant::now();
        if let Some(cgroup_path) = handle.cgroup_path.as_ref() {
            let _ = std::fs::remove_dir(cgroup_path);
        }
        phases_ms.insert(
            "cgroup_rmdir".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        let phase_start = Instant::now();
        let _ = std::fs::remove_dir_all(&handle.scratch_dir);
        phases_ms.insert(
            "rmtree_scratch".to_owned(),
            phase_start.elapsed().as_secs_f64() * 1000.0,
        );
        let cgroup_exists_after = handle.cgroup_path.as_ref().map(|path| path.exists());
        let inspection = json!({
            "handle_registered_after": self.handles.contains_key(&handle.workspace_handle_id),
            "agent_registered_after": self.by_agent.contains_key(&handle.agent_id),
            "open_handle_count_after": self.handles.len(),
            "open_agent_count_after": self.by_agent.len(),
            "lease_released": lease_released,
            "active_leases_after": self.layer_stack.active_lease_count().ok().flatten(),
            "holder_pid": handle.holder_pid,
            "holder_kill_error": holder_kill_error,
            "ns_fd_count": handle.ns_fds.len(),
            "readiness_fd_was_open": handle.readiness_fd >= 0,
            "control_fd_was_open": handle.control_fd >= 0,
            "veth_host_name": handle.veth.as_ref().map(|veth| veth.host_name.as_str()),
            "veth_ns_name": handle.veth.as_ref().map(|veth| veth.ns_name.as_str()),
            "cgroup_path": handle
                .cgroup_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "cgroup_exists_after": cgroup_exists_after,
            "scratch_dir": handle.scratch_dir.to_string_lossy(),
            "scratch_exists_after": handle.scratch_dir.exists(),
            "upperdir_exists_after": handle.upperdir.exists(),
            "workdir_exists_after": handle.workdir.exists(),
            "mountinfo_reference_count_after": mountinfo_reference_count(&[
                &handle.scratch_dir,
                &handle.upperdir,
                &handle.workdir,
            ]),
        });
        (inspection, phases_ms)
    }
}

fn persisted_string(row: &Value, key: &str) -> Option<String> {
    let value = row.get(key)?.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.to_owned())
}

fn persisted_i32(row: &Value, key: &str) -> Option<i32> {
    let value = row.get(key)?.as_i64()?;
    i32::try_from(value).ok()
}

fn persisted_ipv4(row: &Value, key: &str) -> Option<Ipv4Addr> {
    persisted_string(row, key)?.parse().ok()
}

fn persisted_path(row: &Value, key: &str) -> Option<PathBuf> {
    persisted_string(row, key).map(PathBuf::from)
}

fn path_identifier(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map_or_else(|| path.to_string_lossy().into_owned(), ToOwned::to_owned)
}

fn kill_cgroup_pids(cgroup_path: &Path) {
    let kill_file = cgroup_path.join("cgroup.kill");
    if kill_file.exists() {
        let _ = std::fs::write(kill_file, "1\n");
    }
}

fn maybe_inject_phase(phase: &str) -> Result<(), IsolatedError> {
    if let Some(target) = env_trimmed("EOS_ISOLATED_WORKSPACE_TEST_HANG_AT") {
        if phase_matches(&target, phase) {
            return Err(IsolatedError::SetupTimeout { step: target });
        }
    }
    if let Some(target) = env_trimmed("EOS_ISOLATED_WORKSPACE_TEST_FAIL_AT") {
        if phase_matches(&target, phase) {
            return Err(IsolatedError::SetupFailed { step: target });
        }
    }
    if let Some(delays) = env_trimmed("EOS_ISOLATED_WORKSPACE_TEST_PHASE_DELAY") {
        for spec in delays.split(',') {
            let Some((target, delay_ms)) = spec.split_once(':') else {
                continue;
            };
            if !phase_matches(target, phase) {
                continue;
            }
            let delay_ms = delay_ms.trim().trim_end_matches("ms").trim();
            let Ok(delay_ms) = delay_ms.parse::<f64>() else {
                continue;
            };
            if delay_ms.is_finite() && delay_ms > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(delay_ms / 1000.0));
            }
        }
    }
    Ok(())
}

fn phase_matches(target: &str, phase: &str) -> bool {
    let target = target.trim();
    target == phase || matches!((target, phase), ("overlay_mount", "mount_overlay"))
}

fn env_trimmed(key: &str) -> Option<String> {
    let value = std::env::var(key).ok()?.trim().to_owned();
    if value.is_empty() {
        return None;
    }
    Some(value)
}

fn close_handle_fds(handle: &WorkspaceHandle) {
    for fd in handle.ns_fds.values().copied() {
        if fd >= 0 {
            let _ = nix::unistd::close(fd);
        }
    }
    for fd in [handle.readiness_fd, handle.control_fd] {
        if fd >= 0 {
            let _ = nix::unistd::close(fd);
        }
    }
}

fn next_handle_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "{:016x}{:04x}",
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn monotonic_seconds() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn directory_file_bytes(path: &Path) -> u64 {
    let mut total = 0_u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            total = total.saturating_add(directory_file_bytes(&path));
        }
    }
    total
}

fn check_host_capacity_against_budget(
    open_handles: usize,
    upperdir_bytes: u64,
    budget_bytes: u64,
) -> Result<(), IsolatedError> {
    let required_bytes = required_host_capacity_bytes(open_handles, upperdir_bytes);
    if required_bytes > budget_bytes {
        return Err(IsolatedError::HostRamPressure {
            required_bytes,
            budget_bytes,
        });
    }
    Ok(())
}

fn required_host_capacity_bytes(open_handles: usize, upperdir_bytes: u64) -> u64 {
    u64::try_from(open_handles)
        .unwrap_or(u64::MAX)
        .saturating_add(1)
        .saturating_mul(upperdir_bytes)
}

fn host_capacity_budget_bytes(memavail_fraction: f64) -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|meminfo| parse_memavailable_kib(&meminfo))
        .map_or(HOST_BUDGET_FALLBACK_BYTES, |memavailable_kib| {
            host_capacity_budget_bytes_from_memavailable_kib(memavailable_kib, memavail_fraction)
        })
}

fn parse_memavailable_kib(meminfo: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix("MemAvailable:")?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

fn host_capacity_budget_bytes_from_memavailable_kib(
    memavailable_kib: u64,
    memavail_fraction: f64,
) -> u64 {
    let memavailable_bytes = memavailable_kib.saturating_mul(KIB_BYTES);
    f64_floor_to_u64_saturating(u64_to_f64_lossy(memavailable_bytes) * memavail_fraction)
}

fn f64_floor_to_u64_saturating(value: f64) -> u64 {
    if !value.is_finite() {
        return if value.is_sign_positive() {
            u64::MAX
        } else {
            0
        };
    }
    if value <= 0.0 {
        return 0;
    }
    let floored = value.floor();
    if floored >= u64_to_f64_lossy(u64::MAX) {
        return u64::MAX;
    }
    format!("{floored:.0}").parse().unwrap_or(u64::MAX)
}

fn u64_to_f64_lossy(value: u64) -> f64 {
    const U32_FACTOR: f64 = 4_294_967_296.0;
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high).mul_add(U32_FACTOR, f64::from(low))
}

fn mountinfo_reference_count(paths: &[&Path]) -> Option<usize> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let needles = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    Some(
        mountinfo
            .lines()
            .filter(|line| needles.iter().any(|needle| line.contains(needle)))
            .count(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_memavailable_from_proc_meminfo() {
        let meminfo = "MemTotal:       1024 kB\nMemAvailable:    2048 kB\n";
        assert_eq!(parse_memavailable_kib(meminfo), Some(2_048));
    }

    #[test]
    fn host_capacity_budget_matches_python_floor() {
        assert_eq!(
            host_capacity_budget_bytes_from_memavailable_kib(1_001, 0.5),
            512_512
        );
    }

    #[test]
    fn host_capacity_required_saturates() {
        assert_eq!(required_host_capacity_bytes(usize::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn host_capacity_rejects_when_required_exceeds_budget() -> Result<(), Box<dyn std::error::Error>>
    {
        let error = match check_host_capacity_against_budget(2, 10, 29) {
            Ok(()) => return Err("expected host RAM pressure rejection".into()),
            Err(error) => error,
        };
        let (required_bytes, budget_bytes) = match error {
            IsolatedError::HostRamPressure {
                required_bytes,
                budget_bytes,
            } => (required_bytes, budget_bytes),
            other => return Err(format!("expected host RAM pressure error, got {other}").into()),
        };
        assert_eq!(required_bytes, 30);
        assert_eq!(budget_bytes, 29);
        Ok(())
    }
}
