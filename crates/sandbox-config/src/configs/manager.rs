//! Typed schema for the Docker manager-runtime section of the gateway config.
//!
//! The gateway loads this section only under `--backend docker`; it stays an
//! optional root section so existing daemon configs continue to load.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::Deserialize;

use crate::configs::validate::{
    require_absolute, require_f64_gt, require_non_empty, require_u64_at_least,
    require_unix_absolute, require_usize_at_least, ConfigFieldError,
};
use crate::configs::{daemon::DaemonConfig, runtime::RuntimeConfig};

/// Host-side caps for the `export_changes` apply path (`manager.export`),
/// the sole tuning path since the env side channels retired; the gateway
/// injects these into the manager applier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagerExportConfig {
    /// Compressed byte cap for one export delivery stream.
    pub max_stream_bytes: u64,
    /// Decompressed byte cap guarding against zstd bombs.
    pub max_decompressed_bytes: u64,
    /// Entry-count cap guarding against archive entry bombs.
    pub max_apply_entries: u64,
}

impl Default for ManagerExportConfig {
    fn default() -> Self {
        Self {
            max_stream_bytes: 2 * 1024 * 1024 * 1024,
            max_decompressed_bytes: 8 * 1024 * 1024 * 1024,
            max_apply_entries: 1_000_000,
        }
    }
}

impl ManagerExportConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates export apply policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_u64_at_least(self.max_stream_bytes, 1, "manager.export.max_stream_bytes")?;
        require_u64_at_least(
            self.max_decompressed_bytes,
            1,
            "manager.export.max_decompressed_bytes",
        )?;
        require_u64_at_least(
            self.max_apply_entries,
            1,
            "manager.export.max_apply_entries",
        )
    }
}

/// Concurrency and per-daemon deadline for the `observability_snapshot`
/// fan-out (`manager.observability_snapshot`); the gateway injects these into
/// the manager services.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagerObservabilitySnapshotConfig {
    /// Daemon snapshot requests issued concurrently per aggregation wave.
    pub max_concurrent_requests: usize,
    /// Per-daemon snapshot request deadline.
    pub timeout_ms: u64,
}

impl Default for ManagerObservabilitySnapshotConfig {
    fn default() -> Self {
        Self {
            max_concurrent_requests: 8,
            timeout_ms: 1_500,
        }
    }
}

impl ManagerObservabilitySnapshotConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates snapshot aggregation policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_usize_at_least(
            self.max_concurrent_requests,
            1,
            "manager.observability_snapshot.max_concurrent_requests",
        )?;
        require_u64_at_least(
            self.timeout_ms,
            1,
            "manager.observability_snapshot.timeout_ms",
        )
    }
}

/// Ready/stop deadlines for the local-process daemon installer
/// (`manager.local_daemon`); polls stay hardcoded cadence.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagerLocalDaemonConfig {
    pub ready_timeout_s: f64,
    pub stop_timeout_s: f64,
}

impl Default for ManagerLocalDaemonConfig {
    fn default() -> Self {
        Self {
            ready_timeout_s: 2.0,
            stop_timeout_s: 2.0,
        }
    }
}

impl ManagerLocalDaemonConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates local daemon install policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_f64_gt(
            self.ready_timeout_s,
            0.0,
            "manager.local_daemon.ready_timeout_s",
        )?;
        require_f64_gt(
            self.stop_timeout_s,
            0.0,
            "manager.local_daemon.stop_timeout_s",
        )
    }
}

pub const DEFAULT_CONTAINER_WORKSPACE_ROOT: &str = "/workspace";
pub const DEFAULT_CONTAINER_DAEMON_BINARY_PATH: &str = "/eos/bin/sandbox-daemon";
pub const DEFAULT_CONTAINER_DAEMON_CONFIG_PATH: &str = "/eos/config/daemon.yml";
pub const DEFAULT_DAEMON_PORT: u16 = 7000;
pub const DEFAULT_DAEMON_HTTP_PORT: u16 = 7001;
pub const DEFAULT_READINESS_TIMEOUT_MS: u64 = 60_000;
pub const DEFAULT_GATEWAY_INSTANCE_ID: &str = "eos-gateway";
pub const STANDARD_RESOURCE_PROFILE: &str = "standard";
pub const BUILD_HEAVY_RESOURCE_PROFILE: &str = "build-heavy";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerResourceProfile {
    pub nano_cpus: i64,
    pub memory_high_bytes: i64,
    pub memory_max_bytes: i64,
    #[serde(default)]
    pub workload_memory_high_bytes: Option<i64>,
    #[serde(default)]
    pub workload_memory_max_bytes: Option<i64>,
    pub pids_max: i64,
    pub daemon_runtime_profile: String,
    pub separate_workload_cgroup: bool,
}

impl DockerResourceProfile {
    #[must_use]
    pub fn resolved_workload_memory_high_bytes(&self) -> i64 {
        self.workload_memory_high_bytes
            .unwrap_or(self.memory_high_bytes)
    }

    #[must_use]
    pub fn resolved_workload_memory_max_bytes(&self) -> i64 {
        self.workload_memory_max_bytes
            .unwrap_or(self.memory_high_bytes)
    }

    #[must_use]
    pub fn control_plane_pids_reserve(&self) -> i64 {
        let profile_reserve = match self.daemon_runtime_profile.as_str() {
            BUILD_HEAVY_RESOURCE_PROFILE => 64,
            _ => 32,
        };
        profile_reserve.min((self.pids_max / 8).max(8))
    }

    #[must_use]
    pub fn workload_pids_max(&self) -> i64 {
        self.pids_max
            .saturating_sub(self.control_plane_pids_reserve())
    }

    fn validate(&self, name: &str) -> Result<(), ConfigFieldError> {
        const FIELD: &str = "manager.docker.resource_profiles";
        if self.nano_cpus <= 0 {
            return Err(ConfigFieldError::new(
                FIELD,
                format!("profile `{name}` nano_cpus must be greater than zero"),
            ));
        }
        if self.memory_high_bytes <= 0 {
            return Err(ConfigFieldError::new(
                FIELD,
                format!("profile `{name}` memory_high_bytes must be greater than zero"),
            ));
        }
        if self.memory_max_bytes <= 0 {
            return Err(ConfigFieldError::new(
                FIELD,
                format!("profile `{name}` memory_max_bytes must be greater than zero"),
            ));
        }
        if self.memory_high_bytes > self.memory_max_bytes {
            return Err(ConfigFieldError::new(
                FIELD,
                format!("profile `{name}` memory_high_bytes must not exceed memory_max_bytes"),
            ));
        }
        let workload_memory_high_bytes = self.resolved_workload_memory_high_bytes();
        let workload_memory_max_bytes = self.resolved_workload_memory_max_bytes();
        if workload_memory_high_bytes <= 0 {
            return Err(ConfigFieldError::new(
                FIELD,
                format!("profile `{name}` workload_memory_high_bytes must be greater than zero"),
            ));
        }
        if workload_memory_max_bytes <= 0 {
            return Err(ConfigFieldError::new(
                FIELD,
                format!("profile `{name}` workload_memory_max_bytes must be greater than zero"),
            ));
        }
        if workload_memory_high_bytes > workload_memory_max_bytes {
            return Err(ConfigFieldError::new(
                FIELD,
                format!(
                    "profile `{name}` workload_memory_high_bytes must not exceed workload_memory_max_bytes"
                ),
            ));
        }
        if workload_memory_max_bytes > self.memory_max_bytes {
            return Err(ConfigFieldError::new(
                FIELD,
                format!(
                    "profile `{name}` workload_memory_max_bytes must not exceed memory_max_bytes"
                ),
            ));
        }
        if self.pids_max <= 0 {
            return Err(ConfigFieldError::new(
                FIELD,
                format!("profile `{name}` pids_max must be greater than zero"),
            ));
        }
        if self.separate_workload_cgroup && self.memory_high_bytes >= self.memory_max_bytes {
            return Err(ConfigFieldError::new(
                FIELD,
                format!(
                    "profile `{name}` memory_high_bytes must be below memory_max_bytes when workload separation is enabled"
                ),
            ));
        }
        if !matches!(
            self.daemon_runtime_profile.as_str(),
            STANDARD_RESOURCE_PROFILE | BUILD_HEAVY_RESOURCE_PROFILE
        ) {
            return Err(ConfigFieldError::new(
                FIELD,
                format!(
                    "profile `{name}` daemon_runtime_profile must be `standard` or `build-heavy`"
                ),
            ));
        }
        if self.separate_workload_cgroup && self.workload_pids_max() <= 0 {
            return Err(ConfigFieldError::new(
                FIELD,
                format!(
                    "profile `{name}` pids_max must leave a positive workload allowance after the control-plane reserve"
                ),
            ));
        }
        Ok(())
    }
}

fn default_resource_profile_name() -> String {
    STANDARD_RESOURCE_PROFILE.to_owned()
}

fn default_resource_profiles() -> BTreeMap<String, DockerResourceProfile> {
    BTreeMap::from([
        (
            STANDARD_RESOURCE_PROFILE.to_owned(),
            DockerResourceProfile {
                nano_cpus: 1_000_000_000,
                memory_high_bytes: 384 * 1024 * 1024,
                memory_max_bytes: 512 * 1024 * 1024,
                workload_memory_high_bytes: None,
                workload_memory_max_bytes: None,
                pids_max: 256,
                daemon_runtime_profile: STANDARD_RESOURCE_PROFILE.to_owned(),
                separate_workload_cgroup: true,
            },
        ),
        (
            BUILD_HEAVY_RESOURCE_PROFILE.to_owned(),
            DockerResourceProfile {
                nano_cpus: 4_000_000_000,
                memory_high_bytes: 3 * 1024 * 1024 * 1024,
                memory_max_bytes: 4 * 1024 * 1024 * 1024,
                workload_memory_high_bytes: None,
                workload_memory_max_bytes: None,
                pids_max: 1024,
                daemon_runtime_profile: BUILD_HEAVY_RESOURCE_PROFILE.to_owned(),
                separate_workload_cgroup: true,
            },
        ),
    ])
}

/// Root `manager` section. Holds one backend sub-section; only `docker` exists
/// in v1, and it stays optional so the gateway's default `none` backend needs no
/// config at all.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagerConfig {
    /// Host path of the sandbox registry JSON snapshot. When set, the gateway
    /// persists every registry mutation there and reloads it on restart,
    /// reconciling against the containers the runtime actually recovers.
    /// `None` keeps the registry in process memory only.
    pub registry_path: Option<PathBuf>,
    /// Optional host directory allowlist for sandbox creation. When set,
    /// `create_sandbox` must use a directory below one of these roots.
    /// Omitting it preserves unrestricted API behavior.
    pub workspace_roots: Option<Vec<PathBuf>>,
    pub export: ManagerExportConfig,
    pub observability_snapshot: ManagerObservabilitySnapshotConfig,
    pub local_daemon: ManagerLocalDaemonConfig,
    pub docker: Option<DockerRuntimeConfig>,
}

impl ManagerConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates manager policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        if let Some(roots) = &self.workspace_roots {
            if roots.is_empty() {
                return Err(ConfigFieldError::new(
                    "manager.workspace_roots",
                    "must contain at least one directory",
                ));
            }
            for root in roots {
                require_absolute(root, "manager.workspace_roots")?;
            }
        }
        self.export.validate()?;
        self.observability_snapshot.validate()?;
        self.local_daemon.validate()?;
        if let Some(docker) = &self.docker {
            docker.validate()?;
        }
        Ok(())
    }
}

/// Cgroup namespace presented to a sandbox container.
///
/// The default private namespace protects the host hierarchy. A trusted
/// physical-validation environment can explicitly opt into `host` when the
/// daemon must create its managed workload cgroup and Docker's private
/// namespace exposes `/sys/fs/cgroup` read-only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockerCgroupNamespaceMode {
    #[default]
    Private,
    Host,
}

/// A server-configured Docker named volume that outlives individual sandboxes.
///
/// The volume name and target are policy, not caller input.  This permits an
/// immutable prepared fixture to be shared without reusing a sandbox-owned
/// workspace, lease, or writable upper directory.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerPersistentVolumeMount {
    pub name: String,
    pub target: PathBuf,
    #[serde(default)]
    pub readonly: bool,
}

impl DockerPersistentVolumeMount {
    fn validate(&self) -> Result<(), ConfigFieldError> {
        require_non_empty(&self.name, "manager.docker.persistent_volume_mounts")?;
        if !self
            .name
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ConfigFieldError::new(
                "manager.docker.persistent_volume_mounts",
                format!("volume name `{}` contains an unsafe character", self.name),
            ));
        }
        require_unix_absolute(
            &self.target,
            "manager.docker.persistent_volume_mounts.target",
        )?;
        if self.target == PathBuf::from("/") {
            return Err(ConfigFieldError::new(
                "manager.docker.persistent_volume_mounts.target",
                "must not be the container root",
            ));
        }
        Ok(())
    }
}

/// Configuration for the Docker-backed sandbox runtime + daemon installer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerRuntimeConfig {
    /// Explicit Docker Engine endpoint; when `None`, connect with local defaults
    /// (honoring `DOCKER_HOST`).
    pub docker_endpoint: Option<String>,
    /// Host path to the Linux `sandbox-daemon` binary uploaded into containers.
    pub daemon_binary_path: PathBuf,
    /// Host path to the daemon config YAML uploaded into containers.
    pub daemon_config_yaml_path: PathBuf,
    /// Container path where the daemon binary is uploaded.
    pub container_daemon_binary_path: PathBuf,
    /// Container path where the daemon config YAML is uploaded.
    pub container_daemon_config_yaml_path: PathBuf,
    /// Default base image when `create_sandbox` is invoked without one.
    pub default_image: Option<String>,
    /// Linux container path the host workspace root is bind-mounted to.
    pub container_workspace_root: PathBuf,
    /// Explicit platform (for example `linux/amd64`) for image/container create.
    pub platform: Option<String>,
    /// Whether containers run with Docker `--privileged`. `false` (the default)
    /// runs the de-privileged boundary: `cap_add SYS_ADMIN,NET_ADMIN`,
    /// Docker's default seccomp profile, and `no-new-privileges` — the minimal
    /// set live-proven for namespace/overlay/network setup. `true` is the
    /// legacy escape hatch.
    pub privileged: bool,
    /// Container TCP port the daemon listens on (published to a host port).
    pub daemon_port: u16,
    /// Container TCP port the daemon HTTP surface listens on (published to a
    /// separate host port, distinct from the JSON-line RPC `daemon_port`).
    pub daemon_http_port: u16,
    /// Identifies the owning gateway; recovery filters containers by this label.
    pub gateway_instance_id: String,
    /// Cgroup namespace assigned to sandbox containers. Defaults to a private
    /// namespace; `host` is an explicit trusted-environment exception for
    /// workload-cgroup-capable physical validation.
    pub cgroup_namespace_mode: DockerCgroupNamespaceMode,
    /// Readiness deadline for the authenticated daemon check.
    pub readiness_timeout_ms: u64,
    /// Docker Engine API connect timeout, in seconds.
    pub connect_timeout_s: u64,
    /// Grace Docker gives a container between SIGTERM and SIGKILL on stop,
    /// in seconds.
    pub stop_timeout_s: u64,
    /// Cadence of readiness probes under `readiness_timeout_ms`.
    pub readiness_poll_ms: u64,
    /// Inspect retries waiting for published host ports after container start.
    pub port_publish_attempts: u32,
    /// Delay between port-publish inspect retries.
    pub port_publish_retry_delay_ms: u64,
    /// Name of the CPU, memory, PID, and daemon-runtime profile applied to
    /// every sandbox created by this manager instance.
    pub resource_profile: String,
    /// Named, validated resource profiles available to this manager instance.
    pub resource_profiles: BTreeMap<String, DockerResourceProfile>,
    /// Legacy optional per-container memory cap in bytes. New deployments use
    /// the selected named resource profile.
    pub memory_bytes: Option<i64>,
    /// Legacy optional per-container CPU cap in nano-CPUs. New deployments use
    /// the selected named resource profile.
    pub nano_cpus: Option<i64>,
    /// Environment variables injected into every sandbox container, as a
    /// `name -> value` map. The Docker CLI injects proxy settings from
    /// `~/.docker/config.json` into containers it runs; the Engine API this
    /// runtime uses does not, so declare them here (for example `HTTP_PROXY`)
    /// to give sandboxes the same egress path.
    pub container_env: BTreeMap<String, String>,
    /// Server-configured named volumes that persist across sandbox cleanup.
    /// These are never selected by public operation arguments.
    pub persistent_volume_mounts: Vec<DockerPersistentVolumeMount>,
    /// Whether each sandbox receives its own lifecycle-owned layer-stack and
    /// workspace volumes. A dedicated fixture builder may disable these only
    /// while both roots are supplied by a configured persistent volume.
    pub sandbox_owned_workspace_volumes: bool,
    /// Opt in to copying the immutable shared-base tree into a writable
    /// persistent volume during sandbox creation. This is reserved for the
    /// trusted fixture builder: nested Docker mounts otherwise make the base
    /// disappear when that builder sandbox is destroyed.
    pub materialize_shared_base_into_persistent_volume: bool,
}

impl Default for DockerRuntimeConfig {
    fn default() -> Self {
        Self {
            docker_endpoint: None,
            daemon_binary_path: PathBuf::new(),
            daemon_config_yaml_path: PathBuf::new(),
            container_daemon_binary_path: PathBuf::from(DEFAULT_CONTAINER_DAEMON_BINARY_PATH),
            container_daemon_config_yaml_path: PathBuf::from(DEFAULT_CONTAINER_DAEMON_CONFIG_PATH),
            default_image: None,
            container_workspace_root: PathBuf::from(DEFAULT_CONTAINER_WORKSPACE_ROOT),
            platform: None,
            privileged: false,
            daemon_port: DEFAULT_DAEMON_PORT,
            daemon_http_port: DEFAULT_DAEMON_HTTP_PORT,
            gateway_instance_id: DEFAULT_GATEWAY_INSTANCE_ID.to_owned(),
            cgroup_namespace_mode: DockerCgroupNamespaceMode::Private,
            readiness_timeout_ms: DEFAULT_READINESS_TIMEOUT_MS,
            connect_timeout_s: 120,
            stop_timeout_s: 5,
            readiness_poll_ms: 250,
            // Docker Desktop can take several seconds to expose dynamic host
            // bindings under sustained sequential campaign load. Keep this
            // bounded (200 × 50 ms = 10 s) but do not fail a running daemon
            // merely because its published port has not appeared in 2 s.
            port_publish_attempts: 200,
            port_publish_retry_delay_ms: 50,
            resource_profile: default_resource_profile_name(),
            resource_profiles: default_resource_profiles(),
            memory_bytes: None,
            nano_cpus: None,
            container_env: BTreeMap::new(),
            persistent_volume_mounts: Vec::new(),
            sandbox_owned_workspace_volumes: true,
            materialize_shared_base_into_persistent_volume: false,
        }
    }
}

impl DockerRuntimeConfig {
    #[must_use]
    pub fn selected_resource_profile(&self) -> Option<&DockerResourceProfile> {
        self.resource_profiles.get(&self.resource_profile)
    }

    /// Validate that the daemon configuration uploaded for the selected
    /// resource profile has its exact worker count and stays within the
    /// profile's blocking, keepalive, connection, and command ceilings.
    ///
    /// This keeps `daemon_runtime_profile` from becoming descriptive metadata:
    /// selecting a profile with a mismatched daemon would otherwise defeat its
    /// worker, blocking-pool, connection, or command-admission budget.
    pub fn validate_daemon_runtime_profile(
        &self,
        daemon: &DaemonConfig,
        runtime: &RuntimeConfig,
    ) -> Result<(), ConfigFieldError> {
        let profile = self.selected_resource_profile().ok_or_else(|| {
            ConfigFieldError::new(
                "manager.docker.resource_profile",
                format!("unknown profile `{}`", self.resource_profile),
            )
        })?;
        let expected = match profile.daemon_runtime_profile.as_str() {
            STANDARD_RESOURCE_PROFILE => DaemonRuntimeProfileLimits {
                worker_threads: 2,
                max_blocking_threads: 8,
                blocking_thread_keep_alive_s: 5.0,
                max_concurrent_connections: 64,
                max_active_commands: 32,
            },
            BUILD_HEAVY_RESOURCE_PROFILE => DaemonRuntimeProfileLimits {
                worker_threads: 4,
                max_blocking_threads: 16,
                blocking_thread_keep_alive_s: 5.0,
                max_concurrent_connections: 128,
                max_active_commands: 64,
            },
            unknown => {
                return Err(ConfigFieldError::new(
                    "manager.docker.resource_profiles",
                    format!("unknown daemon runtime profile `{unknown}`"),
                ));
            }
        };
        require_profile_value(
            daemon.server.worker_threads,
            expected.worker_threads,
            "daemon.server.worker_threads",
            &profile.daemon_runtime_profile,
        )?;
        require_profile_at_most(
            daemon.server.max_blocking_threads,
            expected.max_blocking_threads,
            "daemon.server.max_blocking_threads",
            &profile.daemon_runtime_profile,
        )?;
        if daemon.server.blocking_thread_keep_alive_s > expected.blocking_thread_keep_alive_s {
            return Err(profile_ceiling_error(
                "daemon.server.blocking_thread_keep_alive_s",
                daemon.server.blocking_thread_keep_alive_s,
                expected.blocking_thread_keep_alive_s,
                &profile.daemon_runtime_profile,
            ));
        }
        require_profile_at_most(
            daemon.server.max_concurrent_connections,
            expected.max_concurrent_connections,
            "daemon.server.max_concurrent_connections",
            &profile.daemon_runtime_profile,
        )?;
        require_profile_at_most(
            runtime.command.max_active,
            expected.max_active_commands,
            "runtime.command.max_active",
            &profile.daemon_runtime_profile,
        )
    }

    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates Docker-runtime policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_non_empty(
            &self.daemon_binary_path.to_string_lossy(),
            "manager.docker.daemon_binary_path",
        )?;
        require_non_empty(
            &self.daemon_config_yaml_path.to_string_lossy(),
            "manager.docker.daemon_config_yaml_path",
        )?;
        require_unix_absolute(
            &self.container_daemon_binary_path,
            "manager.docker.container_daemon_binary_path",
        )?;
        require_unix_absolute(
            &self.container_daemon_config_yaml_path,
            "manager.docker.container_daemon_config_yaml_path",
        )?;
        require_unix_absolute(
            &self.container_workspace_root,
            "manager.docker.container_workspace_root",
        )?;
        require_non_empty(
            &self.gateway_instance_id,
            "manager.docker.gateway_instance_id",
        )?;
        require_u64_at_least(u64::from(self.daemon_port), 1, "manager.docker.daemon_port")?;
        require_u64_at_least(
            u64::from(self.daemon_http_port),
            1,
            "manager.docker.daemon_http_port",
        )?;
        require_u64_at_least(
            self.readiness_timeout_ms,
            1,
            "manager.docker.readiness_timeout_ms",
        )?;
        require_u64_at_least(
            self.connect_timeout_s,
            1,
            "manager.docker.connect_timeout_s",
        )?;
        require_u64_at_least(self.stop_timeout_s, 1, "manager.docker.stop_timeout_s")?;
        require_u64_at_least(
            self.readiness_poll_ms,
            1,
            "manager.docker.readiness_poll_ms",
        )?;
        require_u64_at_least(
            u64::from(self.port_publish_attempts),
            1,
            "manager.docker.port_publish_attempts",
        )?;
        require_u64_at_least(
            self.port_publish_retry_delay_ms,
            1,
            "manager.docker.port_publish_retry_delay_ms",
        )?;
        require_non_empty(&self.resource_profile, "manager.docker.resource_profile")?;
        if self.resource_profiles.is_empty() {
            return Err(ConfigFieldError::new(
                "manager.docker.resource_profiles",
                "must contain at least one named profile",
            ));
        }
        for (name, profile) in &self.resource_profiles {
            require_non_empty(name, "manager.docker.resource_profiles")?;
            profile.validate(name)?;
        }
        if self.selected_resource_profile().is_none() {
            return Err(ConfigFieldError::new(
                "manager.docker.resource_profile",
                format!("unknown profile `{}`", self.resource_profile),
            ));
        }
        let selected = self
            .selected_resource_profile()
            .expect("selected profile existence checked above");
        if let Some(nano_cpus) = self.nano_cpus {
            if nano_cpus <= 0 {
                return Err(ConfigFieldError::new(
                    "manager.docker.nano_cpus",
                    "legacy override must be greater than zero",
                ));
            }
        }
        if let Some(memory_bytes) = self.memory_bytes {
            if memory_bytes <= 0 {
                return Err(ConfigFieldError::new(
                    "manager.docker.memory_bytes",
                    "legacy override must be greater than zero",
                ));
            }
            if selected.separate_workload_cgroup && memory_bytes <= selected.memory_high_bytes {
                return Err(ConfigFieldError::new(
                    "manager.docker.memory_bytes",
                    format!(
                        "legacy override must exceed selected profile memory_high_bytes ({}) to preserve control-plane headroom",
                        selected.memory_high_bytes
                    ),
                ));
            }
        }
        for name in self.container_env.keys() {
            require_non_empty(name, "manager.docker.container_env")?;
            if name.contains('=') {
                return Err(ConfigFieldError::new(
                    "manager.docker.container_env",
                    format!("variable name `{name}` must not contain '='"),
                ));
            }
        }
        let mut names = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for volume in &self.persistent_volume_mounts {
            volume.validate()?;
            if !names.insert(volume.name.as_str()) {
                return Err(ConfigFieldError::new(
                    "manager.docker.persistent_volume_mounts",
                    format!("volume `{}` is configured more than once", volume.name),
                ));
            }
            if !targets.insert(volume.target.clone()) {
                return Err(ConfigFieldError::new(
                    "manager.docker.persistent_volume_mounts",
                    format!(
                        "target `{}` is configured more than once",
                        volume.target.display()
                    ),
                ));
            }
        }
        if self.materialize_shared_base_into_persistent_volume {
            if self.sandbox_owned_workspace_volumes {
                return Err(ConfigFieldError::new(
                    "manager.docker.materialize_shared_base_into_persistent_volume",
                    "requires sandbox_owned_workspace_volumes=false",
                ));
            }
            if !self
                .persistent_volume_mounts
                .iter()
                .any(|volume| !volume.readonly)
            {
                return Err(ConfigFieldError::new(
                    "manager.docker.materialize_shared_base_into_persistent_volume",
                    "requires at least one writable persistent volume",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct DaemonRuntimeProfileLimits {
    worker_threads: usize,
    max_blocking_threads: usize,
    blocking_thread_keep_alive_s: f64,
    max_concurrent_connections: usize,
    max_active_commands: usize,
}

fn require_profile_value<T>(
    actual: T,
    expected: T,
    field: &'static str,
    profile: &str,
) -> Result<(), ConfigFieldError>
where
    T: Copy + PartialEq + std::fmt::Display,
{
    if actual == expected {
        Ok(())
    } else {
        Err(profile_binding_error(field, actual, expected, profile))
    }
}

fn require_profile_at_most<T>(
    actual: T,
    ceiling: T,
    field: &'static str,
    profile: &str,
) -> Result<(), ConfigFieldError>
where
    T: Copy + PartialOrd + std::fmt::Display,
{
    if actual <= ceiling {
        Ok(())
    } else {
        Err(profile_ceiling_error(field, actual, ceiling, profile))
    }
}

fn profile_binding_error(
    field: &'static str,
    actual: impl std::fmt::Display,
    expected: impl std::fmt::Display,
    profile: &str,
) -> ConfigFieldError {
    ConfigFieldError::new(
        field,
        format!("must be {expected} for daemon runtime profile `{profile}` (got {actual})"),
    )
}

fn profile_ceiling_error(
    field: &'static str,
    actual: impl std::fmt::Display,
    ceiling: impl std::fmt::Display,
    profile: &str,
) -> ConfigFieldError {
    ConfigFieldError::new(
        field,
        format!("must be at most {ceiling} for daemon runtime profile `{profile}` (got {actual})"),
    )
}
