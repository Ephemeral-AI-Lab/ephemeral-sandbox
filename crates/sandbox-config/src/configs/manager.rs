//! Typed schema for the Docker manager-runtime section of the gateway config.
//!
//! The gateway loads this section only under `--backend docker`; it stays an
//! optional root section so existing daemon configs continue to load.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::configs::validate::{
    require_absolute, require_non_empty, require_u64_at_least, ConfigFieldError,
};

/// Host-side caps for the `export_changes` apply path (`manager.export`).
/// Retired the `EOS_EXPORT_MAX_DECOMPRESSED_BYTES` / `EOS_EXPORT_MAX_ENTRIES`
/// env side channels; the gateway injects these into the manager applier.
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
        require_u64_at_least(self.max_apply_entries, 1, "manager.export.max_apply_entries")
    }
}

pub const DEFAULT_CONTAINER_WORKSPACE_ROOT: &str = "/workspace";
pub const DEFAULT_CONTAINER_DAEMON_BINARY_PATH: &str = "/eos/bin/sandbox-daemon";
pub const DEFAULT_CONTAINER_DAEMON_CONFIG_PATH: &str = "/eos/config/daemon.yml";
pub const DEFAULT_DAEMON_PORT: u16 = 7000;
pub const DEFAULT_DAEMON_HTTP_PORT: u16 = 7001;
pub const DEFAULT_READINESS_TIMEOUT_MS: u64 = 60_000;
pub const DEFAULT_GATEWAY_INSTANCE_ID: &str = "eos-gateway";

/// Root `manager` section. Holds one backend sub-section; only `docker` exists
/// in v1, and it stays optional so the gateway's default `none` backend needs no
/// config at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagerConfig {
    /// Host path of the sandbox registry JSON snapshot. When set, the gateway
    /// persists every registry mutation there and reloads it on restart,
    /// reconciling against the containers the runtime actually recovers.
    /// `None` keeps the registry in process memory only.
    pub registry_path: Option<PathBuf>,
    pub export: ManagerExportConfig,
    pub docker: Option<DockerRuntimeConfig>,
}

impl ManagerConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when a field violates manager policy.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        self.export.validate()?;
        if let Some(docker) = &self.docker {
            docker.validate()?;
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
    /// Readiness deadline for the authenticated daemon check.
    pub readiness_timeout_ms: u64,
    /// Optional per-container memory cap in bytes.
    pub memory_bytes: Option<i64>,
    /// Optional per-container CPU cap in nano-CPUs.
    pub nano_cpus: Option<i64>,
    /// Environment variables injected into every sandbox container, as a
    /// `name -> value` map. The Docker CLI injects proxy settings from
    /// `~/.docker/config.json` into containers it runs; the Engine API this
    /// runtime uses does not, so declare them here (for example `HTTP_PROXY`)
    /// to give sandboxes the same egress path.
    pub container_env: BTreeMap<String, String>,
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
            readiness_timeout_ms: DEFAULT_READINESS_TIMEOUT_MS,
            memory_bytes: None,
            nano_cpus: None,
            container_env: BTreeMap::new(),
        }
    }
}

impl DockerRuntimeConfig {
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
        require_absolute(
            &self.container_daemon_binary_path,
            "manager.docker.container_daemon_binary_path",
        )?;
        require_absolute(
            &self.container_daemon_config_yaml_path,
            "manager.docker.container_daemon_config_yaml_path",
        )?;
        require_absolute(
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
        for name in self.container_env.keys() {
            require_non_empty(name, "manager.docker.container_env")?;
            if name.contains('=') {
                return Err(ConfigFieldError::new(
                    "manager.docker.container_env",
                    format!("variable name `{name}` must not contain '='"),
                ));
            }
        }
        Ok(())
    }
}
