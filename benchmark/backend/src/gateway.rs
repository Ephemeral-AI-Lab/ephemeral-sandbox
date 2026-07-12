use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sandbox_operation_client::{GatewayClient, GatewayClientError};
use sandbox_operation_contract::{OperationRequest, OperationScope};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::cleanup::{CleanupLedger, OwnedIdentity};
use crate::config::{BenchmarkPaths, StartupConfig};
use crate::executors::command::{CommandCase, ExecCommandCell};
use crate::model::ProductOperation;

const BASE_CONFIG: &str = "config/prd.yml";
const EFFECTIVE_CONFIG: &str = "effective-config.yml";
const REGISTRY_FILE: &str = "registry.json";
const PID_FILE: &str = "gateway.pid";
const SHARED_BASE_CACHE: &str = "shared-base-cache";
const GATEWAY_AUTH_ENV: &str = "SANDBOX_GATEWAY_AUTH_TOKEN";
const SHARED_BASE_CACHE_ENV: &str = "EOS_SHARED_BASE_CACHE";
const GIT_TOOLCHAIN_DIR_ENV: &str = "SANDBOX_GIT_TOOLCHAIN_DIR";
const FIXED_GIT_TOOLCHAIN_DIRECTORY: &str = "dist/git";
const FIXED_GIT_TOOLCHAIN_ARCHIVES: &[&str] = &["linux-arm64.tar", "linux-amd64.tar"];
const DOCKER_BINARY: &str = "docker";
const GATEWAY_INSTANCE_LABEL: &str = "eos.gateway_instance_id";
const SHARED_BASE_VOLUME_PREFIX: &str = "eos-shared-base-";
const SHARED_BASE_VOLUME_DIGEST_HEX_LEN: usize = 64;
const MAX_OWNED_SHARED_BASE_VOLUMES: usize = 64;
const MAX_DOCKER_CLEANUP_OUTPUT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_GATEWAY_CONNECTIONS: usize = 256;
pub(crate) const READINESS_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const READINESS_POLL: Duration = Duration::from_millis(50);
pub(crate) const READINESS_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const OWNED_RESOURCE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10 * 60);
pub(crate) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const LOG_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const MAX_LOG_BYTES: usize = 128 * 1024;
pub(crate) const MAX_LOG_LINE_BYTES: usize = 16 * 1024;
pub(crate) const MAX_PRODUCT_PATH_BYTES: usize = 4_096;
pub(crate) const MAX_PRODUCT_CONTENT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_PRODUCT_EDITS: usize = 4_096;
pub(crate) const MAX_COMMAND_TIMEOUT_MS: u64 = 120 * 1_000;
pub(crate) const MAX_PRODUCT_TRACE_NODES: usize = 16_384;
pub(crate) const PRODUCT_RESOURCE_WINDOW_MS: u64 = 10 * 60 * 1_000;
pub(crate) const MAX_PRODUCT_RESOURCE_SAMPLES: usize = 65_536;
/// A product response is untrusted diagnostic input. Preserve only a small,
/// sanitized summary so terminal benchmark evidence can attribute a failure
/// without becoming an unbounded or credential-bearing error channel.
pub(crate) const MAX_PRODUCT_ERROR_DETAIL_BYTES: usize = 1_024;
const MAX_PRODUCT_SNAPSHOT_WORKSPACES: usize = 4_096;

/// Fixed launch inputs. Bind addresses, runtime paths, credentials, and safety
/// caps are intentionally not configurable through a benchmark plan.
#[derive(Debug, Clone)]
pub struct GatewayLaunchConfig {
    gateway_binary: PathBuf,
    daemon_binary: PathBuf,
    remount_sweep_width: u32,
}

impl GatewayLaunchConfig {
    #[must_use]
    pub fn new(
        gateway_binary: impl Into<PathBuf>,
        daemon_binary: impl Into<PathBuf>,
        remount_sweep_width: u32,
    ) -> Self {
        Self {
            gateway_binary: gateway_binary.into(),
            daemon_binary: daemon_binary.into(),
            remount_sweep_width,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayIdentity {
    pub runner_instance_id: String,
    pub gateway_instance_id: String,
    pub bind_addr: SocketAddr,
    pub remount_sweep_width: u32,
    pub gateway_binary_sha256: String,
    pub daemon_binary_sha256: String,
    pub effective_config_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnedSandboxId(String);

impl OwnedSandboxId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Correlation is retained in typed form while the product receives one stable,
/// bounded request id derived from every component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Correlation {
    pub run_id: String,
    pub cell_id: String,
    pub trial_id: String,
    pub request_id: String,
    wire_request_id: String,
}

impl Correlation {
    pub fn new(
        run_id: impl Into<String>,
        cell_id: impl Into<String>,
        trial_id: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Result<Self, GatewayError> {
        let run_id = validate_correlation_part("run_id", run_id.into())?;
        let cell_id = validate_correlation_part("cell_id", cell_id.into())?;
        let trial_id = validate_correlation_part("trial_id", trial_id.into())?;
        let request_id = validate_correlation_part("request_id", request_id.into())?;
        let mut digest = Sha256::new();
        for value in [&run_id, &cell_id, &trial_id, &request_id] {
            digest.update(value.len().to_le_bytes());
            digest.update(value.as_bytes());
        }
        let wire_request_id = format!("benchmark-{:x}", digest.finalize());
        Ok(Self {
            run_id,
            cell_id,
            trial_id,
            request_id,
            wire_request_id,
        })
    }

    #[must_use]
    pub fn wire_request_id(&self) -> &str {
        &self.wire_request_id
    }

    fn internal(label: &str) -> Self {
        let request = Uuid::now_v7().to_string();
        Self::new("runner", label, "infrastructure", request)
            .expect("fixed internal correlation is valid")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductPath(String);

impl ProductPath {
    pub fn new(value: impl Into<String>) -> Result<Self, GatewayError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_PRODUCT_PATH_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(GatewayError::InvalidProductInput("invalid product path"));
        }
        let path = Path::new(&value);
        let normalized = path.components().collect::<PathBuf>();
        if path.is_absolute()
            || value.contains('\\')
            || normalized.as_os_str() != path.as_os_str()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(GatewayError::InvalidProductInput(
                "product path must be a canonical repository-relative path",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductEdit {
    old_string: String,
    new_string: String,
    replace_all: bool,
}

impl ProductEdit {
    pub fn new(
        old_string: impl Into<String>,
        new_string: impl Into<String>,
        replace_all: bool,
    ) -> Result<Self, GatewayError> {
        let old_string = old_string.into();
        let new_string = new_string.into();
        if old_string.is_empty()
            || old_string.len().saturating_add(new_string.len()) > MAX_PRODUCT_CONTENT_BYTES
        {
            return Err(GatewayError::InvalidProductInput(
                "file edit strings exceed the fixed safety bound",
            ));
        }
        Ok(Self {
            old_string,
            new_string,
            replace_all,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayLogs {
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayShutdown {
    pub logs: GatewayLogs,
    pub runtime_removed: bool,
    pub shared_base_volumes_removed: bool,
}

/// Strict live LayerStack inventory returned by the product observability
/// operation. This is a fixed read-only adapter surface, not general RPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProductLayerstackSnapshot {
    pub(crate) manifest_version: u64,
    pub(crate) root_hash: String,
    pub(crate) active_lease_count: u32,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) total_allocated_bytes: Option<u64>,
    pub(crate) storage_logical_bytes: Option<u64>,
    pub(crate) storage_allocated_bytes: Option<u64>,
    pub(crate) staging_entry_count: Option<u64>,
    pub(crate) layers: Vec<ProductLayerstackLayer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProductLayerstackLayer {
    pub(crate) layer_id: String,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub(crate) bytes: Option<u64>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub(crate) allocated_bytes: Option<u64>,
    pub(crate) leased_by_workspaces: u32,
    pub(crate) booked_by: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductLayerstackSnapshotWire {
    view: String,
    manifest_version: u64,
    root_hash: String,
    active_lease_count: u32,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    total_bytes: Option<u64>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    total_allocated_bytes: Option<u64>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    storage_logical_bytes: Option<u64>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    storage_allocated_bytes: Option<u64>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    staging_entry_count: Option<u64>,
    layers: Vec<ProductLayerstackLayer>,
}

/// One runtime-owned sandbox resource observation. Missing counters remain
/// `None`; zero is reserved for an observed zero reported by the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProductSandboxResources {
    pub(crate) observed_unix_ms: i64,
    pub(crate) cpu_usage_usec: Option<u64>,
    pub(crate) memory_current_bytes: Option<u64>,
    pub(crate) memory_limit_bytes: Option<u64>,
    pub(crate) io_read_bytes: Option<u64>,
    pub(crate) io_write_bytes: Option<u64>,
}

/// Allocated storage observed through the product's closed snapshot contract.
/// The daemon PID is deliberately named as container-scoped: the Docker-backed
/// gateway does not prove a corresponding host PID or process start identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProductStorageResources {
    pub(crate) daemon_container_pid: u32,
    pub(crate) layerstack_storage_allocated_bytes: Option<u64>,
    pub(crate) upperdir: ProductUpperdirAllocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProductUpperdirAllocation {
    Available {
        allocated_bytes: u64,
        workspace_count: u64,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotWire {
    sandbox_id: String,
    lifecycle_state: String,
    availability: ProductSnapshotAvailability,
    sampled_at_unix_ms: i64,
    errors: Vec<String>,
    daemon: ProductSnapshotDaemonWire,
    resources: ProductSnapshotResourceBundleWire,
    workspaces: Vec<ProductSnapshotWorkspaceWire>,
    stack: Option<ProductSnapshotStackWire>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProductSnapshotAvailability {
    Available,
    Partial,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotDaemonWire {
    daemon_pid: u32,
    runtime_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotResourceBundleWire {
    #[serde(deserialize_with = "deserialize_required_nullable")]
    latest: Option<ProductSnapshotSampleWire>,
    history: Vec<ProductSnapshotSampleWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotSampleWire {
    ts: i64,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    sample_delta_ms: Option<i64>,
    metrics: ProductSnapshotMetricsWire,
    deltas: ProductSnapshotDeltasWire,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotMetricsWire {
    cpu_usec: Option<u64>,
    mem_cur: Option<u64>,
    mem_max: Option<u64>,
    mem_max_unlimited: Option<bool>,
    cgroup_available: Option<bool>,
    cgroup_error: Option<String>,
    disk_bytes: Option<u64>,
    disk_allocated_bytes: Option<u64>,
    files: Option<u64>,
    disk_truncated: Option<bool>,
    #[serde(rename = "_truncated")]
    record_truncated_bytes: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotDeltasWire {
    cpu_usec: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotWorkspaceWire {
    workspace_id: String,
    lifecycle_state: String,
    network_profile: String,
    finalize_policy: String,
    layers: ProductSnapshotWorkspaceLayersWire,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    namespace_fd_count: Option<u64>,
    resources: ProductSnapshotResourceBundleWire,
    active_namespace_executions: Vec<ProductSnapshotNamespaceExecutionWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotWorkspaceLayersWire {
    #[serde(deserialize_with = "deserialize_required_nullable")]
    base_root_hash: Option<String>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    layer_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotNamespaceExecutionWire {
    namespace_execution_id: String,
    operation: String,
    lifecycle_state: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductSnapshotStackWire {
    layer_count: u64,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    layers_bytes: Option<u64>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    layers_allocated_bytes: Option<u64>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    storage_allocated_bytes: Option<u64>,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    staging_entry_count: Option<u64>,
    active_leases: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductCgroupWire {
    view: String,
    scope: String,
    series: Vec<ProductCgroupSampleWire>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductCgroupSampleWire {
    ts: i64,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    sample_delta_ms: Option<u64>,
    metrics: ProductCgroupMetricsWire,
    deltas: ProductCgroupDeltasWire,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductCgroupMetricsWire {
    metrics_source: ProductResourceSource,
    cpu_usec: Option<u64>,
    mem_cur: Option<u64>,
    mem_max: Option<u64>,
    io_rbytes: Option<u64>,
    io_wbytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum ProductResourceSource {
    #[serde(rename = "docker_engine")]
    DockerEngine,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductCgroupDeltasWire {
    cpu_usec: Option<u64>,
    io_rbytes: Option<u64>,
    io_wbytes: Option<u64>,
}

fn deserialize_required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Strict product trace tree used to correlate registered benchmark phases.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProductTrace {
    pub(crate) trace_id: String,
    pub(crate) spans: Vec<ProductTraceSpanNode>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProductTraceSpanNode {
    pub(crate) span: ProductTraceSpan,
    pub(crate) offset_ms: f64,
    pub(crate) children: Vec<ProductTraceSpanNode>,
    events: Vec<ProductTraceEventNode>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProductTraceSpan {
    pub(crate) ts: i64,
    pub(crate) trace: String,
    pub(crate) span: String,
    pub(crate) parent: Option<String>,
    pub(crate) name: String,
    pub(crate) dur_ms: f64,
    pub(crate) status: ProductTraceStatus,
    pub(crate) attrs: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProductTraceStatus {
    Completed,
    Error,
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductTraceEventNode {
    offset_ms: f64,
    event: ProductTraceEvent,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductTraceEvent {
    ts: i64,
    trace: String,
    parent: Option<String>,
    name: String,
    attrs: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductTraceWire {
    view: String,
    trace: String,
    spans: Vec<ProductTraceSpanNode>,
}

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("invalid gateway launch input: {0}")]
    InvalidLaunch(&'static str),
    #[error("invalid product input: {0}")]
    InvalidProductInput(&'static str),
    #[error("invalid correlation field {0}")]
    InvalidCorrelation(&'static str),
    #[error("filesystem operation failed while {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("effective sandbox configuration is invalid")]
    InvalidEffectiveConfig,
    #[error("gateway process could not be started")]
    Spawn,
    #[error("gateway process streams were unavailable")]
    MissingProcessStream,
    #[error("gateway readiness timed out")]
    ReadinessTimeout,
    #[error("gateway pid ownership check failed")]
    PidOwnership,
    #[error("gateway process crashed")]
    Crashed,
    #[error("gateway transport failed ({kind})")]
    Client { kind: &'static str },
    #[error("gateway returned an error ({kind}): {detail}")]
    Product { kind: String, detail: String },
    #[error("gateway response did not match the product schema")]
    ResponseSchema,
    #[error("gateway response contained benchmark credentials")]
    CredentialEcho,
    #[error("sandbox identity is not owned by this gateway")]
    UnownedSandbox,
    #[error("sandbox identity collision")]
    SandboxIdentityCollision,
    #[error("benchmark workspace is not marker-owned by this trial")]
    WorkspaceOwnership,
    #[error("gateway state lock is unavailable")]
    StateUnavailable,
    #[error("gateway cleanup was incomplete")]
    CleanupIncomplete,
    #[error("gateway shutdown cleanup was incomplete")]
    ShutdownIncomplete { shutdown: GatewayShutdown },
}

pub struct IsolatedGateway {
    identity: GatewayIdentity,
    product: Arc<ProductGateway>,
    child: Child,
    child_pid: u32,
    runtime_path: PathBuf,
    paths: BenchmarkPaths,
    runtime_identity: OwnedIdentity,
    cleanup: CleanupLedger,
    logs: Arc<LogCapture>,
    stdout_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
}

/// Removes a marker-owned runtime directory when startup returns before the
/// gateway object can take responsibility for it.
struct StartupRuntimeGuard {
    cleanup: CleanupLedger,
    paths: BenchmarkPaths,
    runtime_path: PathBuf,
    runtime_identity: OwnedIdentity,
    armed: bool,
}

impl StartupRuntimeGuard {
    fn into_cleanup(mut self) -> CleanupLedger {
        self.armed = false;
        std::mem::take(&mut self.cleanup)
    }
}

impl Drop for StartupRuntimeGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ =
                self.cleanup
                    .remove_owned(&self.paths, &self.runtime_path, &self.runtime_identity);
        }
    }
}

impl IsolatedGateway {
    pub async fn start(
        startup: &StartupConfig,
        launch: GatewayLaunchConfig,
    ) -> Result<Self, GatewayError> {
        if launch.remount_sweep_width == 0 {
            return Err(GatewayError::InvalidLaunch(
                "remount sweep width must be positive",
            ));
        }
        let gateway_binary = canonical_file(&launch.gateway_binary, "resolving gateway binary")?;
        let daemon_binary = canonical_file(&launch.daemon_binary, "resolving daemon binary")?;
        let git_toolchain_dir = fixed_git_toolchain_directory(&startup.repo)?;
        let runs_root = canonical_owned_directory(
            &startup.paths.runs,
            "resolving benchmark workspace allowlist root",
        )?;
        let gateway_binary_sha256 = sha256_file(&gateway_binary)?;
        let daemon_binary_sha256 = sha256_file(&daemon_binary)?;

        let runner_instance_id = format!("runner-{}", Uuid::now_v7());
        let gateway_instance_id = format!("benchmark-gateway-{}", Uuid::now_v7());
        let runtime_identity = OwnedIdentity::Runtime {
            runner_instance_id: runner_instance_id.clone(),
        };
        let runtime_path = startup.paths.runtime.join(&runner_instance_id);
        fs::create_dir(&runtime_path).map_err(|source| GatewayError::Io {
            action: "creating isolated gateway runtime directory",
            source,
        })?;
        if let Err(error) = set_owner_only_directory(&runtime_path) {
            let _ = fs::remove_dir(&runtime_path);
            return Err(error);
        }
        let mut cleanup = CleanupLedger::default();
        if cleanup
            .register(&startup.paths, &runtime_path, runtime_identity.clone())
            .is_err()
        {
            // Registration may have created its known marker before a final
            // durability step failed. Remove only that file and the now-empty
            // directory; never recurse without a registered ownership entry.
            let _ = fs::remove_file(runtime_path.join(crate::cleanup::OWNERSHIP_MARKER));
            let _ = fs::remove_dir(&runtime_path);
            return Err(GatewayError::CleanupIncomplete);
        }
        let runtime_guard = StartupRuntimeGuard {
            cleanup,
            paths: startup.paths.clone(),
            runtime_path: runtime_path.clone(),
            runtime_identity: runtime_identity.clone(),
            armed: true,
        };

        let shared_base_cache = runtime_path.join(SHARED_BASE_CACHE);
        fs::create_dir(&shared_base_cache).map_err(|source| GatewayError::Io {
            action: "creating isolated shared-base cache",
            source,
        })?;
        set_owner_only_directory(&shared_base_cache)?;

        let reservation =
            TcpListener::bind((IpAddr::from([127, 0, 0, 1]), 0)).map_err(|source| {
                GatewayError::Io {
                    action: "reserving isolated gateway port",
                    source,
                }
            })?;
        let bind_addr = reservation
            .local_addr()
            .map_err(|source| GatewayError::Io {
                action: "reading isolated gateway port",
                source,
            })?;
        if !bind_addr.ip().is_loopback() {
            return Err(GatewayError::InvalidLaunch("gateway bind must be loopback"));
        }

        let effective_config = runtime_path.join(EFFECTIVE_CONFIG);
        render_effective_config(
            startup,
            &effective_config,
            &daemon_binary,
            bind_addr,
            &runtime_path,
            &gateway_instance_id,
            launch.remount_sweep_width,
        )?;
        let effective_config_sha256 = sha256_file(&effective_config)?;
        let auth_token = format!("benchmark-{}-{}", Uuid::now_v7(), Uuid::now_v7());

        let mut command = Command::new(&gateway_binary);
        command
            .arg("serve")
            .arg("--backend")
            .arg("docker")
            .arg("--config-yaml")
            .arg(&effective_config)
            .current_dir(&startup.repo)
            .env_clear()
            .env(GATEWAY_AUTH_ENV, &auth_token)
            .env(SHARED_BASE_CACHE_ENV, &shared_base_cache)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        copy_process_environment(&mut command);
        command.env(GIT_TOOLCHAIN_DIR_ENV, &git_toolchain_dir);
        drop(reservation);

        let mut child = command.spawn().map_err(|_| GatewayError::Spawn)?;
        let child_pid = match child.id() {
            Some(pid) => pid,
            None => {
                let _ = terminate_child(&mut child).await;
                return Err(GatewayError::Spawn);
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = terminate_child(&mut child).await;
                return Err(GatewayError::MissingProcessStream);
            }
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                let _ = terminate_child(&mut child).await;
                return Err(GatewayError::MissingProcessStream);
            }
        };
        let logs = Arc::new(LogCapture::default());
        let stdout_task = tokio::spawn(drain_log(
            stdout,
            Arc::clone(&logs),
            LogStream::Stdout,
            auth_token.clone(),
        ));
        let stderr_task = tokio::spawn(drain_log(
            stderr,
            Arc::clone(&logs),
            LogStream::Stderr,
            auth_token.clone(),
        ));
        let product = Arc::new(ProductGateway::new(bind_addr, auth_token, runs_root));
        let identity = GatewayIdentity {
            runner_instance_id,
            gateway_instance_id,
            bind_addr,
            remount_sweep_width: launch.remount_sweep_width,
            gateway_binary_sha256,
            daemon_binary_sha256,
            effective_config_sha256,
        };
        let cleanup = runtime_guard.into_cleanup();
        let mut gateway = Self {
            identity,
            product,
            child,
            child_pid,
            runtime_path,
            paths: startup.paths.clone(),
            runtime_identity,
            cleanup,
            logs,
            stdout_task: Some(stdout_task),
            stderr_task: Some(stderr_task),
        };
        if let Err(error) = gateway.wait_until_ready().await {
            gateway.abort_start().await;
            return Err(error);
        }
        Ok(gateway)
    }

    #[must_use]
    pub fn identity(&self) -> &GatewayIdentity {
        &self.identity
    }

    #[must_use]
    pub fn product(&self) -> Arc<ProductGateway> {
        Arc::clone(&self.product)
    }

    #[must_use]
    pub fn logs(&self) -> GatewayLogs {
        self.logs.snapshot()
    }

    pub fn ensure_alive(&mut self) -> Result<(), GatewayError> {
        match self.child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(_)) | Err(_) => Err(GatewayError::Crashed),
        }
    }

    pub async fn stop(mut self) -> Result<GatewayShutdown, GatewayError> {
        let cleanup_ok = tokio::time::timeout(
            OWNED_RESOURCE_CLEANUP_TIMEOUT,
            self.product.destroy_all_owned(),
        )
        .await
        .is_ok_and(|result| result.is_ok());
        let child_ok = terminate_child(&mut self.child).await;
        self.join_log_tasks().await;
        let shared_base_volumes_removed = cleanup_ok
            && child_ok
            && cleanup_owned_shared_base_volumes(&self.identity.gateway_instance_id)
                .await
                .is_ok();
        let mut runtime_removed = false;
        if cleanup_ok && child_ok && shared_base_volumes_removed {
            runtime_removed = self
                .cleanup
                .remove_owned(&self.paths, &self.runtime_path, &self.runtime_identity)
                .is_ok();
        }
        let shutdown = GatewayShutdown {
            logs: self.logs.snapshot(),
            runtime_removed,
            shared_base_volumes_removed,
        };
        if cleanup_ok && child_ok && shared_base_volumes_removed && runtime_removed {
            Ok(shutdown)
        } else {
            Err(GatewayError::ShutdownIncomplete { shutdown })
        }
    }

    async fn wait_until_ready(&mut self) -> Result<(), GatewayError> {
        let deadline = Instant::now() + READINESS_TIMEOUT;
        loop {
            self.ensure_alive()?;
            if pid_ready(&self.runtime_path.join(PID_FILE), self.child_pid).await?
                && tokio::time::timeout(READINESS_PROBE_TIMEOUT, self.product.readiness_probe())
                    .await
                    .is_ok_and(|result| result.is_ok())
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(GatewayError::ReadinessTimeout);
            }
            tokio::time::sleep(READINESS_POLL).await;
        }
    }

    async fn abort_start(&mut self) {
        let _ = terminate_child(&mut self.child).await;
        self.join_log_tasks().await;
        let _ = cleanup_owned_shared_base_volumes(&self.identity.gateway_instance_id).await;
        let _ = self
            .cleanup
            .remove_owned(&self.paths, &self.runtime_path, &self.runtime_identity);
    }

    async fn join_log_tasks(&mut self) {
        if let Some(task) = self.stdout_task.take() {
            join_log_task(task).await;
        }
        if let Some(task) = self.stderr_task.take() {
            join_log_task(task).await;
        }
    }
}

impl Drop for IsolatedGateway {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// The authenticated transport and credential remain private. Callers can only
/// select one of the fixed typed methods below.
pub struct ProductGateway {
    client: GatewayClient,
    auth_token: String,
    runs_root: PathBuf,
    owned_sandboxes: Mutex<BTreeSet<String>>,
}

impl std::fmt::Debug for ProductGateway {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductGateway")
            .field("runs_root", &self.runs_root)
            .field("auth_token", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl ProductGateway {
    fn new(bind_addr: SocketAddr, auth_token: String, runs_root: PathBuf) -> Self {
        Self {
            client: GatewayClient::new(bind_addr.to_string(), Some(auth_token.clone())),
            auth_token,
            runs_root,
            owned_sandboxes: Mutex::new(BTreeSet::new()),
        }
    }

    pub async fn create_sandbox(
        &self,
        image: &str,
        workspace_root: &Path,
        correlation: &Correlation,
    ) -> Result<OwnedSandboxId, GatewayError> {
        if image.trim().is_empty() || image.len() > 512 || image.chars().any(char::is_control) {
            return Err(GatewayError::InvalidProductInput("invalid image reference"));
        }
        let workspace_root = self.validate_owned_workspace(workspace_root, correlation)?;
        let request = OperationRequest::new(
            sandbox_operation_catalog::manager::CREATE_SANDBOX_SPEC.name,
            correlation.wire_request_id(),
            OperationScope::system(),
            json!({
                "image": image,
                "workspace_root": workspace_root,
                "count": 1,
            }),
        );
        let wire: SandboxRecordWire = self.send_typed(&request).await?;
        let record = SandboxRecord::try_from(wire)?;
        if record.state != SandboxState::Ready || record.workspace_root != workspace_root {
            return Err(GatewayError::ResponseSchema);
        }
        let id = record.id;
        let mut owned = self
            .owned_sandboxes
            .lock()
            .map_err(|_| GatewayError::StateUnavailable)?;
        if !owned.insert(id.0.clone()) {
            return Err(GatewayError::SandboxIdentityCollision);
        }
        Ok(id)
    }

    pub async fn destroy_sandbox(
        &self,
        sandbox_id: &OwnedSandboxId,
        correlation: &Correlation,
    ) -> Result<(), GatewayError> {
        self.require_owned(sandbox_id)?;
        self.destroy_known_sandbox(sandbox_id, correlation).await?;
        self.owned_sandboxes
            .lock()
            .map_err(|_| GatewayError::StateUnavailable)?
            .remove(sandbox_id.as_str());
        Ok(())
    }

    pub(crate) async fn inspect_sandbox(
        &self,
        sandbox_id: &OwnedSandboxId,
        correlation: &Correlation,
    ) -> Result<SandboxRecord, GatewayError> {
        self.require_owned(sandbox_id)?;
        let request = OperationRequest::new(
            sandbox_operation_catalog::manager::INSPECT_SANDBOX_SPEC.name,
            correlation.wire_request_id(),
            OperationScope::system(),
            json!({ "sandbox_id": sandbox_id.as_str() }),
        );
        let wire: SandboxRecordWire = self.send_typed(&request).await?;
        let record = SandboxRecord::try_from(wire)?;
        if record.id != *sandbox_id || record.state != SandboxState::Ready {
            return Err(GatewayError::ResponseSchema);
        }
        Ok(record)
    }

    pub async fn exec_command(
        &self,
        sandbox_id: &OwnedSandboxId,
        workspace_session_id: Option<&crate::daemon_session::WorkspaceSessionId>,
        cell: &ExecCommandCell,
        timeout_ms: u64,
        yield_time_ms: u64,
        correlation: &Correlation,
    ) -> Result<Value, GatewayError> {
        self.require_owned(sandbox_id)?;
        validate_command_cell(cell)?;
        if timeout_ms == 0 || timeout_ms > MAX_COMMAND_TIMEOUT_MS || yield_time_ms > timeout_ms {
            return Err(GatewayError::InvalidProductInput(
                "command timing exceeds the fixed safety bound",
            ));
        }
        self.send_public(
            PublicRequest::ExecCommand {
                sandbox_id: sandbox_id.0.clone(),
                workspace_session_id: workspace_session_id.map(|id| id.as_str().to_owned()),
                command: cell.command.clone(),
                timeout_ms,
                yield_time_ms,
            },
            correlation,
        )
        .await
    }

    pub async fn file_read(
        &self,
        sandbox_id: &OwnedSandboxId,
        workspace_session_id: Option<&crate::daemon_session::WorkspaceSessionId>,
        path: &ProductPath,
        offset: u64,
        limit: u64,
        correlation: &Correlation,
    ) -> Result<Value, GatewayError> {
        self.require_owned(sandbox_id)?;
        if offset == 0 || !(1..=2_000).contains(&limit) {
            return Err(GatewayError::InvalidProductInput(
                "file read window exceeds the fixed safety bound",
            ));
        }
        self.send_public(
            PublicRequest::FileRead {
                sandbox_id: sandbox_id.0.clone(),
                workspace_session_id: workspace_session_id.map(|id| id.as_str().to_owned()),
                path: path.0.clone(),
                offset,
                limit,
            },
            correlation,
        )
        .await
    }

    pub async fn file_write(
        &self,
        sandbox_id: &OwnedSandboxId,
        workspace_session_id: Option<&crate::daemon_session::WorkspaceSessionId>,
        path: &ProductPath,
        content: &str,
        correlation: &Correlation,
    ) -> Result<Value, GatewayError> {
        self.require_owned(sandbox_id)?;
        if content.len() > MAX_PRODUCT_CONTENT_BYTES {
            return Err(GatewayError::InvalidProductInput(
                "file content exceeds the fixed safety bound",
            ));
        }
        self.send_public(
            PublicRequest::FileWrite {
                sandbox_id: sandbox_id.0.clone(),
                workspace_session_id: workspace_session_id.map(|id| id.as_str().to_owned()),
                path: path.0.clone(),
                content: content.to_owned(),
            },
            correlation,
        )
        .await
    }

    pub async fn file_edit(
        &self,
        sandbox_id: &OwnedSandboxId,
        workspace_session_id: Option<&crate::daemon_session::WorkspaceSessionId>,
        path: &ProductPath,
        edits: &[ProductEdit],
        correlation: &Correlation,
    ) -> Result<Value, GatewayError> {
        self.require_owned(sandbox_id)?;
        if edits.is_empty() || edits.len() > MAX_PRODUCT_EDITS {
            return Err(GatewayError::InvalidProductInput(
                "file edit count exceeds the fixed safety bound",
            ));
        }
        let edit_bytes = edits.iter().fold(0_usize, |total, edit| {
            total
                .saturating_add(edit.old_string.len())
                .saturating_add(edit.new_string.len())
        });
        if edit_bytes > MAX_PRODUCT_CONTENT_BYTES {
            return Err(GatewayError::InvalidProductInput(
                "file edits exceed the fixed safety bound",
            ));
        }
        self.send_public(
            PublicRequest::FileEdit {
                sandbox_id: sandbox_id.0.clone(),
                workspace_session_id: workspace_session_id.map(|id| id.as_str().to_owned()),
                path: path.0.clone(),
                edits: edits.to_vec(),
            },
            correlation,
        )
        .await
    }

    pub async fn file_blame(
        &self,
        sandbox_id: &OwnedSandboxId,
        path: &ProductPath,
        correlation: &Correlation,
    ) -> Result<Value, GatewayError> {
        self.require_owned(sandbox_id)?;
        self.send_public(
            PublicRequest::FileBlame {
                sandbox_id: sandbox_id.0.clone(),
                path: path.0.clone(),
            },
            correlation,
        )
        .await
    }

    pub async fn squash_layerstacks(
        &self,
        sandbox_id: &OwnedSandboxId,
        correlation: &Correlation,
    ) -> Result<Value, GatewayError> {
        self.require_owned(sandbox_id)?;
        self.send_public(
            PublicRequest::SquashLayerstacks {
                sandbox_id: sandbox_id.0.clone(),
            },
            correlation,
        )
        .await
    }

    /// Read the owned sandbox's live LayerStack inventory through the fixed
    /// observability contract.
    pub(crate) async fn observe_layerstack(
        &self,
        sandbox_id: &OwnedSandboxId,
        correlation: &Correlation,
    ) -> Result<ProductLayerstackSnapshot, GatewayError> {
        self.require_owned(sandbox_id)?;
        let request = OperationRequest::new(
            sandbox_operation_catalog::observability::LAYERSTACK_SPEC.name,
            correlation.wire_request_id(),
            OperationScope::sandbox(sandbox_id.as_str()),
            json!({}),
        );
        let wire: ProductLayerstackSnapshotWire = self.send_typed(&request).await?;
        validate_layerstack_snapshot(wire)
    }

    /// Read runtime-owned CPU, memory, and block-I/O counters for one owned
    /// sandbox through the closed observability operation.
    pub(crate) async fn observe_sandbox_resources(
        &self,
        sandbox_id: &OwnedSandboxId,
        correlation: &Correlation,
    ) -> Result<ProductSandboxResources, GatewayError> {
        self.require_owned(sandbox_id)?;
        let request = OperationRequest::new(
            sandbox_operation_catalog::observability::CGROUP_SPEC.name,
            correlation.wire_request_id(),
            OperationScope::sandbox(sandbox_id.as_str()),
            json!({
                "scope": "sandbox",
                "window_ms": PRODUCT_RESOURCE_WINDOW_MS,
            }),
        );
        let wire: ProductCgroupWire = self.send_typed(&request).await?;
        validate_product_cgroup(wire)
    }

    /// Read daemon identity and allocated LayerStack/upperdir storage through
    /// the fixed product snapshot operation. No runtime path or generic RPC is
    /// exposed to callers.
    pub(crate) async fn observe_storage_resources(
        &self,
        sandbox_id: &OwnedSandboxId,
        correlation: &Correlation,
    ) -> Result<ProductStorageResources, GatewayError> {
        self.require_owned(sandbox_id)?;
        let request = OperationRequest::new(
            sandbox_operation_catalog::observability::SNAPSHOT_SPEC.name,
            correlation.wire_request_id(),
            OperationScope::sandbox(sandbox_id.as_str()),
            json!({}),
        );
        let wire: ProductSnapshotWire = self.send_typed(&request).await?;
        validate_product_snapshot(wire, sandbox_id.as_str())
    }

    /// Read exactly one owned squash trace. The target trace id is derived
    /// from a typed correlation and cannot be supplied as arbitrary text.
    pub(crate) async fn observe_trace(
        &self,
        sandbox_id: &OwnedSandboxId,
        target: &Correlation,
        query_correlation: &Correlation,
    ) -> Result<ProductTrace, GatewayError> {
        self.require_owned(sandbox_id)?;
        let request = OperationRequest::new(
            sandbox_operation_catalog::observability::TRACE_SPEC.name,
            query_correlation.wire_request_id(),
            OperationScope::sandbox(sandbox_id.as_str()),
            json!({ "trace_id": target.wire_request_id() }),
        );
        let wire: ProductTraceWire = self.send_typed(&request).await?;
        validate_product_trace(wire, target.wire_request_id())
    }

    async fn readiness_probe(&self) -> Result<(), GatewayError> {
        let records = self
            .list_sandboxes(&Correlation::internal("readiness"))
            .await?;
        if records.is_empty() {
            Ok(())
        } else {
            Err(GatewayError::ResponseSchema)
        }
    }

    async fn list_sandboxes(
        &self,
        correlation: &Correlation,
    ) -> Result<Vec<SandboxRecord>, GatewayError> {
        let request = OperationRequest::new(
            sandbox_operation_catalog::manager::LIST_SANDBOXES_SPEC.name,
            correlation.wire_request_id(),
            OperationScope::system(),
            json!({}),
        );
        let wire: SandboxListWire = self.send_typed(&request).await?;
        wire.sandboxes
            .into_iter()
            .map(SandboxRecord::try_from)
            .collect()
    }

    async fn destroy_known_sandbox(
        &self,
        sandbox_id: &OwnedSandboxId,
        correlation: &Correlation,
    ) -> Result<(), GatewayError> {
        let request = OperationRequest::new(
            sandbox_operation_catalog::manager::DESTROY_SANDBOX_SPEC.name,
            correlation.wire_request_id(),
            OperationScope::system(),
            json!({ "sandbox_id": sandbox_id.as_str() }),
        );
        let wire: SandboxRecordWire = self.send_typed(&request).await?;
        let record = SandboxRecord::try_from(wire)?;
        if record.id != *sandbox_id || record.state != SandboxState::Stopped {
            return Err(GatewayError::ResponseSchema);
        }
        Ok(())
    }

    async fn destroy_all_owned(&self) -> Result<(), GatewayError> {
        let records = self
            .list_sandboxes(&Correlation::internal("shutdown-list"))
            .await?;
        let mut failed = false;
        for record in records {
            if self
                .destroy_known_sandbox(&record.id, &Correlation::internal("shutdown-destroy"))
                .await
                .is_err()
            {
                failed = true;
            } else if let Ok(mut owned) = self.owned_sandboxes.lock() {
                owned.remove(record.id.as_str());
            }
        }
        if failed {
            Err(GatewayError::CleanupIncomplete)
        } else {
            Ok(())
        }
    }

    async fn send_public(
        &self,
        request: PublicRequest,
        correlation: &Correlation,
    ) -> Result<Value, GatewayError> {
        let request = request.into_wire(correlation);
        self.send(&request).await
    }

    async fn send_typed<T: DeserializeOwned>(
        &self,
        request: &OperationRequest,
    ) -> Result<T, GatewayError> {
        let value = self.send(request).await?;
        serde_json::from_value(value).map_err(|_| GatewayError::ResponseSchema)
    }

    async fn send(&self, request: &OperationRequest) -> Result<Value, GatewayError> {
        let value = self.client.send(request).await.map_err(client_error)?;
        let encoded = serde_json::to_string(&value).map_err(|_| GatewayError::ResponseSchema)?;
        if encoded.contains(&self.auth_token) {
            return Err(GatewayError::CredentialEcho);
        }
        reject_product_error(&value, &self.auth_token)?;
        Ok(value)
    }

    fn require_owned(&self, sandbox_id: &OwnedSandboxId) -> Result<(), GatewayError> {
        let owned = self
            .owned_sandboxes
            .lock()
            .map_err(|_| GatewayError::StateUnavailable)?;
        if owned.contains(sandbox_id.as_str()) {
            Ok(())
        } else {
            Err(GatewayError::UnownedSandbox)
        }
    }

    fn validate_owned_workspace(
        &self,
        workspace: &Path,
        correlation: &Correlation,
    ) -> Result<PathBuf, GatewayError> {
        if !workspace.is_absolute() || !workspace.starts_with(&self.runs_root) {
            return Err(GatewayError::WorkspaceOwnership);
        }
        reject_workspace_symlinks(&self.runs_root, workspace)?;
        let canonical = workspace
            .canonicalize()
            .map_err(|source| GatewayError::Io {
                action: "canonicalizing benchmark workspace",
                source,
            })?;
        let runs_root = self
            .runs_root
            .canonicalize()
            .map_err(|source| GatewayError::Io {
                action: "canonicalizing benchmark runs root",
                source,
            })?;
        if runs_root != self.runs_root
            || canonical == runs_root
            || !canonical.starts_with(&runs_root)
            || !canonical.is_dir()
        {
            return Err(GatewayError::WorkspaceOwnership);
        }
        if !has_matching_trial_marker(&canonical, &runs_root, correlation) {
            return Err(GatewayError::WorkspaceOwnership);
        }
        Ok(canonical)
    }
}

fn validate_layerstack_snapshot(
    wire: ProductLayerstackSnapshotWire,
) -> Result<ProductLayerstackSnapshot, GatewayError> {
    if wire.view != "layerstack" || wire.root_hash.is_empty() {
        return Err(GatewayError::ResponseSchema);
    }
    let mut ids = BTreeSet::new();
    for layer in &wire.layers {
        if layer.layer_id.is_empty()
            || !ids.insert(layer.layer_id.as_str())
            || layer
                .booked_by
                .iter()
                .any(|id| id.is_empty() || id == &layer.layer_id)
        {
            return Err(GatewayError::ResponseSchema);
        }
    }
    validate_optional_total(
        wire.total_bytes,
        wire.layers.iter().map(|layer| layer.bytes),
    )?;
    validate_optional_total(
        wire.total_allocated_bytes,
        wire.layers.iter().map(|layer| layer.allocated_bytes),
    )?;
    if matches!(
        (wire.storage_allocated_bytes, wire.total_allocated_bytes),
        (Some(storage), Some(active)) if storage < active
    ) {
        return Err(GatewayError::ResponseSchema);
    }
    Ok(ProductLayerstackSnapshot {
        manifest_version: wire.manifest_version,
        root_hash: wire.root_hash,
        active_lease_count: wire.active_lease_count,
        total_bytes: wire.total_bytes,
        total_allocated_bytes: wire.total_allocated_bytes,
        storage_logical_bytes: wire.storage_logical_bytes,
        storage_allocated_bytes: wire.storage_allocated_bytes,
        staging_entry_count: wire.staging_entry_count,
        layers: wire.layers,
    })
}

fn validate_product_cgroup(
    wire: ProductCgroupWire,
) -> Result<ProductSandboxResources, GatewayError> {
    if wire.view != "cgroup"
        || wire.scope != "sandbox"
        || wire.series.is_empty()
        || wire.series.len() > MAX_PRODUCT_RESOURCE_SAMPLES
    {
        return Err(GatewayError::ResponseSchema);
    }

    let mut previous: Option<&ProductCgroupSampleWire> = None;
    for sample in &wire.series {
        if sample.ts < 0 {
            return Err(GatewayError::ResponseSchema);
        }
        let expected_delta = match previous {
            None => None,
            Some(prior) if sample.ts >= prior.ts => {
                let delta = sample
                    .ts
                    .checked_sub(prior.ts)
                    .ok_or(GatewayError::ResponseSchema)?;
                Some(u64::try_from(delta).map_err(|_| GatewayError::ResponseSchema)?)
            }
            Some(_) => return Err(GatewayError::ResponseSchema),
        };
        let ProductResourceSource::DockerEngine = sample.metrics.metrics_source;
        if sample.sample_delta_ms != expected_delta
            || !valid_product_counter_delta(
                sample.deltas.cpu_usec,
                sample.metrics.cpu_usec,
                previous.and_then(|prior| prior.metrics.cpu_usec),
            )
            || !valid_product_counter_delta(
                sample.deltas.io_rbytes,
                sample.metrics.io_rbytes,
                previous.and_then(|prior| prior.metrics.io_rbytes),
            )
            || !valid_product_counter_delta(
                sample.deltas.io_wbytes,
                sample.metrics.io_wbytes,
                previous.and_then(|prior| prior.metrics.io_wbytes),
            )
        {
            return Err(GatewayError::ResponseSchema);
        }
        previous = Some(sample);
    }

    let latest = wire
        .series
        .last()
        .copied()
        .ok_or(GatewayError::ResponseSchema)?;
    Ok(ProductSandboxResources {
        observed_unix_ms: latest.ts,
        cpu_usage_usec: latest.metrics.cpu_usec,
        memory_current_bytes: latest.metrics.mem_cur,
        memory_limit_bytes: latest.metrics.mem_max,
        io_read_bytes: latest.metrics.io_rbytes,
        io_write_bytes: latest.metrics.io_wbytes,
    })
}

fn validate_product_snapshot(
    wire: ProductSnapshotWire,
    expected_sandbox_id: &str,
) -> Result<ProductStorageResources, GatewayError> {
    if wire.sandbox_id != expected_sandbox_id
        || wire.lifecycle_state != "ready"
        || wire.sampled_at_unix_ms < 0
        || wire.daemon.daemon_pid == 0
        || !Path::new(&wire.daemon.runtime_dir).is_absolute()
        || !wire.resources.history.is_empty()
        || wire.workspaces.len() > MAX_PRODUCT_SNAPSHOT_WORKSPACES
    {
        return Err(GatewayError::ResponseSchema);
    }
    validate_snapshot_sample(wire.resources.latest.as_ref(), wire.sampled_at_unix_ms)?;
    if (wire.availability == ProductSnapshotAvailability::Available && !wire.errors.is_empty())
        || (wire.availability == ProductSnapshotAvailability::Partial && wire.errors.is_empty())
        || wire.errors.iter().any(|error| error.is_empty())
    {
        return Err(GatewayError::ResponseSchema);
    }

    let layerstack_storage_allocated_bytes = match wire.stack {
        Some(stack) => {
            if matches!(
                (stack.storage_allocated_bytes, stack.layers_allocated_bytes),
                (Some(storage), Some(active)) if storage < active
            ) {
                return Err(GatewayError::ResponseSchema);
            }
            let _ = (
                stack.layer_count,
                stack.layers_bytes,
                stack.staging_entry_count,
                stack.active_leases,
            );
            stack.storage_allocated_bytes
        }
        None => None,
    };

    let mut workspace_ids = BTreeSet::new();
    let mut upperdir_total = Some(0_u64);
    let mut upperdir_reason = None;
    for workspace in &wire.workspaces {
        if workspace.workspace_id.is_empty()
            || workspace.lifecycle_state != "active"
            || workspace.network_profile.is_empty()
            || workspace.finalize_policy.is_empty()
            || !workspace.resources.history.is_empty()
            || !workspace_ids.insert(workspace.workspace_id.as_str())
            || workspace
                .layers
                .base_root_hash
                .as_ref()
                .is_some_and(String::is_empty)
            || workspace
                .active_namespace_executions
                .iter()
                .any(|execution| {
                    execution.namespace_execution_id.is_empty()
                        || execution.operation.is_empty()
                        || execution.lifecycle_state != "running"
                })
        {
            return Err(GatewayError::ResponseSchema);
        }
        let _ = (workspace.layers.layer_count, workspace.namespace_fd_count);
        validate_snapshot_sample(workspace.resources.latest.as_ref(), wire.sampled_at_unix_ms)?;
        let Some(latest) = workspace.resources.latest.as_ref() else {
            upperdir_total = None;
            upperdir_reason.get_or_insert_with(|| {
                format!(
                    "workspace {} has no upperdir allocation sample",
                    workspace.workspace_id
                )
            });
            continue;
        };
        if latest.metrics.record_truncated_bytes.is_some()
            || latest.metrics.disk_truncated == Some(true)
        {
            upperdir_total = None;
            upperdir_reason.get_or_insert_with(|| {
                format!(
                    "workspace {} upperdir allocation walk was truncated",
                    workspace.workspace_id
                )
            });
            continue;
        }
        let Some(bytes) = latest.metrics.disk_allocated_bytes else {
            upperdir_total = None;
            upperdir_reason.get_or_insert_with(|| {
                format!(
                    "workspace {} did not report allocated upperdir bytes",
                    workspace.workspace_id
                )
            });
            continue;
        };
        upperdir_total = upperdir_total.and_then(|total| total.checked_add(bytes));
        if upperdir_total.is_none() {
            upperdir_reason.get_or_insert_with(|| {
                "allocated upperdir byte sum overflowed the product counter".to_owned()
            });
        }
    }

    if wire.availability == ProductSnapshotAvailability::Partial {
        upperdir_total = None;
        upperdir_reason = Some(format!(
            "product snapshot was partial: {}",
            wire.errors.join("; ")
        ));
    }
    let upperdir = match upperdir_total {
        Some(allocated_bytes) => ProductUpperdirAllocation::Available {
            allocated_bytes,
            workspace_count: u64::try_from(wire.workspaces.len())
                .map_err(|_| GatewayError::ResponseSchema)?,
        },
        None => ProductUpperdirAllocation::Unavailable {
            reason: upperdir_reason
                .unwrap_or_else(|| "upperdir allocation observation unavailable".to_owned()),
        },
    };

    Ok(ProductStorageResources {
        daemon_container_pid: wire.daemon.daemon_pid,
        layerstack_storage_allocated_bytes,
        upperdir,
    })
}

fn validate_snapshot_sample(
    sample: Option<&ProductSnapshotSampleWire>,
    snapshot_unix_ms: i64,
) -> Result<(), GatewayError> {
    let Some(sample) = sample else {
        return Ok(());
    };
    if sample.ts < 0
        || sample.ts > snapshot_unix_ms
        || sample.sample_delta_ms.is_some_and(|delta| delta < 0)
        || sample
            .metrics
            .cgroup_error
            .as_ref()
            .is_some_and(String::is_empty)
        || sample.metrics.mem_max_unlimited == Some(false)
        || sample.metrics.cgroup_available == Some(true)
        || sample.metrics.disk_truncated == Some(false)
    {
        return Err(GatewayError::ResponseSchema);
    }
    let _ = (
        sample.metrics.cpu_usec,
        sample.metrics.mem_cur,
        sample.metrics.mem_max,
        sample.metrics.disk_bytes,
        sample.metrics.files,
        sample.deltas.cpu_usec,
    );
    Ok(())
}

fn valid_product_counter_delta(
    observed_delta: Option<u64>,
    current: Option<u64>,
    previous: Option<u64>,
) -> bool {
    match (current, previous) {
        (Some(current), Some(previous)) => observed_delta == Some(current.saturating_sub(previous)),
        _ => observed_delta.is_none(),
    }
}

fn validate_optional_total(
    total: Option<u64>,
    values: impl Iterator<Item = Option<u64>>,
) -> Result<(), GatewayError> {
    let mut sum = 0_u64;
    let mut available = true;
    for value in values {
        match value {
            Some(value) => {
                sum = sum.checked_add(value).ok_or(GatewayError::ResponseSchema)?;
            }
            None => available = false,
        }
    }
    let matches_inventory = if available {
        total == Some(sum)
    } else {
        total.is_none()
    };
    if matches_inventory {
        Ok(())
    } else {
        Err(GatewayError::ResponseSchema)
    }
}

fn validate_product_trace(
    wire: ProductTraceWire,
    expected_trace: &str,
) -> Result<ProductTrace, GatewayError> {
    if wire.view != "trace" || wire.trace != expected_trace {
        return Err(GatewayError::ResponseSchema);
    }
    let mut remaining = MAX_PRODUCT_TRACE_NODES;
    let mut span_ids = BTreeSet::new();
    for node in &wire.spans {
        validate_trace_node(node, expected_trace, None, &mut remaining, &mut span_ids)?;
    }
    Ok(ProductTrace {
        trace_id: wire.trace,
        spans: wire.spans,
    })
}

fn validate_trace_node<'a>(
    node: &'a ProductTraceSpanNode,
    expected_trace: &str,
    expected_parent: Option<&str>,
    remaining: &mut usize,
    span_ids: &mut BTreeSet<&'a str>,
) -> Result<(), GatewayError> {
    *remaining = remaining
        .checked_sub(1)
        .ok_or(GatewayError::ResponseSchema)?;
    if node.span.trace != expected_trace
        || node.span.span.is_empty()
        || node.span.name.is_empty()
        || node.span.parent.as_deref() != expected_parent
        || !node.span.dur_ms.is_finite()
        || node.span.dur_ms < 0.0
        || !node.offset_ms.is_finite()
        || node.offset_ms < 0.0
        || !span_ids.insert(node.span.span.as_str())
    {
        return Err(GatewayError::ResponseSchema);
    }
    for event in &node.events {
        if !event.offset_ms.is_finite()
            || event.offset_ms < 0.0
            || event.event.trace != expected_trace
            || event.event.parent.as_deref() != Some(node.span.span.as_str())
            || event.event.name.is_empty()
        {
            return Err(GatewayError::ResponseSchema);
        }
    }
    for child in &node.children {
        validate_trace_node(
            child,
            expected_trace,
            Some(node.span.span.as_str()),
            remaining,
            span_ids,
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
enum PublicRequest {
    ExecCommand {
        sandbox_id: String,
        workspace_session_id: Option<String>,
        command: String,
        timeout_ms: u64,
        yield_time_ms: u64,
    },
    FileRead {
        sandbox_id: String,
        workspace_session_id: Option<String>,
        path: String,
        offset: u64,
        limit: u64,
    },
    FileWrite {
        sandbox_id: String,
        workspace_session_id: Option<String>,
        path: String,
        content: String,
    },
    FileEdit {
        sandbox_id: String,
        workspace_session_id: Option<String>,
        path: String,
        edits: Vec<ProductEdit>,
    },
    FileBlame {
        sandbox_id: String,
        path: String,
    },
    SquashLayerstacks {
        sandbox_id: String,
    },
}

impl PublicRequest {
    const fn operation(&self) -> ProductOperation {
        match self {
            Self::ExecCommand { .. } => ProductOperation::ExecCommand,
            Self::FileRead { .. } => ProductOperation::FileRead,
            Self::FileWrite { .. } => ProductOperation::FileWrite,
            Self::FileEdit { .. } => ProductOperation::FileEdit,
            Self::FileBlame { .. } => ProductOperation::FileBlame,
            Self::SquashLayerstacks { .. } => ProductOperation::SquashLayerstacks,
        }
    }

    fn into_wire(self, correlation: &Correlation) -> OperationRequest {
        let operation = self.operation();
        let (scope, args) = match self {
            Self::ExecCommand {
                sandbox_id,
                workspace_session_id,
                command,
                timeout_ms,
                yield_time_ms,
            } => (
                OperationScope::sandbox(sandbox_id),
                with_optional_workspace_session(
                    json!({
                        "cmd": command,
                        "timeout_ms": timeout_ms,
                        "yield_time_ms": yield_time_ms,
                    }),
                    workspace_session_id,
                ),
            ),
            Self::FileRead {
                sandbox_id,
                workspace_session_id,
                path,
                offset,
                limit,
            } => (
                OperationScope::sandbox(sandbox_id),
                with_optional_workspace_session(
                    json!({
                        "path": path,
                        "offset": offset,
                        "limit": limit,
                    }),
                    workspace_session_id,
                ),
            ),
            Self::FileWrite {
                sandbox_id,
                workspace_session_id,
                path,
                content,
            } => (
                OperationScope::sandbox(sandbox_id),
                with_optional_workspace_session(
                    json!({
                        "path": path,
                        "content": content,
                    }),
                    workspace_session_id,
                ),
            ),
            Self::FileEdit {
                sandbox_id,
                workspace_session_id,
                path,
                edits,
            } => (
                OperationScope::sandbox(sandbox_id),
                with_optional_workspace_session(
                    json!({
                        "path": path,
                        "edits": edits,
                    }),
                    workspace_session_id,
                ),
            ),
            Self::FileBlame { sandbox_id, path } => {
                (OperationScope::sandbox(sandbox_id), json!({ "path": path }))
            }
            Self::SquashLayerstacks { sandbox_id } => (
                OperationScope::system(),
                json!({ "sandbox_id": sandbox_id }),
            ),
        };
        OperationRequest::new(
            operation.catalog_spec().name,
            correlation.wire_request_id(),
            scope,
            args,
        )
    }
}

fn with_optional_workspace_session(mut args: Value, workspace_session_id: Option<String>) -> Value {
    if let (Some(workspace_session_id), Value::Object(fields)) = (workspace_session_id, &mut args) {
        fields.insert(
            "workspace_session_id".to_owned(),
            Value::String(workspace_session_id),
        );
    }
    args
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxRecord {
    pub(crate) id: OwnedSandboxId,
    pub(crate) workspace_root: PathBuf,
    pub(crate) state: SandboxState,
    pub(crate) daemon: Option<DaemonEndpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DaemonEndpoint {
    pub(crate) host: String,
    pub(crate) port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxState {
    Creating,
    Ready,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxListWire {
    sandboxes: Vec<SandboxRecordWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxRecordWire {
    id: String,
    workspace_root: String,
    state: String,
    daemon: Option<DaemonEndpointWire>,
    daemon_http: Option<DaemonEndpointWire>,
    shared_base: Option<SharedBaseWire>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonEndpointWire {
    host: String,
    port: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedBaseWire {
    source: String,
    target: String,
    root_hash: String,
    readonly: bool,
}

impl TryFrom<SandboxRecordWire> for SandboxRecord {
    type Error = GatewayError;

    fn try_from(wire: SandboxRecordWire) -> Result<Self, Self::Error> {
        validate_sandbox_id(&wire.id)?;
        if let Some(http) = wire.daemon_http {
            validate_endpoint(&http)?;
        }
        if let Some(shared) = wire.shared_base {
            if shared.source.is_empty()
                || shared.target.is_empty()
                || shared.root_hash.is_empty()
                || !shared.readonly
            {
                return Err(GatewayError::ResponseSchema);
            }
        }
        let daemon = wire
            .daemon
            .map(|endpoint| {
                validate_endpoint(&endpoint)?;
                Ok(DaemonEndpoint {
                    host: endpoint.host,
                    port: endpoint.port,
                })
            })
            .transpose()?;
        let state = match wire.state.as_str() {
            "creating" => SandboxState::Creating,
            "ready" => SandboxState::Ready,
            "stopping" => SandboxState::Stopping,
            "stopped" => SandboxState::Stopped,
            "failed" => SandboxState::Failed,
            _ => return Err(GatewayError::ResponseSchema),
        };
        Ok(Self {
            id: OwnedSandboxId(wire.id),
            workspace_root: PathBuf::from(wire.workspace_root),
            state,
            daemon,
        })
    }
}

fn validate_endpoint(endpoint: &DaemonEndpointWire) -> Result<(), GatewayError> {
    let loopback = endpoint
        .host
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback());
    if !loopback || endpoint.port == 0 {
        Err(GatewayError::ResponseSchema)
    } else {
        Ok(())
    }
}

fn validate_sandbox_id(value: &str) -> Result<(), GatewayError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        Err(GatewayError::ResponseSchema)
    } else {
        Ok(())
    }
}

fn validate_command_cell(cell: &ExecCommandCell) -> Result<(), GatewayError> {
    let expected = match cell.command_case {
        CommandCase::Noop => "true",
        CommandCase::Output64Kib => "head -c 65536 /dev/zero | tr '\\000' x",
        CommandCase::Cpu50Ms => "i=0; while [ \"$i\" -lt 20000 ]; do i=$((i + 1)); done",
        CommandCase::FixtureRead => "wc -c < .eos-benchmark-fixture/command-read.bin",
    };
    let hash = format!("sha256:{:x}", Sha256::digest(cell.command.as_bytes()));
    if cell.template_revision != 1
        || cell.output_limit_bytes != 65_536
        || cell.command != expected
        || cell.command_sha256 != hash
    {
        return Err(GatewayError::InvalidProductInput(
            "command is not an allowlisted compiled template",
        ));
    }
    Ok(())
}

fn validate_correlation_part(name: &'static str, value: String) -> Result<String, GatewayError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(GatewayError::InvalidCorrelation(name))
    } else {
        Ok(value)
    }
}

fn reject_product_error(value: &Value, auth_token: &str) -> Result<(), GatewayError> {
    let Some(error) = value.get("error") else {
        return Ok(());
    };
    let kind = error
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| {
            !kind.is_empty()
                && kind.len() <= 64
                && kind
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .unwrap_or("unknown")
        .to_owned();
    Err(GatewayError::Product {
        kind,
        detail: sanitized_product_error_detail(error, auth_token),
    })
}

fn sanitized_product_error_detail(error: &Value, auth_token: &str) -> String {
    let Some(message) = error.get("message").and_then(Value::as_str) else {
        return "details unavailable".to_owned();
    };

    let lower = message.to_ascii_lowercase();
    if lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("credential")
        || (!auth_token.is_empty() && message.contains(auth_token))
    {
        return "[redacted sensitive product error detail]".to_owned();
    }

    let mut detail = String::with_capacity(message.len().min(MAX_PRODUCT_ERROR_DETAIL_BYTES));
    let mut previous_space = false;
    for character in message.chars() {
        let character = if character.is_control() || character.is_whitespace() {
            ' '
        } else {
            character
        };
        if character == ' ' {
            if previous_space {
                continue;
            }
            previous_space = true;
        } else {
            previous_space = false;
        }
        detail.push(character);
        if detail.len() > MAX_PRODUCT_ERROR_DETAIL_BYTES {
            let mut end = MAX_PRODUCT_ERROR_DETAIL_BYTES.saturating_sub("...".len());
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            detail.truncate(end);
            detail.push_str("...");
            break;
        }
    }
    let detail = detail.trim();
    if detail.is_empty() {
        "details unavailable".to_owned()
    } else {
        detail.to_owned()
    }
}

fn client_error(error: GatewayClientError) -> GatewayError {
    GatewayError::Client { kind: error.kind() }
}

fn canonical_file(path: &Path, action: &'static str) -> Result<PathBuf, GatewayError> {
    let canonical = path
        .canonicalize()
        .map_err(|source| GatewayError::Io { action, source })?;
    if !canonical.is_file() {
        return Err(GatewayError::InvalidLaunch("binary is not a file"));
    }
    Ok(canonical)
}

fn canonical_owned_directory(path: &Path, action: &'static str) -> Result<PathBuf, GatewayError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| GatewayError::Io { action, source })?;
    let canonical = path
        .canonicalize()
        .map_err(|source| GatewayError::Io { action, source })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || canonical != path {
        return Err(GatewayError::InvalidLaunch(
            "workspace allowlist root is not canonical",
        ));
    }
    Ok(canonical)
}

/// Resolve the repository-owned prebuilt Git toolchains used by the Docker
/// runtime. This is a fixed product prerequisite, not caller-supplied
/// environment injection; rejecting links prevents an isolated gateway from
/// inheriting a toolchain outside the selected repository checkout.
pub fn fixed_git_toolchain_directory(repo: &Path) -> Result<PathBuf, GatewayError> {
    let directory = repo.join(FIXED_GIT_TOOLCHAIN_DIRECTORY);
    let metadata = fs::symlink_metadata(&directory).map_err(|source| GatewayError::Io {
        action: "reading fixed git toolchain directory",
        source,
    })?;
    let canonical = directory
        .canonicalize()
        .map_err(|source| GatewayError::Io {
            action: "resolving fixed git toolchain directory",
            source,
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || canonical != directory {
        return Err(GatewayError::InvalidLaunch(
            "fixed git toolchain directory is not canonical",
        ));
    }
    for archive_name in FIXED_GIT_TOOLCHAIN_ARCHIVES {
        let archive = directory.join(archive_name);
        let metadata = fs::symlink_metadata(&archive).map_err(|source| GatewayError::Io {
            action: "reading fixed git toolchain archive",
            source,
        })?;
        let canonical = archive.canonicalize().map_err(|source| GatewayError::Io {
            action: "resolving fixed git toolchain archive",
            source,
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() == 0
            || canonical != archive
        {
            return Err(GatewayError::InvalidLaunch(
                "fixed git toolchain archive is not a nonempty canonical file",
            ));
        }
    }
    Ok(directory)
}

fn sha256_file(path: &Path) -> Result<String, GatewayError> {
    let mut file = File::open(path).map_err(|source| GatewayError::Io {
        action: "opening file for hashing",
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| GatewayError::Io {
            action: "hashing file",
            source,
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn render_effective_config(
    startup: &StartupConfig,
    destination: &Path,
    daemon_binary: &Path,
    bind_addr: SocketAddr,
    runtime_path: &Path,
    gateway_instance_id: &str,
    remount_sweep_width: u32,
) -> Result<(), GatewayError> {
    let baseline = startup.repo.join(BASE_CONFIG);
    let document =
        sandbox_config::load_path(&baseline).map_err(|_| GatewayError::InvalidEffectiveConfig)?;
    let mut value: Value = document
        .document()
        .map_err(|_| GatewayError::InvalidEffectiveConfig)?;
    let root = value
        .as_object_mut()
        .ok_or(GatewayError::InvalidEffectiveConfig)?;
    root.insert(
        "gateway".to_owned(),
        json!({
            "bind_addr": bind_addr.to_string(),
            "pid_path": runtime_path.join(PID_FILE),
            "max_concurrent_connections": MAX_GATEWAY_CONNECTIONS,
        }),
    );
    let manager = object_field_mut(root, "manager")?;
    manager.insert(
        "registry_path".to_owned(),
        json!(runtime_path.join(REGISTRY_FILE)),
    );
    manager.insert("workspace_roots".to_owned(), json!([startup.paths.runs]));
    let docker = object_field_mut(manager, "docker")?;
    docker.insert("daemon_binary_path".to_owned(), json!(daemon_binary));
    docker.insert("daemon_config_yaml_path".to_owned(), json!(destination));
    docker.insert("gateway_instance_id".to_owned(), json!(gateway_instance_id));
    let runtime = object_field_mut(root, "runtime")?;
    let layerstack = runtime
        .entry("layerstack".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or(GatewayError::InvalidEffectiveConfig)?;
    layerstack.insert("remount_sweep_width".to_owned(), json!(remount_sweep_width));

    let bytes =
        serde_json::to_vec_pretty(&value).map_err(|_| GatewayError::InvalidEffectiveConfig)?;
    write_private_file(destination, &bytes)?;
    let rendered =
        sandbox_config::load_path(destination).map_err(|_| GatewayError::InvalidEffectiveConfig)?;
    let gateway: sandbox_config::configs::gateway::GatewayConfig = rendered
        .section("gateway")
        .map_err(|_| GatewayError::InvalidEffectiveConfig)?;
    let manager: sandbox_config::configs::manager::ManagerConfig = rendered
        .section("manager")
        .map_err(|_| GatewayError::InvalidEffectiveConfig)?;
    let runtime: sandbox_config::configs::runtime::RuntimeConfig = rendered
        .section("runtime")
        .map_err(|_| GatewayError::InvalidEffectiveConfig)?;
    let daemon: sandbox_config::configs::daemon::DaemonConfig = rendered
        .section("daemon")
        .map_err(|_| GatewayError::InvalidEffectiveConfig)?;
    gateway
        .validate()
        .and_then(|()| manager.validate())
        .and_then(|()| runtime.validate())
        .and_then(|()| daemon.validate())
        .map_err(|_| GatewayError::InvalidEffectiveConfig)
}

fn object_field_mut<'a>(
    object: &'a mut Map<String, Value>,
    field: &str,
) -> Result<&'a mut Map<String, Value>, GatewayError> {
    object
        .get_mut(field)
        .and_then(Value::as_object_mut)
        .ok_or(GatewayError::InvalidEffectiveConfig)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), GatewayError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|source| GatewayError::Io {
        action: "creating owner-only effective gateway config",
        source,
    })?;
    file.write_all(bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| GatewayError::Io {
            action: "writing effective gateway config",
            source,
        })
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<(), GatewayError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        GatewayError::Io {
            action: "setting owner-only runtime permissions",
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> Result<(), GatewayError> {
    Ok(())
}

fn copy_process_environment(command: &mut Command) {
    const ALLOWED: &[&str] = &[
        "PATH",
        "HOME",
        "TMPDIR",
        "XDG_RUNTIME_DIR",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ];
    for name in ALLOWED {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

/// Remove only immutable shared-base volumes that the current isolated gateway
/// created. Product teardown removes sandboxes but intentionally keeps this
/// cache for a long-lived gateway; a benchmark gateway is short-lived, so it
/// must not retain the cache beyond its run.
async fn cleanup_owned_shared_base_volumes(gateway_instance_id: &str) -> Result<(), GatewayError> {
    if !valid_benchmark_gateway_instance_id(gateway_instance_id) {
        return Err(GatewayError::CleanupIncomplete);
    }
    let filter = format!("label={GATEWAY_INSTANCE_LABEL}={gateway_instance_id}");
    let mut command = Command::new(DOCKER_BINARY);
    command
        .args(["volume", "ls", "--filter", &filter, "--format", "{{.Name}}"])
        .stdin(Stdio::null())
        .env_clear();
    copy_process_environment(&mut command);
    let output = tokio::time::timeout(SHUTDOWN_TIMEOUT, command.output())
        .await
        .map_err(|_| GatewayError::CleanupIncomplete)?
        .map_err(|_| GatewayError::CleanupIncomplete)?;
    if !output.status.success()
        || output.stdout.len() > MAX_DOCKER_CLEANUP_OUTPUT_BYTES
        || output.stderr.len() > MAX_DOCKER_CLEANUP_OUTPUT_BYTES
    {
        return Err(GatewayError::CleanupIncomplete);
    }
    for volume_name in parse_owned_shared_base_volume_names(&output.stdout)? {
        let mut command = Command::new(DOCKER_BINARY);
        command
            .args(["volume", "rm", &volume_name])
            .stdin(Stdio::null())
            .env_clear();
        copy_process_environment(&mut command);
        let output = tokio::time::timeout(SHUTDOWN_TIMEOUT, command.output())
            .await
            .map_err(|_| GatewayError::CleanupIncomplete)?
            .map_err(|_| GatewayError::CleanupIncomplete)?;
        if !output.status.success()
            || output.stdout.len() > MAX_DOCKER_CLEANUP_OUTPUT_BYTES
            || output.stderr.len() > MAX_DOCKER_CLEANUP_OUTPUT_BYTES
        {
            return Err(GatewayError::CleanupIncomplete);
        }
    }
    Ok(())
}

fn valid_benchmark_gateway_instance_id(value: &str) -> bool {
    value.starts_with("benchmark-gateway-")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn parse_owned_shared_base_volume_names(output: &[u8]) -> Result<Vec<String>, GatewayError> {
    let output = std::str::from_utf8(output).map_err(|_| GatewayError::CleanupIncomplete)?;
    let mut names = BTreeSet::new();
    for volume_name in output.lines() {
        if volume_name.is_empty() {
            continue;
        }
        if !valid_owned_shared_base_volume_name(volume_name)
            || !names.insert(volume_name.to_owned())
            || names.len() > MAX_OWNED_SHARED_BASE_VOLUMES
        {
            return Err(GatewayError::CleanupIncomplete);
        }
    }
    Ok(names.into_iter().collect())
}

fn valid_owned_shared_base_volume_name(value: &str) -> bool {
    let Some(digest) = value.strip_prefix(SHARED_BASE_VOLUME_PREFIX) else {
        return false;
    };
    digest.len() == SHARED_BASE_VOLUME_DIGEST_HEX_LEN
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn pid_ready(path: &Path, expected: u32) -> Result<bool, GatewayError> {
    let value = match tokio::fs::read_to_string(path).await {
        Ok(value) => value,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(GatewayError::Io {
                action: "reading gateway pid ownership file",
                source,
            });
        }
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(false);
    }
    let actual = value
        .parse::<u32>()
        .map_err(|_| GatewayError::PidOwnership)?;
    if actual == expected {
        Ok(true)
    } else {
        Err(GatewayError::PidOwnership)
    }
}

async fn terminate_child(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => {
            if child.start_kill().is_err() {
                return false;
            }
            tokio::time::timeout(SHUTDOWN_TIMEOUT, child.wait())
                .await
                .is_ok_and(|result| result.is_ok())
        }
        Err(_) => false,
    }
}

async fn join_log_task(mut task: JoinHandle<()>) {
    if tokio::time::timeout(LOG_DRAIN_TIMEOUT, &mut task)
        .await
        .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}

fn reject_workspace_symlinks(root: &Path, target: &Path) -> Result<(), GatewayError> {
    let root_metadata = fs::symlink_metadata(root).map_err(|source| GatewayError::Io {
        action: "validating benchmark workspace root ownership",
        source,
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(GatewayError::WorkspaceOwnership);
    }
    let relative = target
        .strip_prefix(root)
        .map_err(|_| GatewayError::WorkspaceOwnership)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|source| GatewayError::Io {
            action: "validating benchmark workspace ownership",
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(GatewayError::WorkspaceOwnership);
        }
    }
    Ok(())
}

fn has_matching_trial_marker(
    workspace: &Path,
    runs_root: &Path,
    correlation: &Correlation,
) -> bool {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Marker {
        schema_version: u32,
        identity: MarkerIdentity,
    }
    #[derive(Deserialize)]
    #[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
    enum MarkerIdentity {
        RunTrial {
            run_id: String,
            trial_id: String,
        },
        Runtime {
            #[serde(rename = "runner_instance_id")]
            _runner_instance_id: String,
        },
    }

    for ancestor in workspace.ancestors().take_while(|path| *path != runs_root) {
        let marker_path = ancestor.join(crate::cleanup::OWNERSHIP_MARKER);
        let Ok(bytes) = fs::read(marker_path) else {
            continue;
        };
        let Ok(marker) = serde_json::from_slice::<Marker>(&bytes) else {
            return false;
        };
        return marker.schema_version == 1
            && matches!(
                marker.identity,
                MarkerIdentity::RunTrial { run_id, trial_id }
                    if run_id == correlation.run_id && trial_id == correlation.trial_id
            );
    }
    false
}

#[derive(Debug, Clone, Copy)]
enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Default)]
struct CapturedLog {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug, Default)]
struct LogCapture {
    stdout: Mutex<CapturedLog>,
    stderr: Mutex<CapturedLog>,
}

impl LogCapture {
    fn append(&self, stream: LogStream, line: &[u8], auth_token: &str) {
        let target = match stream {
            LogStream::Stdout => &self.stdout,
            LogStream::Stderr => &self.stderr,
        };
        let Ok(mut captured) = target.lock() else {
            return;
        };
        let text = String::from_utf8_lossy(line);
        let lower = text.to_ascii_lowercase();
        let sensitive = lower.contains("token")
            || lower.contains("secret")
            || lower.contains("password")
            || lower.contains("credential")
            || (!auth_token.is_empty() && text.contains(auth_token));
        let sanitized = if sensitive {
            "[redacted sensitive gateway log line]\n".to_owned()
        } else {
            text.chars()
                .map(|character| {
                    if character == '\n' || character == '\r' || character == '\t' {
                        character
                    } else if character.is_control() {
                        '\u{fffd}'
                    } else {
                        character
                    }
                })
                .collect()
        };
        let remaining = MAX_LOG_BYTES.saturating_sub(captured.bytes.len());
        if sanitized.len() > remaining {
            captured
                .bytes
                .extend_from_slice(&sanitized.as_bytes()[..remaining]);
            captured.truncated = true;
        } else {
            captured.bytes.extend_from_slice(sanitized.as_bytes());
        }
    }

    fn mark_oversized_line(&self, stream: LogStream) {
        self.append(stream, b"[oversized gateway log line discarded]\n", "");
    }

    fn snapshot(&self) -> GatewayLogs {
        let stdout = self.stdout.lock().ok();
        let stderr = self.stderr.lock().ok();
        GatewayLogs {
            stdout: stdout.as_ref().map_or_else(String::new, |log| {
                String::from_utf8_lossy(&log.bytes).into_owned()
            }),
            stderr: stderr.as_ref().map_or_else(String::new, |log| {
                String::from_utf8_lossy(&log.bytes).into_owned()
            }),
            stdout_truncated: stdout.as_ref().is_some_and(|log| log.truncated),
            stderr_truncated: stderr.as_ref().is_some_and(|log| log.truncated),
        }
    }
}

async fn drain_log<R: AsyncRead + Unpin>(
    mut reader: R,
    capture: Arc<LogCapture>,
    stream: LogStream,
    auth_token: String,
) {
    let mut buffer = [0_u8; 4_096];
    let mut line = Vec::new();
    let mut discarding = false;
    while let Ok(read) = reader.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        for &byte in &buffer[..read] {
            if discarding {
                if byte == b'\n' {
                    discarding = false;
                }
                continue;
            }
            line.push(byte);
            if byte == b'\n' {
                capture.append(stream, &line, &auth_token);
                line.clear();
            } else if line.len() > MAX_LOG_LINE_BYTES {
                capture.mark_oversized_line(stream);
                line.clear();
                discarding = true;
            }
        }
    }
    if !line.is_empty() {
        capture.append(stream, &line, &auth_token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn correlation() -> Correlation {
        Correlation::new("run", "cell", "trial", "request")
            .expect("fixed test correlation must be valid")
    }

    fn available_layerstack_json() -> Value {
        json!({
            "view": "layerstack",
            "manifest_version": 7,
            "root_hash": "root-hash",
            "active_lease_count": 1,
            "total_bytes": 18,
            "total_allocated_bytes": 12_288,
            "storage_logical_bytes": 30,
            "storage_allocated_bytes": 16_384,
            "staging_entry_count": 2,
            "layers": [
                {
                    "layer_id": "layer-a",
                    "bytes": 11,
                    "allocated_bytes": 4_096,
                    "leased_by_workspaces": 1,
                    "booked_by": ["workspace-1"]
                },
                {
                    "layer_id": "layer-b",
                    "bytes": 7,
                    "allocated_bytes": 8_192,
                    "leased_by_workspaces": 0,
                    "booked_by": []
                }
            ]
        })
    }

    fn parse_layerstack(value: Value) -> Result<ProductLayerstackSnapshot, GatewayError> {
        let wire = serde_json::from_value(value).map_err(|_| GatewayError::ResponseSchema)?;
        validate_layerstack_snapshot(wire)
    }

    fn available_cgroup_json() -> Value {
        json!({
            "view": "cgroup",
            "scope": "sandbox",
            "series": [
                {
                    "ts": 1_000,
                    "sample_delta_ms": null,
                    "metrics": {
                        "metrics_source": "docker_engine",
                        "cpu_usec": 100,
                        "mem_cur": 2_048,
                        "mem_max": 4_096,
                        "io_rbytes": 10,
                        "io_wbytes": 20
                    },
                    "deltas": {}
                },
                {
                    "ts": 1_025,
                    "sample_delta_ms": 25,
                    "metrics": {
                        "metrics_source": "docker_engine",
                        "cpu_usec": 160,
                        "mem_cur": 3_072,
                        "mem_max": 4_096,
                        "io_rbytes": 14,
                        "io_wbytes": 29
                    },
                    "deltas": {
                        "cpu_usec": 60,
                        "io_rbytes": 4,
                        "io_wbytes": 9
                    }
                }
            ]
        })
    }

    fn parse_cgroup(value: Value) -> Result<ProductSandboxResources, GatewayError> {
        let wire = serde_json::from_value(value).map_err(|_| GatewayError::ResponseSchema)?;
        validate_product_cgroup(wire)
    }

    fn available_snapshot_json() -> Value {
        json!({
            "sandbox_id": "sandbox",
            "lifecycle_state": "ready",
            "availability": "available",
            "sampled_at_unix_ms": 2_000,
            "errors": [],
            "daemon": {
                "daemon_pid": 42,
                "runtime_dir": "/run/ephemeral-sandbox"
            },
            "resources": {
                "latest": null,
                "history": []
            },
            "workspaces": [
                {
                    "workspace_id": "workspace-1",
                    "lifecycle_state": "active",
                    "network_profile": "isolated",
                    "finalize_policy": "discard",
                    "layers": {
                        "base_root_hash": null,
                        "layer_count": 0
                    },
                    "namespace_fd_count": null,
                    "resources": {
                        "latest": {
                            "ts": 1_990,
                            "sample_delta_ms": 25,
                            "metrics": {
                                "disk_bytes": 3,
                                "disk_allocated_bytes": 4_096,
                                "files": 1
                            },
                            "deltas": {}
                        },
                        "history": []
                    },
                    "active_namespace_executions": []
                },
                {
                    "workspace_id": "workspace-2",
                    "lifecycle_state": "active",
                    "network_profile": "isolated",
                    "finalize_policy": "discard",
                    "layers": {
                        "base_root_hash": "root-a",
                        "layer_count": 1
                    },
                    "namespace_fd_count": 2,
                    "resources": {
                        "latest": {
                            "ts": 1_995,
                            "sample_delta_ms": null,
                            "metrics": {
                                "disk_bytes": 5,
                                "disk_allocated_bytes": 8_192,
                                "files": 1
                            },
                            "deltas": {}
                        },
                        "history": []
                    },
                    "active_namespace_executions": []
                }
            ],
            "stack": {
                "layer_count": 1,
                "layers_bytes": 5,
                "layers_allocated_bytes": 8_192,
                "storage_allocated_bytes": 16_384,
                "staging_entry_count": 0,
                "active_leases": 2
            }
        })
    }

    fn parse_snapshot(value: Value) -> Result<ProductStorageResources, GatewayError> {
        let wire = serde_json::from_value(value).map_err(|_| GatewayError::ResponseSchema)?;
        validate_product_snapshot(wire, "sandbox")
    }

    #[test]
    fn absent_workspace_session_is_omitted_from_product_args() {
        let request = PublicRequest::FileRead {
            sandbox_id: "sandbox".to_owned(),
            workspace_session_id: None,
            path: "fixture.txt".to_owned(),
            offset: 1,
            limit: 16,
        }
        .into_wire(&correlation());

        assert_eq!(request.args.get("workspace_session_id"), None);
    }

    #[test]
    fn product_snapshot_preserves_container_pid_and_sums_allocated_upperdirs() {
        let snapshot = parse_snapshot(available_snapshot_json()).expect("valid snapshot");

        assert_eq!(snapshot.daemon_container_pid, 42);
        assert_eq!(snapshot.layerstack_storage_allocated_bytes, Some(16_384));
        assert_eq!(
            snapshot.upperdir,
            ProductUpperdirAllocation::Available {
                allocated_bytes: 12_288,
                workspace_count: 2,
            }
        );
    }

    #[test]
    fn product_snapshot_marks_incomplete_upperdir_walk_unavailable() {
        let mut value = available_snapshot_json();
        value["workspaces"][0]["resources"]["latest"]["metrics"]["disk_truncated"] = json!(true);

        let snapshot = parse_snapshot(value).expect("truncation is resource unavailability");
        assert!(matches!(
            snapshot.upperdir,
            ProductUpperdirAllocation::Unavailable { reason }
                if reason.contains("workspace-1") && reason.contains("truncated")
        ));
    }

    #[test]
    fn product_snapshot_never_substitutes_logical_for_missing_allocation() {
        let mut value = available_snapshot_json();
        value["workspaces"][0]["resources"]["latest"]["metrics"]
            .as_object_mut()
            .expect("metrics object")
            .remove("disk_allocated_bytes");

        let snapshot = parse_snapshot(value).expect("missing counter is resource unavailability");
        assert!(matches!(
            snapshot.upperdir,
            ProductUpperdirAllocation::Unavailable { reason }
                if reason.contains("workspace-1") && reason.contains("allocated")
        ));
    }

    #[test]
    fn product_snapshot_empty_workspace_set_is_observed_zero() {
        let mut value = available_snapshot_json();
        value["workspaces"] = json!([]);

        let snapshot = parse_snapshot(value).expect("empty active set is valid");
        assert_eq!(
            snapshot.upperdir,
            ProductUpperdirAllocation::Available {
                allocated_bytes: 0,
                workspace_count: 0,
            }
        );
    }

    #[test]
    fn product_snapshot_rejects_unknown_fields_and_identity_mismatch() {
        let mut unknown = available_snapshot_json();
        unknown["workspaces"][0]["resources"]["latest"]["metrics"]["logical_as_allocated"] =
            json!(3);
        assert!(matches!(
            parse_snapshot(unknown),
            Err(GatewayError::ResponseSchema)
        ));

        let mut wrong_identity = available_snapshot_json();
        wrong_identity["sandbox_id"] = json!("another-sandbox");
        assert!(matches!(
            parse_snapshot(wrong_identity),
            Err(GatewayError::ResponseSchema)
        ));
    }

    #[test]
    fn present_workspace_session_is_forwarded_as_a_string() {
        let request = PublicRequest::FileRead {
            sandbox_id: "sandbox".to_owned(),
            workspace_session_id: Some("session-1".to_owned()),
            path: "fixture.txt".to_owned(),
            offset: 1,
            limit: 16,
        }
        .into_wire(&correlation());

        assert_eq!(
            request.args.get("workspace_session_id"),
            Some(&Value::String("session-1".to_owned()))
        );
    }

    #[test]
    fn available_layerstack_values_are_preserved() {
        let snapshot =
            parse_layerstack(available_layerstack_json()).expect("valid snapshot must parse");

        assert_eq!(snapshot.manifest_version, 7);
        assert_eq!(snapshot.root_hash, "root-hash");
        assert_eq!(snapshot.active_lease_count, 1);
        assert_eq!(snapshot.total_bytes, Some(18));
        assert_eq!(snapshot.total_allocated_bytes, Some(12_288));
        assert_eq!(snapshot.storage_logical_bytes, Some(30));
        assert_eq!(snapshot.storage_allocated_bytes, Some(16_384));
        assert_eq!(snapshot.staging_entry_count, Some(2));
        assert_eq!(snapshot.layers[0].bytes, Some(11));
        assert_eq!(snapshot.layers[0].allocated_bytes, Some(4_096));
    }

    #[test]
    fn sandbox_resource_counters_preserve_values_and_absence() {
        let resources =
            parse_cgroup(available_cgroup_json()).expect("valid resource series must parse");
        assert_eq!(resources.observed_unix_ms, 1_025);
        assert_eq!(resources.cpu_usage_usec, Some(160));
        assert_eq!(resources.memory_current_bytes, Some(3_072));
        assert_eq!(resources.memory_limit_bytes, Some(4_096));
        assert_eq!(resources.io_read_bytes, Some(14));
        assert_eq!(resources.io_write_bytes, Some(29));

        let mut unavailable = available_cgroup_json();
        {
            let latest = unavailable["series"][1]["metrics"]
                .as_object_mut()
                .expect("metrics object");
            for key in ["cpu_usec", "io_rbytes", "io_wbytes"] {
                latest.remove(key);
            }
        }
        {
            let deltas = unavailable["series"][1]["deltas"]
                .as_object_mut()
                .expect("deltas object");
            for key in ["cpu_usec", "io_rbytes", "io_wbytes"] {
                deltas.remove(key);
            }
        }
        let resources = parse_cgroup(unavailable).expect("missing counters are valid absence");
        assert_eq!(resources.cpu_usage_usec, None);
        assert_eq!(resources.io_read_bytes, None);
        assert_eq!(resources.io_write_bytes, None);
    }

    #[test]
    fn sandbox_resource_schema_and_history_are_strict() {
        let mut unknown = available_cgroup_json();
        unknown["series"][0]["metrics"]
            .as_object_mut()
            .expect("metrics object")
            .insert("unexpected".to_owned(), json!(0));
        assert!(parse_cgroup(unknown).is_err());

        let mut omitted_nullable = available_cgroup_json();
        omitted_nullable["series"][0]
            .as_object_mut()
            .expect("sample object")
            .remove("sample_delta_ms");
        assert!(serde_json::from_value::<ProductCgroupWire>(omitted_nullable).is_err());

        let mut wrong_source = available_cgroup_json();
        wrong_source["series"][0]["metrics"]["metrics_source"] = json!("daemon");
        assert!(parse_cgroup(wrong_source).is_err());

        let mut bad_interval = available_cgroup_json();
        bad_interval["series"][1]["sample_delta_ms"] = json!(24);
        assert!(parse_cgroup(bad_interval).is_err());

        let mut bad_delta = available_cgroup_json();
        bad_delta["series"][1]["deltas"]["cpu_usec"] = json!(59);
        assert!(parse_cgroup(bad_delta).is_err());

        let mut regressed_time = available_cgroup_json();
        regressed_time["series"][1]["ts"] = json!(999);
        regressed_time["series"][1]["sample_delta_ms"] = Value::Null;
        assert!(parse_cgroup(regressed_time).is_err());
    }

    #[test]
    fn explicit_json_null_remains_unavailable() {
        let mut value = available_layerstack_json();
        let object = value.as_object_mut().expect("fixture is an object");
        object.insert("total_bytes".to_owned(), Value::Null);
        object.insert("total_allocated_bytes".to_owned(), Value::Null);
        object.insert("storage_logical_bytes".to_owned(), Value::Null);
        object.insert("storage_allocated_bytes".to_owned(), Value::Null);
        object.insert("staging_entry_count".to_owned(), Value::Null);
        for layer in object["layers"]
            .as_array_mut()
            .expect("fixture layers are an array")
        {
            let layer = layer.as_object_mut().expect("fixture layer is an object");
            layer.insert("bytes".to_owned(), Value::Null);
            layer.insert("allocated_bytes".to_owned(), Value::Null);
        }

        let snapshot = parse_layerstack(value).expect("explicit null is valid unavailability");
        assert_eq!(snapshot.total_bytes, None);
        assert_eq!(snapshot.total_allocated_bytes, None);
        assert_eq!(snapshot.storage_logical_bytes, None);
        assert_eq!(snapshot.storage_allocated_bytes, None);
        assert_eq!(snapshot.staging_entry_count, None);
        assert!(snapshot
            .layers
            .iter()
            .all(|layer| layer.bytes.is_none() && layer.allocated_bytes.is_none()));
    }

    #[test]
    fn required_nullable_layerstack_keys_cannot_be_omitted() {
        for field in [
            "total_bytes",
            "total_allocated_bytes",
            "storage_logical_bytes",
            "storage_allocated_bytes",
            "staging_entry_count",
        ] {
            let mut value = available_layerstack_json();
            value
                .as_object_mut()
                .expect("fixture is an object")
                .remove(field);
            assert!(serde_json::from_value::<ProductLayerstackSnapshotWire>(value).is_err());
        }
        for field in ["bytes", "allocated_bytes"] {
            let mut value = available_layerstack_json();
            value["layers"][0]
                .as_object_mut()
                .expect("fixture layer is an object")
                .remove(field);
            assert!(serde_json::from_value::<ProductLayerstackSnapshotWire>(value).is_err());
        }
    }

    #[test]
    fn unknown_layerstack_keys_are_rejected_at_both_levels() {
        let mut top_level = available_layerstack_json();
        top_level
            .as_object_mut()
            .expect("fixture is an object")
            .insert("unexpected".to_owned(), json!(true));
        assert!(serde_json::from_value::<ProductLayerstackSnapshotWire>(top_level).is_err());

        let mut nested = available_layerstack_json();
        nested["layers"][0]
            .as_object_mut()
            .expect("fixture layer is an object")
            .insert("unexpected".to_owned(), json!(true));
        assert!(serde_json::from_value::<ProductLayerstackSnapshotWire>(nested).is_err());
    }

    #[test]
    fn duplicate_layers_and_mismatched_totals_are_rejected() {
        let mut duplicate = available_layerstack_json();
        duplicate["layers"][1]["layer_id"] = json!("layer-a");
        assert!(parse_layerstack(duplicate).is_err());

        let mut logical_mismatch = available_layerstack_json();
        logical_mismatch["total_bytes"] = json!(17);
        assert!(parse_layerstack(logical_mismatch).is_err());

        let mut allocation_mismatch = available_layerstack_json();
        allocation_mismatch["total_allocated_bytes"] = json!(12_287);
        assert!(parse_layerstack(allocation_mismatch).is_err());

        let mut false_availability = available_layerstack_json();
        false_availability["layers"][0]["bytes"] = Value::Null;
        false_availability["total_bytes"] = json!(0);
        assert!(parse_layerstack(false_availability).is_err());
    }

    #[test]
    fn known_zero_and_unavailable_totals_are_distinct() {
        let mut empty = available_layerstack_json();
        empty["layers"] = json!([]);
        empty["total_bytes"] = json!(0);
        empty["total_allocated_bytes"] = json!(0);
        let snapshot = parse_layerstack(empty.clone()).expect("known empty inventory is valid");
        assert_eq!(snapshot.total_bytes, Some(0));
        assert_eq!(snapshot.total_allocated_bytes, Some(0));

        empty["total_bytes"] = Value::Null;
        assert!(parse_layerstack(empty).is_err());

        let mut unavailable = available_layerstack_json();
        unavailable["layers"][0]["bytes"] = Value::Null;
        unavailable["total_bytes"] = Value::Null;
        assert_eq!(
            parse_layerstack(unavailable)
                .expect("unknown component makes the total unavailable")
                .total_bytes,
            None
        );
    }

    #[test]
    fn layerstack_total_overflow_is_invalid_not_unavailable() {
        let mut value = available_layerstack_json();
        value["layers"][0]["bytes"] = json!(u64::MAX);
        value["layers"][1]["bytes"] = json!(1);
        value["total_bytes"] = Value::Null;

        assert!(parse_layerstack(value).is_err());
    }

    #[test]
    fn product_paths_reject_escape_and_non_canonical_forms() {
        assert!(ProductPath::new("fixture/data.txt").is_ok());
        assert!(ProductPath::new("../escape").is_err());
        assert!(ProductPath::new("fixture/./data.txt").is_err());
        assert!(ProductPath::new("/absolute").is_err());
        assert!(ProductPath::new(r"fixture\data.txt").is_err());
        assert!(ProductPath::new("fixture/new\nline.txt").is_err());
    }

    #[test]
    fn fixed_git_toolchain_directory_requires_both_nonempty_canonical_archives() {
        let root = std::env::temp_dir()
            .canonicalize()
            .expect("canonical temporary directory")
            .join(format!("eos-benchmark-git-toolchain-{}", Uuid::now_v7()));
        let directory = root.join(FIXED_GIT_TOOLCHAIN_DIRECTORY);
        fs::create_dir_all(&directory).expect("create fixed toolchain directory");
        for archive in FIXED_GIT_TOOLCHAIN_ARCHIVES {
            fs::write(directory.join(archive), b"toolchain").expect("write toolchain archive");
        }

        assert_eq!(
            fixed_git_toolchain_directory(&root).expect("valid fixed toolchain directory"),
            directory
        );
        fs::write(directory.join("linux-amd64.tar"), b"").expect("empty toolchain archive");
        assert!(fixed_git_toolchain_directory(&root).is_err());

        fs::remove_dir_all(root).expect("remove fixed toolchain test directory");
    }

    #[test]
    fn owned_shared_base_cleanup_accepts_only_current_safe_volume_names() {
        let valid = format!("{SHARED_BASE_VOLUME_PREFIX}{}", "a".repeat(64));
        assert_eq!(
            parse_owned_shared_base_volume_names(format!("{valid}\n").as_bytes())
                .expect("strict product volume name is removable"),
            vec![valid]
        );
        assert!(parse_owned_shared_base_volume_names(b"eos-shared-base-ABC\n").is_err());
        assert!(parse_owned_shared_base_volume_names(b"foreign-volume\n").is_err());
        assert!(parse_owned_shared_base_volume_names(
            format!("{SHARED_BASE_VOLUME_PREFIX}{}\n", "a".repeat(63)).as_bytes()
        )
        .is_err());
    }

    #[test]
    fn benchmark_gateway_cleanup_accepts_only_generated_owner_ids() {
        assert!(valid_benchmark_gateway_instance_id(
            "benchmark-gateway-019f5437-7594-7752-84d6-40d182d36b1e"
        ));
        assert!(!valid_benchmark_gateway_instance_id("eos-gateway"));
        assert!(!valid_benchmark_gateway_instance_id(
            "benchmark-gateway-019F5437-7594-7752-84d6-40d182d36b1e"
        ));
    }

    #[test]
    fn product_daemon_endpoints_must_be_numeric_loopback() {
        assert!(validate_endpoint(&DaemonEndpointWire {
            host: "127.0.0.1".to_owned(),
            port: 31_337,
        })
        .is_ok());
        assert!(validate_endpoint(&DaemonEndpointWire {
            host: "::1".to_owned(),
            port: 31_337,
        })
        .is_ok());
        assert!(validate_endpoint(&DaemonEndpointWire {
            host: "localhost".to_owned(),
            port: 31_337,
        })
        .is_err());
        assert!(validate_endpoint(&DaemonEndpointWire {
            host: "192.0.2.1".to_owned(),
            port: 31_337,
        })
        .is_err());
    }

    #[test]
    fn gateway_logs_redact_sensitive_lines() {
        let capture = LogCapture::default();
        capture.append(LogStream::Stdout, b"auth token=top-secret\n", "top-secret");

        let logs = capture.snapshot();
        assert_eq!(logs.stdout, "[redacted sensitive gateway log line]\n");
        assert!(!logs.stdout.contains("top-secret"));
    }

    #[test]
    fn product_error_detail_is_bounded_normalized_and_secret_safe() {
        let error = json!({
            "kind": "internal_error",
            "message": format!("failed\n{}", "界".repeat(MAX_PRODUCT_ERROR_DETAIL_BYTES)),
        });
        let detail = sanitized_product_error_detail(&error, "not-present");
        assert!(detail.starts_with("failed "));
        assert!(detail.ends_with("..."));
        assert!(detail.len() <= MAX_PRODUCT_ERROR_DETAIL_BYTES);
        assert!(!detail.contains('\n'));

        let secret = "benchmark-auth-token";
        let error = json!({
            "kind": "internal_error",
            "message": format!("gateway denied {secret}"),
        });
        let detail = sanitized_product_error_detail(&error, secret);
        assert_eq!(detail, "[redacted sensitive product error detail]");
        assert!(!detail.contains(secret));
    }

    #[test]
    fn product_errors_retain_only_a_sanitized_message() {
        let error = reject_product_error(
            &json!({
                "error": {
                    "kind": "internal_error",
                    "message": "base layer unavailable"
                }
            }),
            "benchmark-auth-token",
        )
        .expect_err("product error must be rejected");
        assert!(matches!(
            error,
            GatewayError::Product { kind, detail }
                if kind == "internal_error" && detail == "base layer unavailable"
        ));
    }
}
