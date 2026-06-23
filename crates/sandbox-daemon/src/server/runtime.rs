use std::path::PathBuf;
use std::sync::Arc;

use crate::observability::DaemonObservability;
pub(crate) use sandbox_protocol::{MAX_REQUEST_BYTES, REQUEST_READ_TIMEOUT_S};
use sandbox_runtime::{SandboxRuntimeConfig, SandboxRuntimeOperations};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

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
    /// TCP-only auth token; popped from each TCP request before dispatch.
    pub auth_token: Option<String>,
    /// Dynamic sandbox identity supplied by the process manager or serve CLI.
    pub sandbox_id: Option<String>,
}

/// The running sandbox daemon: request dispatch state and shutdown token.
pub struct SandboxDaemonServer {
    pub(crate) config: ServerConfig,
    pub(crate) operations: Arc<SandboxRuntimeOperations>,
    pub(crate) observability: Option<Arc<DaemonObservability>>,
    pub(crate) shutdown: CancellationToken,
}

impl SandboxDaemonServer {
    /// Assemble a daemon over `config`, wiring the shutdown token.
    #[must_use]
    pub fn new(config: ServerConfig, operations: Arc<SandboxRuntimeOperations>) -> Self {
        let observability = DaemonObservability::from_config(&config).map(Arc::new);
        Self {
            config,
            operations,
            observability,
            shutdown: CancellationToken::new(),
        }
    }

    #[must_use]
    pub fn new_with_runtime_config(
        config: ServerConfig,
        runtime_config: SandboxRuntimeConfig,
    ) -> Self {
        let observability = DaemonObservability::from_config(&config).map(Arc::new);
        let async_trace_sink = observability
            .as_ref()
            .map(|observability| DaemonObservability::async_trace_sink(Arc::clone(observability)));
        let operations = Arc::new(SandboxRuntimeOperations::from_config_with_async_trace_sink(
            runtime_config,
            async_trace_sink,
        ));
        Self {
            config,
            operations,
            observability,
            shutdown: CancellationToken::new(),
        }
    }

    pub(crate) fn trigger_observability_collection(&self) {
        let Some(observability) = self.observability.clone() else {
            return;
        };
        let config = self.config.clone();
        let operations = Arc::clone(&self.operations);
        let handle = tokio::task::spawn_blocking(move || {
            let _ = observability.collect(&config, &operations);
        });
        drop(handle);
    }
}

pub(crate) fn error_response(
    kind: &'static str,
    message: impl Into<String>,
    details: Value,
) -> Value {
    sandbox_protocol::response::error_response_with_details(kind, message, fault_details(details))
}

fn fault_details(details: Value) -> Value {
    match details {
        Value::Null => json!({}),
        Value::Object(fields) if fields.is_empty() => json!({}),
        Value::Object(fields) => json!({ "fields": fields }),
        value => json!({ "fields": { "value": value } }),
    }
}
