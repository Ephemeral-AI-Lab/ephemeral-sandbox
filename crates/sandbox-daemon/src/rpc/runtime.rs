use std::path::PathBuf;
use std::sync::Arc;

use crate::observability::DaemonObservability;
use sandbox_config::configs::daemon::DaemonHttpForwardConfig;
use sandbox_config::configs::observability::ObservabilityConfig;
use sandbox_observability_telemetry::Observer;
use sandbox_operation_contract::OperationResponse;
use sandbox_protocol::ProtocolLimits;
use sandbox_runtime::{SandboxRuntimeConfig, SandboxRuntimeOperations};
use serde_json::{json, Value};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Where the daemon binds + writes its pid, plus the optional TCP listener.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// `AF_UNIX` socket path (chmod 0o600 after bind).
    pub socket_path: PathBuf,
    /// Pid file path written after the listeners bind.
    pub pid_path: PathBuf,
    /// Optional loopback TCP host (e.g. `127.0.0.1`).
    pub tcp_host: Option<String>,
    /// Optional loopback TCP port; both host+port enable the TCP listener.
    pub tcp_port: Option<u16>,
    /// Optional HTTP listener host; both host+port enable the daemon HTTP
    /// surface, a transport separate from the JSON-line RPC listener.
    pub http_host: Option<String>,
    /// Optional HTTP listener port.
    pub http_port: Option<u16>,
    /// TCP-only auth token; popped from each TCP request before dispatch.
    pub auth_token: Option<String>,
    /// Dynamic sandbox identity supplied by the process manager or serve CLI.
    pub sandbox_id: Option<String>,
    /// The daemon's delegated cgroup v2 root `R`, discovered at startup. `None`
    /// when cgroup v2 is unavailable; the sandbox-wide resource sample is read
    /// here and degrades to unavailable when absent.
    pub cgroup_root: Option<PathBuf>,
    /// Observability emit gate + rotation policy (`observability` config
    /// section); the emit gate maps into the leaf `ObserverConfig`.
    pub observability: ObservabilityConfig,
    /// Request read limits (`daemon.server`), threaded down both listeners'
    /// read paths and the HTTP API body cap.
    pub limits: ProtocolLimits,
    /// RPC connection-permit count (`daemon.server.max_concurrent_connections`).
    pub max_concurrent_connections: usize,
    /// Exact Tokio multi-thread runtime worker count.
    pub worker_threads: usize,
    /// Maximum number of blocking dispatches admitted at once. Requests above
    /// this bound fail immediately instead of entering Tokio's blocking queue.
    pub max_blocking_requests: usize,
    /// Seconds an idle Tokio blocking worker is retained.
    pub blocking_thread_keep_alive_s: f64,
    /// `/forward` reverse-proxy deadlines (`daemon.http.forward`).
    pub forward: DaemonHttpForwardConfig,
}

impl ServerConfig {
    /// The HTTP listener bind `(host, port)`, present only when both the host and
    /// port are configured.
    #[must_use]
    pub(crate) fn http_bind(&self) -> Option<(&str, u16)> {
        match (&self.http_host, self.http_port) {
            (Some(host), Some(port)) => Some((host.as_str(), port)),
            _ => None,
        }
    }
}

/// The running sandbox daemon: request dispatch state and shutdown token.
pub struct SandboxDaemonServer {
    pub(crate) config: ServerConfig,
    pub(crate) operations: Arc<SandboxRuntimeOperations>,
    pub(crate) observability: Option<Arc<DaemonObservability>>,
    pub(crate) shutdown: CancellationToken,
    pub(crate) blocking_admission: BlockingAdmission,
    pub(crate) connection_admission: ConnectionAdmission,
    pub(crate) async_tasks: TaskTracker,
}

#[derive(Clone)]
pub(crate) struct BlockingAdmission {
    permits: Arc<Semaphore>,
    limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    Capacity,
    Closed,
}

impl BlockingAdmission {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }

    pub(crate) fn try_acquire(&self) -> Result<OwnedSemaphorePermit, AdmissionError> {
        match Arc::clone(&self.permits).try_acquire_owned() {
            Ok(permit) => Ok(permit),
            Err(TryAcquireError::NoPermits) => Err(AdmissionError::Capacity),
            Err(TryAcquireError::Closed) => Err(AdmissionError::Closed),
        }
    }

    pub(crate) fn close(&self) {
        self.permits.close();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.permits.is_closed()
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    pub(crate) fn in_use(&self) -> usize {
        self.limit.saturating_sub(self.permits.available_permits())
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionAdmission {
    permits: Arc<Semaphore>,
    limit: usize,
}

impl ConnectionAdmission {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }

    pub(crate) fn try_acquire(&self) -> Result<OwnedSemaphorePermit, AdmissionError> {
        match Arc::clone(&self.permits).try_acquire_owned() {
            Ok(permit) => Ok(permit),
            Err(TryAcquireError::NoPermits) => Err(AdmissionError::Capacity),
            Err(TryAcquireError::Closed) => Err(AdmissionError::Closed),
        }
    }

    pub(crate) fn close(&self) {
        self.permits.close();
    }

    pub(crate) fn in_use(&self) -> usize {
        self.limit.saturating_sub(self.permits.available_permits())
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
    }
}

impl SandboxDaemonServer {
    #[must_use]
    pub fn new_with_runtime_config(
        config: ServerConfig,
        runtime_config: SandboxRuntimeConfig,
    ) -> Self {
        let observability =
            DaemonObservability::from_config(&config, &runtime_config).map(Arc::new);
        let operations = Arc::new(SandboxRuntimeOperations::from_config(
            runtime_config,
            resolve_observer(observability.as_ref()),
        ));
        if let Some(observability) = &observability {
            observability.bind_runtime_operations(Arc::clone(&operations));
        }
        Self {
            blocking_admission: BlockingAdmission::new(config.max_blocking_requests),
            connection_admission: ConnectionAdmission::new(config.max_concurrent_connections),
            async_tasks: TaskTracker::new(),
            config,
            operations,
            observability,
            shutdown: CancellationToken::new(),
        }
    }

    /// A clone of the one process `Observer` (disabled when no observability
    /// stack is configured), used to root the per-request `daemon.dispatch` span.
    pub(crate) fn observer(&self) -> Observer {
        resolve_observer(self.observability.as_ref())
    }
}

/// The one process `Observer`, or a disabled no-op when no observability stack is
/// configured. Resolving in one place keeps the construction and per-request
/// paths on the same handle.
fn resolve_observer(observability: Option<&Arc<DaemonObservability>>) -> Observer {
    observability.map_or_else(Observer::disabled, |observability| observability.observer())
}

pub(crate) fn error_response(
    kind: &'static str,
    message: impl Into<String>,
    details: Value,
) -> OperationResponse {
    OperationResponse::fault_with_details(kind, message, fault_details(details))
}

fn fault_details(details: Value) -> Value {
    match details {
        Value::Null => json!({}),
        Value::Object(fields) if fields.is_empty() => json!({}),
        Value::Object(fields) => json!({ "fields": fields }),
        value => json!({ "fields": { "value": value } }),
    }
}
