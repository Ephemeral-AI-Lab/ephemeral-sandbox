//! Host-side daemon transport: serialize one JSON envelope per call, send it to
//! the resident in-sandbox daemon (TCP fast path or `AF_UNIX` thin client through
//! `adapter.exec`), run the spawn/connect/empty-response recovery state machine,
//! cache the per-sandbox TCP endpoint with single-flight, and decode typed
//! errors. Faithful port of `sandbox/host/daemon_client.py`.
//!
//! Per GC-04 the Rust default runtime is `eosd`; the Python-vs-`eosd` command
//! branching collapses to the `eosd` branch (the Python launcher survives only
//! behind the compat bridge, not implemented here).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eos_sandbox_api::{DaemonOp, SandboxApiError, SandboxTransport};
use eos_types::{JsonObject, SandboxId};
use parking_lot::RwLock;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::SandboxHostError;
use crate::provider::{DaemonTcpEndpoint, ExecOpts, ProviderAdapter};
use crate::registry::ProviderRegistry;
use crate::runtime_artifact::EOSD_VERSION;

// --- wire protocol constants (verbatim from daemon_client.py) -----------------

/// The wire protocol version the host speaks. Lockstep-asserted equal to
/// [`crate::runtime_artifact::PROTOCOL_VERSION`] at compile time (AC-08).
pub const DAEMON_PROTOCOL_VERSION: u32 = 1;
const DAEMON_PROTOCOL_FIELD: &str = "_eos_daemon_protocol_version";
const DAEMON_AUTH_FIELD: &str = "_eos_daemon_auth_token";
const THIN_CLIENT_CONNECT_FAILED: i32 = 97;
const THIN_CLIENT_IO_FAILED: i32 = 98;
const EMPTY_RESPONSE_MESSAGE: &str = "EOS_DAEMON_IO_FAILED:empty_response";
const DAEMON_SPAWN_TIMEOUT_S: u32 = 20;
const READINESS_TIMEOUT_S: u32 = 30;
const TCP_DEFAULT_TIMEOUT_S: u32 = 60;

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}
const CONNECT_RETRY_DELAYS: [Duration; 4] = [ms(250), ms(500), ms(1000), ms(2000)];

// --- resolved container-side paths (from sandbox/daemon/paths.py) -------------

pub(crate) const BUNDLE_REMOTE_DIR: &str = "/eos/daemon";
/// Default `LayerStack` root injected into every envelope's `args.layer_stack_root`.
pub const DEFAULT_LAYER_STACK_ROOT: &str = "/eos/layer-stack";
const DAEMON_SOCKET_PATH: &str = "/eos/daemon/runtime.sock";
const DAEMON_PID_PATH: &str = "/eos/daemon/runtime.pid";
const DAEMON_LOG_PATH: &str = "/eos/daemon/runtime.log";
const DAEMON_ENV_SIGNATURE_PATH: &str = "/eos/daemon/runtime.env";
pub(crate) const EOSD_REMOTE_PATH: &str = "/eos/daemon/eosd";
pub(crate) const EOSD_SHA_MARKER: &str = "/eos/daemon/.eosd-sha256";

// --- public helpers -----------------------------------------------------------

/// Prepend the wire protocol-version field to a payload (payload wins on a key
/// collision). The `eos-sandbox-host` [`SandboxTransport`] impl applies this
/// before dispatch (Python `with_daemon_protocol_version`).
#[must_use]
pub fn with_daemon_protocol_version(payload: JsonObject) -> JsonObject {
    let mut out = JsonObject::new();
    out.insert(
        DAEMON_PROTOCOL_FIELD.to_owned(),
        Value::from(DAEMON_PROTOCOL_VERSION),
    );
    out.extend(payload);
    out
}

/// The daemon-backed [`SandboxTransport`] implementor: resolves the provider
/// adapter, runs the recovery state machine, decodes the typed response, and
/// owns the per-sandbox TCP-endpoint cache + single-flight locks.
#[derive(Debug)]
pub struct DaemonClient {
    registry: Arc<ProviderRegistry>,
    /// `Some(None)` is a valid negative-cache entry (adapter has no TCP path);
    /// absence means "not yet resolved". Sync read/insert (`own-rwlock-readers`).
    tcp_cache: RwLock<HashMap<SandboxId, Option<DaemonTcpEndpoint>>>,
    /// Per-sandbox single-flight guards — the one `tokio::sync::Mutex` in this
    /// crate, deliberately held across the async resolve round-trip (spec §7).
    tcp_locks: RwLock<HashMap<SandboxId, Arc<tokio::sync::Mutex<()>>>>,
}

impl DaemonClient {
    /// Build a daemon client over a shared provider registry.
    #[must_use]
    pub fn new(registry: Arc<ProviderRegistry>) -> Self {
        Self {
            registry,
            tcp_cache: RwLock::new(HashMap::new()),
            tcp_locks: RwLock::new(HashMap::new()),
        }
    }

    /// The shared provider registry this client dispatches through.
    #[must_use]
    pub fn registry(&self) -> &Arc<ProviderRegistry> {
        &self.registry
    }

    /// Drop any cached TCP endpoint for `sandbox_id` (Python
    /// `invalidate_daemon_tcp_endpoint`).
    pub fn invalidate_daemon_tcp_endpoint(&self, sandbox_id: &SandboxId) {
        self.tcp_cache.write().remove(sandbox_id);
    }

    /// Dispatch one daemon op and return the decoded response object.
    ///
    /// `op` is the verbatim wire op string (e.g. `api.v1.read_file`,
    /// `api.ensure_workspace_base`). `args` are merged over the injected
    /// `layer_stack_root`; `args` win on collision (Python `call_daemon_api`).
    pub async fn call_daemon_api(
        &self,
        sandbox_id: &SandboxId,
        op: &str,
        args: JsonObject,
        timeout_s: u32,
        layer_stack_root: &str,
    ) -> Result<JsonObject, SandboxHostError> {
        let mut daemon_args = JsonObject::new();
        daemon_args.insert(
            "layer_stack_root".to_owned(),
            Value::String(layer_stack_root.to_owned()),
        );
        daemon_args.extend(args);

        let adapter = self.registry.adapter(sandbox_id)?;
        let tcp_endpoint = self
            .resolve_daemon_tcp_endpoint(&*adapter, sandbox_id)
            .await;
        self.call_daemon(
            &*adapter,
            sandbox_id,
            op,
            daemon_args,
            timeout_s,
            tcp_endpoint.as_ref(),
        )
        .await
    }

    /// Re-spawn the resident daemon (Python `ensure_daemon_current`). The eosd
    /// spawn restarts the daemon when its env signature changes. Note: unlike the
    /// stale doc comment in the Python source, this does **not** invalidate the
    /// TCP cache (invalidation is `_send_daemon_envelope`-only — see the port
    /// discrepancy note).
    pub async fn ensure_daemon_current(
        &self,
        sandbox_id: &SandboxId,
        timeout_s: u32,
    ) -> Result<(), SandboxHostError> {
        let adapter = self.registry.adapter(sandbox_id)?;
        let tcp_endpoint = self
            .resolve_daemon_tcp_endpoint(&*adapter, sandbox_id)
            .await;
        let command = daemon_spawn_command(tcp_endpoint.as_ref());
        let result = adapter
            .exec(
                sandbox_id,
                &command,
                &exec_opts(BUNDLE_REMOTE_DIR, timeout_s),
            )
            .await?;
        if result.exit_code != 0 {
            return Err(exec_failed(&result));
        }
        Ok(())
    }

    // --- envelope build + decode (AC-03 / AC-05) ------------------------------

    async fn call_daemon(
        &self,
        adapter: &dyn ProviderAdapter,
        sandbox_id: &SandboxId,
        op: &str,
        args: JsonObject,
        timeout_s: u32,
        tcp_endpoint: Option<&DaemonTcpEndpoint>,
    ) -> Result<JsonObject, SandboxHostError> {
        let mut clean_args = without_none(args);
        // invocation_id: fresh for cancel; else reuse a present truthy one or mint
        // (and write it back into args for non-cancel ops).
        let invocation_id = if op == "api.v1.cancel" {
            new_invocation_id()
        } else {
            let id = clean_args
                .get("invocation_id")
                .and_then(truthy_to_string)
                .unwrap_or_else(new_invocation_id);
            clean_args.insert("invocation_id".to_owned(), Value::String(id.clone()));
            id
        };
        let envelope_json = serialize_envelope(op, &invocation_id, &clean_args);
        let result = self
            .dispatch_with_daemon_spawn_recovery(
                adapter,
                sandbox_id,
                op,
                &clean_args,
                &envelope_json,
                timeout_s,
                tcp_endpoint,
            )
            .await?;
        decode_and_classify(&result)
    }

    // --- recovery state machine (AC-04) ---------------------------------------

    #[allow(clippy::too_many_arguments)] // a faithful port of one Python function's params
    async fn dispatch_with_daemon_spawn_recovery(
        &self,
        adapter: &dyn ProviderAdapter,
        sandbox_id: &SandboxId,
        op: &str,
        args: &JsonObject,
        envelope_json: &str,
        timeout_s: u32,
        tcp_endpoint: Option<&DaemonTcpEndpoint>,
    ) -> Result<crate::provider::RawExecResult, SandboxHostError> {
        // STEP 1 — first attempt.
        let result = self
            .send_daemon_envelope(adapter, sandbox_id, envelope_json, timeout_s, tcp_endpoint)
            .await?;

        // STEP 2 — recover iff CONNECT_FAILED (not op-gated) OR empty-response on
        // a retry-eligible op. A mutating op with an empty response fails closed.
        if result.exit_code != THIN_CLIENT_CONNECT_FAILED
            && !(is_empty_response(&result) && can_retry_empty_response(op))
        {
            return Ok(result);
        }

        // STEP 3 — spawn the daemon.
        let spawn_command = daemon_spawn_command(tcp_endpoint);
        let spawn_result = adapter
            .exec(
                sandbox_id,
                &spawn_command,
                &exec_opts(BUNDLE_REMOTE_DIR, DAEMON_SPAWN_TIMEOUT_S),
            )
            .await?;
        if spawn_result.exit_code != 0 {
            return Ok(spawn_result);
        }

        // STEP 4 — readiness requires a layer_stack_root.
        let layer_stack_root = args
            .get("layer_stack_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SandboxHostError::DaemonDispatch {
                kind: "MissingLayerStackRoot".to_owned(),
                message: "daemon readiness check requires layer_stack_root".to_owned(),
                details: detail(&[("op", op)]),
            })?
            .to_owned();

        // STEP 5 — readiness probe with connect-retry (fresh id, fixed 30s,
        // unconditional empty-response retry — readiness is a control op).
        let mut ready_args = JsonObject::new();
        ready_args.insert(
            "layer_stack_root".to_owned(),
            Value::String(layer_stack_root),
        );
        let readiness_json =
            serialize_envelope("api.runtime.ready", &new_invocation_id(), &ready_args);
        let readiness_result = self
            .call_daemon_envelope_with_connect_retry(
                adapter,
                sandbox_id,
                &readiness_json,
                READINESS_TIMEOUT_S,
                tcp_endpoint,
                true,
            )
            .await?;

        // STEP 6 — readiness result handling (ANY error raises; no policy gate).
        if readiness_result.exit_code != 0 {
            let mut details = detail(&[("original_op", op)]);
            details.insert(
                "exit_code".to_owned(),
                Value::from(readiness_result.exit_code),
            );
            return Err(SandboxHostError::DaemonDispatch {
                kind: "RuntimeReadinessFailed".to_owned(),
                message: stderr_or_stdout(&readiness_result),
                details,
            });
        }
        let response = match decode_response(&readiness_result.stdout) {
            Ok(response) => response,
            Err(_) => {
                let mut details = detail(&[("original_op", op)]);
                details.insert(
                    "stdout".to_owned(),
                    Value::String(readiness_result.stdout.clone()),
                );
                return Err(SandboxHostError::DaemonDispatch {
                    kind: "BadRuntimeReadinessResponse".to_owned(),
                    message: "daemon returned invalid JSON".to_owned(),
                    details,
                });
            }
        };
        if let Some(error) = response.get("error").filter(|v| !v.is_null()) {
            return Err(readiness_error_from_value(error, op));
        }

        // STEP 7 — ready flag (must be exactly `true`), with the bootstrap fall-through.
        if response.get("ready") != Some(&Value::Bool(true)) {
            if is_bootstrap_ready_response(op, &response) {
                tracing::warn!(
                    op,
                    "daemon-readiness: declaring op ready despite control_plane WorkspaceBindingError"
                );
            } else {
                let mut details = JsonObject::new();
                details.insert("response".to_owned(), Value::Object(response));
                details.insert("original_op".to_owned(), Value::String(op.to_owned()));
                return Err(SandboxHostError::DaemonNotReady { details });
            }
        }

        // STEP 8 — replay the original envelope (op-gated empty-response retry).
        self.call_daemon_envelope_with_connect_retry(
            adapter,
            sandbox_id,
            envelope_json,
            timeout_s,
            tcp_endpoint,
            can_retry_empty_response(op),
        )
        .await
    }

    async fn call_daemon_envelope_with_connect_retry(
        &self,
        adapter: &dyn ProviderAdapter,
        sandbox_id: &SandboxId,
        envelope_json: &str,
        timeout_s: u32,
        tcp_endpoint: Option<&DaemonTcpEndpoint>,
        retry_empty_response: bool,
    ) -> Result<crate::provider::RawExecResult, SandboxHostError> {
        // 4 retry attempts each followed by its delay, then one final attempt
        // (total 5 sends, 4 sleeps); the 5th is returned unconditionally.
        for delay in CONNECT_RETRY_DELAYS {
            let result = self
                .send_daemon_envelope(adapter, sandbox_id, envelope_json, timeout_s, tcp_endpoint)
                .await?;
            if result.exit_code != THIN_CLIENT_CONNECT_FAILED
                && !(retry_empty_response && is_empty_response(&result))
            {
                return Ok(result);
            }
            tokio::time::sleep(delay).await;
        }
        self.send_daemon_envelope(adapter, sandbox_id, envelope_json, timeout_s, tcp_endpoint)
            .await
    }

    // --- send path (TCP-first, AF_UNIX fallback) ------------------------------

    async fn send_daemon_envelope(
        &self,
        adapter: &dyn ProviderAdapter,
        sandbox_id: &SandboxId,
        envelope_json: &str,
        timeout_s: u32,
        tcp_endpoint: Option<&DaemonTcpEndpoint>,
    ) -> Result<crate::provider::RawExecResult, SandboxHostError> {
        if let Some(endpoint) = tcp_endpoint {
            let tcp_result = call_tcp_daemon(endpoint, envelope_json, timeout_s).await;
            if is_empty_response(&tcp_result) {
                self.invalidate_daemon_tcp_endpoint(sandbox_id);
                return Ok(tcp_result);
            }
            if tcp_result.exit_code != THIN_CLIENT_CONNECT_FAILED {
                return Ok(tcp_result);
            }
            // CONNECT_FAILED → drop cache, fall through to the AF_UNIX thin client.
            self.invalidate_daemon_tcp_endpoint(sandbox_id);
        }
        let command = daemon_thin_client_command(envelope_json);
        adapter
            .exec(
                sandbox_id,
                &command,
                &exec_opts(BUNDLE_REMOTE_DIR, timeout_s),
            )
            .await
    }

    // --- TCP endpoint resolution + single-flight (AC-07b) ---------------------

    /// Resolve (and cache) the per-sandbox TCP endpoint, single-flighting
    /// concurrent callers. Returns `None` when the adapter has no TCP path; a
    /// resolver **error** returns `None` without caching (so the next call
    /// retries — intentional asymmetry, matching Python).
    pub(crate) async fn resolve_daemon_tcp_endpoint(
        &self,
        adapter: &dyn ProviderAdapter,
        sandbox_id: &SandboxId,
    ) -> Option<DaemonTcpEndpoint> {
        // (1) fast-path cache read — clone out, drop the guard before any await.
        {
            let cache = self.tcp_cache.read();
            if let Some(cached) = cache.get(sandbox_id) {
                return cached.clone();
            }
        }
        // (2) get-or-insert the per-sandbox async mutex (parking_lot guard dropped immediately).
        let lock = {
            let mut locks = self.tcp_locks.write();
            Arc::clone(
                locks
                    .entry(sandbox_id.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        // (3) acquire the async mutex — held across the resolve await (the one
        // legitimate must-span-await lock in this crate).
        let _guard = lock.lock().await;
        // (4) re-check the cache under the single-flight guard.
        {
            let cache = self.tcp_cache.read();
            if let Some(cached) = cache.get(sandbox_id) {
                return cached.clone();
            }
        }
        // (5) resolve once.
        let endpoint = match adapter.daemon_tcp_endpoint(sandbox_id).await {
            Ok(endpoint) => endpoint,
            Err(_) => return None, // resolver error → None WITHOUT caching
        };
        // (6) publish under the write guard.
        self.tcp_cache
            .write()
            .insert(sandbox_id.clone(), endpoint.clone());
        endpoint
    }
}

#[async_trait]
impl SandboxTransport for DaemonClient {
    async fn call(
        &self,
        sandbox_id: &SandboxId,
        op: DaemonOp,
        payload: JsonObject,
        timeout_s: u32,
    ) -> Result<JsonObject, SandboxApiError> {
        let payload = with_daemon_protocol_version(payload);
        self.call_daemon_api(
            sandbox_id,
            op.as_wire(),
            payload,
            timeout_s,
            DEFAULT_LAYER_STACK_ROOT,
        )
        .await
        .map_err(map_host_error_to_api_error)
    }
}

// --- free helpers (pub(crate) for tests; private otherwise) -------------------

fn map_host_error_to_api_error(err: SandboxHostError) -> SandboxApiError {
    match err {
        SandboxHostError::DaemonDispatch { kind, message, .. } => {
            SandboxApiError::transport(Some(kind), message)
        }
        SandboxHostError::ExecFailed { exit_code, message } => SandboxApiError::transport(
            Some("RuntimeExecFailed".to_owned()),
            format!("exit {exit_code}: {message}"),
        ),
        SandboxHostError::DaemonNotReady { .. } => {
            SandboxApiError::transport(Some("RuntimeNotReady".to_owned()), "daemon not ready")
        }
        SandboxHostError::BadResponse { stdout } => SandboxApiError::transport(
            Some("BadRuntimeResponse".to_owned()),
            format!("daemon returned invalid response: {stdout}"),
        ),
        other => SandboxApiError::transport(None, other.to_string()),
    }
}

fn new_invocation_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn exec_opts(cwd: &str, timeout_s: u32) -> ExecOpts {
    ExecOpts {
        cwd: Some(cwd.to_owned()),
        timeout: Some(Duration::from_secs(u64::from(timeout_s))),
    }
}

fn without_none(args: JsonObject) -> JsonObject {
    args.into_iter().filter(|(_, v)| !v.is_null()).collect()
}

/// Python `str(x or default)` truthiness: returns `Some(string)` for a truthy
/// value, `None` for a falsy one (null / false / "" / 0 / empty container).
fn truthy_to_string(value: &Value) -> Option<String> {
    match value {
        Value::Null | Value::Bool(false) => None,
        Value::Bool(true) => Some("True".to_owned()),
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => {
            if n.as_f64() == Some(0.0) {
                None
            } else {
                Some(n.to_string())
            }
        }
        Value::Array(a) if a.is_empty() => None,
        Value::Object(o) if o.is_empty() => None,
        other => Some(other.to_string()),
    }
}

fn serialize_envelope(op: &str, invocation_id: &str, args: &JsonObject) -> String {
    let mut envelope = JsonObject::new();
    envelope.insert("op".to_owned(), Value::String(op.to_owned()));
    envelope.insert(
        "invocation_id".to_owned(),
        Value::String(invocation_id.to_owned()),
    );
    envelope.insert("args".to_owned(), Value::Object(args.clone()));
    serde_json::to_string(&Value::Object(envelope)).expect("envelope serializes")
}

fn detail(pairs: &[(&str, &str)]) -> JsonObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
        .collect()
}

fn stderr_or_stdout(result: &crate::provider::RawExecResult) -> String {
    if result.stderr.is_empty() {
        result.stdout.clone()
    } else {
        result.stderr.clone()
    }
}

fn exec_failed(result: &crate::provider::RawExecResult) -> SandboxHostError {
    SandboxHostError::ExecFailed {
        exit_code: result.exit_code,
        message: stderr_or_stdout(result),
    }
}

fn is_empty_response(result: &crate::provider::RawExecResult) -> bool {
    result.exit_code == THIN_CLIENT_IO_FAILED && result.stderr == EMPTY_RESPONSE_MESSAGE
}

fn can_retry_empty_response(op: &str) -> bool {
    !matches!(
        op,
        "api.edit_file"
            | "api.v1.edit_file"
            | "api.write_file"
            | "api.v1.write_file"
            | "api.v1.exec_command"
            | "api.v1.exec_stdin"
    ) && !op.starts_with("plugin.")
}

fn decode_response(stdout: &str) -> Result<JsonObject, SandboxHostError> {
    let value: Value =
        serde_json::from_str(stdout.trim()).map_err(|_| SandboxHostError::BadResponse {
            stdout: stdout.to_owned(),
        })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(SandboxHostError::BadResponse {
            stdout: stdout.to_owned(),
        }),
    }
}

fn is_handler_level_error_result(response: &JsonObject) -> bool {
    response.get("success") == Some(&Value::Bool(false))
        && matches!(response.get("status"), Some(Value::String(s)) if !s.trim().is_empty())
}

pub(crate) fn decode_and_classify(
    result: &crate::provider::RawExecResult,
) -> Result<JsonObject, SandboxHostError> {
    let response = match decode_response(&result.stdout) {
        Ok(response) => response,
        Err(bad) => {
            // ExecFailed wins over BadResponse when the exec itself failed.
            if result.exit_code != 0 {
                return Err(exec_failed(result));
            }
            return Err(bad);
        }
    };
    if let Some(error) = response.get("error") {
        if !error.is_null() && !is_handler_level_error_result(&response) {
            return Err(dispatch_error_from_value(error));
        }
    }
    if result.exit_code != 0 {
        return Err(exec_failed(result));
    }
    Ok(response)
}

fn dispatch_error_from_value(error: &Value) -> SandboxHostError {
    match error {
        Value::Object(map) => SandboxHostError::DaemonDispatch {
            kind: map
                .get("kind")
                .and_then(truthy_to_string)
                .unwrap_or_else(|| "RuntimeError".to_owned()),
            message: map
                .get("message")
                .and_then(truthy_to_string)
                .unwrap_or_default(),
            details: match map.get("details") {
                Some(Value::Object(d)) => d.clone(),
                _ => JsonObject::new(),
            },
        },
        other => SandboxHostError::DaemonDispatch {
            kind: "RuntimeError".to_owned(),
            message: plain_string(other),
            details: JsonObject::new(),
        },
    }
}

fn readiness_error_from_value(error: &Value, op: &str) -> SandboxHostError {
    let (kind, message, mut details) = match error {
        Value::Object(map) => (
            map.get("kind")
                .and_then(truthy_to_string)
                .unwrap_or_else(|| "RuntimeReadinessFailed".to_owned()),
            map.get("message")
                .and_then(truthy_to_string)
                .unwrap_or_default(),
            match map.get("details") {
                Some(Value::Object(d)) => d.clone(),
                _ => JsonObject::new(),
            },
        ),
        other => (
            "RuntimeReadinessFailed".to_owned(),
            plain_string(other),
            JsonObject::new(),
        ),
    };
    details.insert("original_op".to_owned(), Value::String(op.to_owned()));
    SandboxHostError::DaemonDispatch {
        kind,
        message,
        details,
    }
}

pub(crate) fn plain_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The bootstrap fall-through: for the two workspace-base ops, treat the daemon
/// as ready despite a `control_plane` probe `down` with `WorkspaceBindingError`,
/// provided every other probe is `ok`.
fn is_bootstrap_ready_response(op: &str, response: &JsonObject) -> bool {
    if op != "api.ensure_workspace_base" && op != "api.build_workspace_base" {
        return false;
    }
    let probes = match response.get("probes") {
        Some(Value::Array(probes)) => probes,
        _ => return false,
    };
    // by_name: last writer wins (matches the Python dict build).
    let mut by_name: BTreeMap<&str, &JsonObject> = BTreeMap::new();
    for probe in probes {
        if let Value::Object(map) = probe {
            if let Some(name) = map.get("name").and_then(Value::as_str) {
                by_name.insert(name, map);
            }
        }
    }
    let control_plane = match by_name.get("control_plane") {
        Some(cp) => *cp,
        None => return false,
    };
    let details = match control_plane.get("details") {
        Some(Value::Object(details)) => details,
        _ => return false,
    };
    if control_plane.get("status").and_then(Value::as_str) != Some("down") {
        return false;
    }
    if details.get("error_type").and_then(Value::as_str) != Some("WorkspaceBindingError") {
        return false;
    }
    by_name
        .iter()
        .filter(|(name, _)| **name != "control_plane")
        .all(|(_, probe)| probe.get("status").and_then(Value::as_str) == Some("ok"))
}

// --- TCP socket I/O (never raises; returns synthetic exit codes 97/98) --------

enum TcpError {
    Connect(String),
    Io(String),
}

fn io_token(err: &std::io::Error) -> String {
    format!("{:?}", err.kind())
}

async fn call_tcp_daemon(
    endpoint: &DaemonTcpEndpoint,
    envelope_json: &str,
    timeout_s: u32,
) -> crate::provider::RawExecResult {
    let client_timeout = Duration::from_secs(u64::from(if timeout_s == 0 {
        TCP_DEFAULT_TIMEOUT_S
    } else {
        timeout_s
    }));
    let authed = authenticated_envelope_json(envelope_json, endpoint);
    match tokio::time::timeout(client_timeout, call_tcp_daemon_inner(endpoint, &authed)).await {
        Ok(Ok(stdout)) => {
            if stdout.trim().is_empty() {
                io_failed(THIN_CLIENT_IO_FAILED, EMPTY_RESPONSE_MESSAGE.to_owned())
            } else {
                crate::provider::RawExecResult {
                    exit_code: 0,
                    stdout,
                    stderr: String::new(),
                    success: true,
                }
            }
        }
        Ok(Err(TcpError::Connect(token))) => io_failed(
            THIN_CLIENT_CONNECT_FAILED,
            format!("EOS_DAEMON_CONNECT_FAILED:{token}"),
        ),
        Ok(Err(TcpError::Io(token))) => io_failed(
            THIN_CLIENT_IO_FAILED,
            format!("EOS_DAEMON_IO_FAILED:{token}"),
        ),
        Err(_elapsed) => io_failed(
            THIN_CLIENT_IO_FAILED,
            "EOS_DAEMON_IO_FAILED:Elapsed".to_owned(),
        ),
    }
}

fn io_failed(exit_code: i32, stderr: String) -> crate::provider::RawExecResult {
    crate::provider::RawExecResult {
        exit_code,
        stdout: String::new(),
        stderr,
        success: false,
    }
}

async fn call_tcp_daemon_inner(
    endpoint: &DaemonTcpEndpoint,
    envelope_json: &str,
) -> Result<String, TcpError> {
    let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .await
        .map_err(|e| TcpError::Connect(io_token(&e)))?;
    let exchange = async {
        stream.write_all(envelope_json.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.shutdown().await?; // half-close the write side (Python write_eof)
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        Ok::<String, std::io::Error>(String::from_utf8_lossy(&buf).into_owned())
    };
    exchange.await.map_err(|e| TcpError::Io(io_token(&e)))
}

pub(crate) fn authenticated_envelope_json(
    envelope_json: &str,
    endpoint: &DaemonTcpEndpoint,
) -> String {
    if endpoint.auth_token.is_empty() {
        return envelope_json.to_owned();
    }
    match serde_json::from_str::<Value>(envelope_json) {
        Ok(Value::Object(mut map)) => {
            map.insert(
                DAEMON_AUTH_FIELD.to_owned(),
                Value::String(endpoint.auth_token.clone()),
            );
            serde_json::to_string(&Value::Object(map)).unwrap_or_else(|_| envelope_json.to_owned())
        }
        _ => envelope_json.to_owned(),
    }
}

// --- container-side command builders (GC-04: eosd default) --------------------

pub(crate) fn posix_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_owned();
    }
    if s.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'@' | b'%' | b'_' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
            )
    }) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn shell_join(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|p| posix_quote(p))
        .collect::<Vec<_>>()
        .join(" ")
}

fn daemon_thin_client_command(envelope_json: &str) -> String {
    shell_join(&[
        EOSD_REMOTE_PATH,
        "daemon",
        "--client",
        DAEMON_SOCKET_PATH,
        envelope_json,
    ])
}

fn daemon_spawn_command(tcp_endpoint: Option<&DaemonTcpEndpoint>) -> String {
    let mut parts: Vec<String> = vec![
        EOSD_REMOTE_PATH.to_owned(),
        "daemon".to_owned(),
        "--spawn".to_owned(),
        "--socket".to_owned(),
        DAEMON_SOCKET_PATH.to_owned(),
        "--pid-file".to_owned(),
        DAEMON_PID_PATH.to_owned(),
        "--log-file".to_owned(),
        DAEMON_LOG_PATH.to_owned(),
    ];
    if let Some(endpoint) = tcp_endpoint {
        let port = endpoint.internal_port.unwrap_or(endpoint.port);
        parts.push("--tcp-host".to_owned());
        parts.push("0.0.0.0".to_owned());
        parts.push("--tcp-port".to_owned());
        parts.push(port.to_string());
        if !endpoint.auth_token.is_empty() {
            parts.push("--auth-token".to_owned());
            parts.push(endpoint.auth_token.clone());
        }
    }
    let spawn_command = parts
        .iter()
        .map(|p| posix_quote(p))
        .collect::<Vec<_>>()
        .join(" ");
    let inner = rust_daemon_spawn_shell(&spawn_command, &daemon_env_signature(tcp_endpoint));
    // Source /etc/environment so feature-flag env vars propagate to the daemon.
    format!("if [ -r /etc/environment ]; then set -a; . /etc/environment; set +a; fi; {inner}")
}

/// The restart-on-signature-change shell, faithful to Python `_rust_daemon_spawn_shell`.
fn rust_daemon_spawn_shell(spawn_command: &str, signature: &str) -> String {
    let marker = posix_quote(EOSD_SHA_MARKER);
    let socket = posix_quote(DAEMON_SOCKET_PATH);
    let pid = posix_quote(DAEMON_PID_PATH);
    let env = posix_quote(DAEMON_ENV_SIGNATURE_PATH);
    [
        format!("daemon_env_sig={};", posix_quote(signature)),
        format!(
            "if [ -f {marker} ]; then daemon_env_sig=\"$daemon_env_sig;eosd_sha=$(cat {marker})\"; fi;"
        ),
        format!(
            "if [ -S {socket} ] && [ -f {pid} ]; then \
             if [ ! -f {env} ] || [ \"$(cat {env})\" != \"$daemon_env_sig\" ]; then \
             daemon_pid=$(cat {pid} 2>/dev/null || true); \
             if [ -n \"$daemon_pid\" ]; then \
             kill \"$daemon_pid\" 2>/dev/null || true; \
             for _ in $(seq 1 50); do \
             kill -0 \"$daemon_pid\" 2>/dev/null || break; \
             sleep 0.02; \
             done; \
             fi; \
             rm -f {socket} {pid}; \
             fi; \
             fi;"
        ),
        format!("{spawn_command} && printf %s \"$daemon_env_sig\" > {env}"),
    ]
    .join(" ")
}

/// Daemon env signature (Python `_daemon_env_signature`). GC-04: `sandbox_runtime`
/// collapses to `rust`; the dropped module-bundle `bundle_hash()` is replaced by
/// the pinned `EOSD_VERSION` as the `runtime_bundle_sha` identity (the binary's
/// own digest is appended container-side from the `.eosd-sha256` marker).
fn daemon_env_signature(tcp_endpoint: Option<&DaemonTcpEndpoint>) -> String {
    let mut parts = vec![
        "sandbox_runtime=rust".to_owned(),
        format!("runtime_bundle_sha={EOSD_VERSION}"),
    ];
    if let Some(endpoint) = tcp_endpoint {
        let port = endpoint.internal_port.unwrap_or(endpoint.port);
        parts.push(format!("daemon_tcp_port={port}"));
    }
    parts.join(";")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::provider::RawExecResult;
    use crate::testutil::MockAdapter;

    fn sid() -> SandboxId {
        "sb-1".parse().unwrap()
    }

    fn ok_result(stdout: &str) -> RawExecResult {
        RawExecResult {
            exit_code: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
            success: true,
        }
    }

    fn connect_failed() -> RawExecResult {
        io_failed(
            THIN_CLIENT_CONNECT_FAILED,
            "EOS_DAEMON_CONNECT_FAILED:x".to_owned(),
        )
    }

    fn empty_response() -> RawExecResult {
        io_failed(THIN_CLIENT_IO_FAILED, EMPTY_RESPONSE_MESSAGE.to_owned())
    }

    /// Extract the daemon envelope JSON embedded in a thin-client shell command.
    fn envelope_from_command(cmd: &str) -> Value {
        let start = cmd.find('{').expect("envelope start");
        let end = cmd.rfind('}').expect("envelope end");
        serde_json::from_str(&cmd[start..=end]).expect("envelope parses")
    }

    fn client_with(adapter: MockAdapter) -> (DaemonClient, Arc<std::sync::Mutex<Vec<String>>>) {
        let calls = adapter.call_log();
        let registry = ProviderRegistry::new();
        registry.set_default(Arc::new(adapter));
        (DaemonClient::new(Arc::new(registry)), calls)
    }

    // AC-03: envelope {op, invocation_id, args.layer_stack_root}; cancel mints a
    // fresh id; the auth field is added only on the token TCP path.
    #[tokio::test]
    async fn envelope_shape_and_auth() {
        let (client, calls) =
            client_with(MockAdapter::new().with_exec(|_cmd| ok_result("{\"ok\":true}")));
        client
            .call_daemon_api(
                &sid(),
                "api.v1.read_file",
                JsonObject::new(),
                60,
                "/eos/layer-stack",
            )
            .await
            .unwrap();
        let cmd = calls.lock().unwrap()[0].clone();
        let env = envelope_from_command(&cmd);
        assert_eq!(env["op"], serde_json::json!("api.v1.read_file"));
        assert_eq!(
            env["args"]["layer_stack_root"],
            serde_json::json!("/eos/layer-stack")
        );
        let inv = env["invocation_id"].as_str().unwrap();
        assert_eq!(inv.len(), 32, "uuid4().hex is 32 hex chars (no dashes)");
        assert!(inv.bytes().all(|b| b.is_ascii_hexdigit()));

        // cancel mints a fresh top-level invocation id.
        let (client, calls) =
            client_with(MockAdapter::new().with_exec(|_cmd| ok_result("{\"ok\":true}")));
        client
            .call_daemon_api(
                &sid(),
                "api.v1.cancel",
                JsonObject::new(),
                15,
                "/eos/layer-stack",
            )
            .await
            .unwrap();
        let env = envelope_from_command(&calls.lock().unwrap()[0]);
        assert_eq!(env["op"], serde_json::json!("api.v1.cancel"));
        assert_eq!(env["invocation_id"].as_str().unwrap().len(), 32);

        // auth field added only with a token.
        let endpoint = DaemonTcpEndpoint {
            host: "127.0.0.1".to_owned(),
            port: 49153,
            internal_port: Some(37657),
            auth_token: "tok".to_owned(),
        };
        let authed = authenticated_envelope_json("{\"op\":\"x\"}", &endpoint);
        let parsed: Value = serde_json::from_str(&authed).unwrap();
        assert_eq!(parsed[DAEMON_AUTH_FIELD], serde_json::json!("tok"));
        let no_token = DaemonTcpEndpoint {
            auth_token: String::new(),
            ..endpoint
        };
        assert_eq!(
            authenticated_envelope_json("{\"op\":\"x\"}", &no_token),
            "{\"op\":\"x\"}"
        );
    }

    // AC-04: CONNECT_FAILED triggers spawn → readiness → replay; a mutating op
    // returning empty-response fails closed (no spawn).
    #[tokio::test]
    async fn recovery_retry_and_fail_closed() {
        // Case 1: connect-failed then recovery.
        let original_calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&original_calls);
        let (client, calls) = client_with(MockAdapter::new().with_exec(move |cmd| {
            if cmd.contains("--spawn") {
                return ok_result("");
            }
            if cmd.contains("api.runtime.ready") {
                return ok_result("{\"ready\":true}");
            }
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                connect_failed()
            } else {
                ok_result("{\"replayed\":true}")
            }
        }));
        let response = client
            .call_daemon_api(
                &sid(),
                "api.v1.read_file",
                JsonObject::new(),
                60,
                "/eos/layer-stack",
            )
            .await
            .unwrap();
        assert_eq!(response["replayed"], serde_json::json!(true));
        let log = calls.lock().unwrap().clone();
        assert!(log.iter().any(|c| c.contains("--spawn")), "spawn must run");
        assert!(
            log.iter().any(|c| c.contains("api.runtime.ready")),
            "readiness probe must run"
        );

        // Case 2: empty-response on a mutating op fails closed (no spawn).
        let (client, calls) = client_with(MockAdapter::new().with_exec(|cmd| {
            if cmd.contains("--spawn") {
                return ok_result(""); // would mean recovery — must NOT happen
            }
            empty_response()
        }));
        let err = client
            .call_daemon_api(
                &sid(),
                "api.v1.write_file",
                JsonObject::new(),
                60,
                "/eos/layer-stack",
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            SandboxHostError::ExecFailed { exit_code: 98, .. }
        ));
        assert!(
            !calls.lock().unwrap().iter().any(|c| c.contains("--spawn")),
            "fail-closed: mutating op must not spawn/replay"
        );
    }

    // AC-05: a non-policy daemon error decodes to DaemonDispatch; a handler-level
    // policy result (success=false + non-empty status) is returned, not raised.
    #[test]
    fn decode_error_vs_policy_result() {
        let dispatch = decode_and_classify(&ok_result(
            "{\"error\":{\"kind\":\"WorkspaceBindingError\",\"message\":\"boom\"}}",
        ))
        .unwrap_err();
        assert!(matches!(
            dispatch,
            SandboxHostError::DaemonDispatch { kind, message, .. }
                if kind == "WorkspaceBindingError" && message == "boom"
        ));

        let policy = decode_and_classify(&ok_result(
            "{\"success\":false,\"status\":\"rejected\",\"error\":{\"reason\":\"conflict\"}}",
        ))
        .unwrap();
        assert_eq!(policy["status"], serde_json::json!("rejected"));
        assert_eq!(policy["error"]["reason"], serde_json::json!("conflict"));

        // a non-object error string still raises DaemonDispatch.
        let stringy = decode_and_classify(&ok_result("{\"error\":\"down\"}")).unwrap_err();
        assert!(matches!(
            stringy,
            SandboxHostError::DaemonDispatch { message, .. } if message == "down"
        ));
    }

    // AC-07b: concurrent resolves single-flight (one adapter resolve), no guard
    // held across the await, and cache invalidation triggers a fresh resolve.
    #[tokio::test]
    async fn tcp_endpoint_singleflight_lock_order() {
        let endpoint = DaemonTcpEndpoint {
            host: "127.0.0.1".to_owned(),
            port: 49153,
            internal_port: Some(37657),
            auth_token: String::new(),
        };
        let adapter = MockAdapter::new().with_tcp(endpoint).with_tcp_delay_ms(25);
        let resolves = adapter.tcp_resolve_counter();
        let registry = ProviderRegistry::new();
        let adapter_arc: Arc<dyn ProviderAdapter> = Arc::new(adapter);
        registry.set_default(Arc::clone(&adapter_arc));
        let client = DaemonClient::new(Arc::new(registry));

        let id = sid();
        // Two concurrent callers share ONE async resolve (single-flight).
        let (a, b) = tokio::join!(
            client.resolve_daemon_tcp_endpoint(&*adapter_arc, &id),
            client.resolve_daemon_tcp_endpoint(&*adapter_arc, &id),
        );
        assert!(a.is_some() && b.is_some());
        assert_eq!(a.unwrap().port, 49153);
        assert_eq!(
            resolves.load(Ordering::SeqCst),
            1,
            "single-flight: one resolve"
        );

        // A third call hits the cache (no new resolve).
        let _ = client.resolve_daemon_tcp_endpoint(&*adapter_arc, &id).await;
        assert_eq!(
            resolves.load(Ordering::SeqCst),
            1,
            "cache hit: no new resolve"
        );

        // Invalidation forces a fresh single-flight resolve.
        client.invalidate_daemon_tcp_endpoint(&id);
        let _ = client.resolve_daemon_tcp_endpoint(&*adapter_arc, &id).await;
        assert_eq!(
            resolves.load(Ordering::SeqCst),
            2,
            "re-resolve after invalidation"
        );
    }

    #[test]
    fn empty_response_gating_matches_python_set() {
        for op in [
            "api.edit_file",
            "api.v1.edit_file",
            "api.write_file",
            "api.v1.write_file",
            "api.v1.exec_command",
            "api.v1.exec_stdin",
            "plugin.install",
        ] {
            assert!(!can_retry_empty_response(op), "{op} must fail closed");
        }
        for op in [
            "api.v1.read_file",
            "api.runtime.ready",
            "api.ensure_workspace_base",
            "api.v1.cancel",
            "api.shell", // non-v1 shell is NOT in the literal fail-closed set
        ] {
            assert!(can_retry_empty_response(op), "{op} must be retryable");
        }
    }
}
