use std::collections::BTreeMap;
use std::net::IpAddr;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sandbox_operation_catalog::internal::runtime::{
    CREATE_WORKSPACE_SESSION, DESTROY_WORKSPACE_SESSION,
};
use sandbox_operation_contract::{OperationRequest, OperationScope};
use serde::Deserialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Command;

use crate::gateway::{Correlation, GatewayError, OwnedSandboxId, ProductGateway};
use crate::model::{AllowedNetworkProfile, WorkspaceAction};

const DAEMON_AUTH_FIELD: &str = "_sandbox_daemon_auth_token";
const DOCKER_AUTH_LABEL: &str = "eos.auth_token";
const FINALIZE_POLICY_NO_OP: &str = "no_op";
const DOCKER_INSPECT_TIMEOUT: Duration = Duration::from_secs(30);
const DAEMON_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_DOCKER_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_SESSION_ID_BYTES: usize = 256;
const MAX_AUTH_TOKEN_BYTES: usize = 4 * 1024;

/// A workspace-session identity accepted from the product only after strict
/// schema validation. The adapter separately tracks its owning sandbox.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceSessionId(String);

impl WorkspaceSessionId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parse(value: String) -> Result<Self, WorkspaceSessionError> {
        if value.is_empty()
            || value.len() > MAX_SESSION_ID_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(WorkspaceSessionError::ResponseSchema);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedSession {
    sandbox_id: OwnedSandboxId,
    workspace_session_id: WorkspaceSessionId,
    network_profile: AllowedNetworkProfile,
}

impl CreatedSession {
    #[must_use]
    pub fn sandbox_id(&self) -> &OwnedSandboxId {
        &self.sandbox_id
    }

    #[must_use]
    pub fn workspace_session_id(&self) -> &WorkspaceSessionId {
        &self.workspace_session_id
    }

    #[must_use]
    pub const fn network_profile(&self) -> AllowedNetworkProfile {
        self.network_profile
    }

    #[must_use]
    pub fn into_parts(self) -> (OwnedSandboxId, WorkspaceSessionId) {
        (self.sandbox_id, self.workspace_session_id)
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceSessionError {
    #[error("workspace-session gateway access failed")]
    Gateway,
    #[error("workspace-session ownership check failed")]
    Ownership,
    #[error("workspace-session state lock is unavailable")]
    StateUnavailable,
    #[error("workspace-session daemon endpoint is unavailable")]
    EndpointUnavailable,
    #[error("workspace-session credential lookup failed")]
    CredentialLookup,
    #[error("workspace-session transport failed")]
    Transport,
    #[error("workspace-session request timed out")]
    Timeout,
    #[error("workspace-session response did not match the product schema")]
    ResponseSchema,
    #[error("workspace-session response contained benchmark credentials")]
    CredentialEcho,
    #[error("workspace-session product operation failed ({kind})")]
    Product { kind: String },
}

/// Closed test-only lifecycle surface. It is intentionally not a raw daemon
/// client and cannot carry an operation name or arbitrary payload.
#[allow(async_fn_in_trait)]
pub trait WorkspaceSessionLifecycle {
    async fn create_no_op(
        &self,
        sandbox_id: OwnedSandboxId,
        network: AllowedNetworkProfile,
        correlation: Correlation,
    ) -> Result<CreatedSession, WorkspaceSessionError>;

    async fn destroy(
        &self,
        sandbox_id: OwnedSandboxId,
        session_id: WorkspaceSessionId,
        correlation: Correlation,
    ) -> Result<(), WorkspaceSessionError>;
}

pub struct WorkspaceSessionAdapter {
    gateway: Arc<ProductGateway>,
    owned_sessions: Mutex<BTreeMap<String, OwnedSandboxId>>,
}

impl std::fmt::Debug for WorkspaceSessionAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceSessionAdapter")
            .finish_non_exhaustive()
    }
}

impl WorkspaceSessionAdapter {
    #[must_use]
    pub fn new(gateway: Arc<ProductGateway>) -> Self {
        Self {
            gateway,
            owned_sessions: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn owned_session_count(&self) -> Result<usize, WorkspaceSessionError> {
        self.owned_sessions
            .lock()
            .map(|sessions| sessions.len())
            .map_err(|_| WorkspaceSessionError::StateUnavailable)
    }

    /// Retire a session only after a typed product response has established
    /// that the product no longer owns it (for example `faulty` after the
    /// squash cleanup path, or `session_gone`). This updates only the closed
    /// ownership ledger and performs no transport access.
    pub(crate) fn retire_product_destroyed(
        &self,
        sandbox_id: &OwnedSandboxId,
        session_id: &WorkspaceSessionId,
    ) -> Result<(), WorkspaceSessionError> {
        let mut sessions = self
            .owned_sessions
            .lock()
            .map_err(|_| WorkspaceSessionError::StateUnavailable)?;
        match sessions.get(session_id.as_str()) {
            Some(owned_sandbox) if owned_sandbox == sandbox_id => {
                sessions.remove(session_id.as_str());
                Ok(())
            }
            Some(_) | None => Err(WorkspaceSessionError::Ownership),
        }
    }

    async fn invoke(
        &self,
        sandbox_id: &OwnedSandboxId,
        request: WorkspaceDaemonRequest,
        correlation: &Correlation,
    ) -> Result<Value, WorkspaceSessionError> {
        // This exhaustive action match is deliberately first. No endpoint,
        // credential, process, or network access happens before the request is
        // proven to be one of the two allowlisted lifecycle actions.
        let operation = allowlisted_operation(request.action());
        let args = request.args();

        let sandbox = self
            .gateway
            .inspect_sandbox(sandbox_id, correlation)
            .await
            .map_err(map_gateway_error)?;
        let endpoint = sandbox
            .daemon
            .ok_or(WorkspaceSessionError::EndpointUnavailable)?;
        validate_loopback_host(&endpoint.host)?;

        let auth_token = lookup_daemon_auth(sandbox_id).await?;
        let operation_request = OperationRequest::new(
            operation,
            correlation.wire_request_id(),
            OperationScope::sandbox(sandbox_id.as_str()),
            args,
        );
        let request_line = authenticated_request_line(&operation_request, &auth_token)?;
        let response = tokio::time::timeout(
            DAEMON_REQUEST_TIMEOUT,
            daemon_exchange(&endpoint.host, endpoint.port, &request_line),
        )
        .await
        .map_err(|_| WorkspaceSessionError::Timeout)??;

        reject_credential_echo(&response, &auth_token)?;
        reject_product_error(&response)?;
        Ok(response)
    }
}

impl WorkspaceSessionLifecycle for WorkspaceSessionAdapter {
    async fn create_no_op(
        &self,
        sandbox_id: OwnedSandboxId,
        network: AllowedNetworkProfile,
        correlation: Correlation,
    ) -> Result<CreatedSession, WorkspaceSessionError> {
        let value = self
            .invoke(
                &sandbox_id,
                WorkspaceDaemonRequest::CreateNoOp { network },
                &correlation,
            )
            .await?;
        let wire: CreateSessionWire =
            serde_json::from_value(value).map_err(|_| WorkspaceSessionError::ResponseSchema)?;
        if wire.network_profile != network || wire.finalize_policy != FINALIZE_POLICY_NO_OP {
            return Err(WorkspaceSessionError::ResponseSchema);
        }
        let workspace_session_id = WorkspaceSessionId::parse(wire.workspace_session_id)?;
        let mut sessions = self
            .owned_sessions
            .lock()
            .map_err(|_| WorkspaceSessionError::StateUnavailable)?;
        if sessions.contains_key(workspace_session_id.as_str()) {
            return Err(WorkspaceSessionError::Ownership);
        }
        sessions.insert(workspace_session_id.0.clone(), sandbox_id.clone());
        Ok(CreatedSession {
            sandbox_id,
            workspace_session_id,
            network_profile: network,
        })
    }

    async fn destroy(
        &self,
        sandbox_id: OwnedSandboxId,
        session_id: WorkspaceSessionId,
        correlation: Correlation,
    ) -> Result<(), WorkspaceSessionError> {
        // Remove and validate ownership before invoke can inspect the sandbox,
        // look up its credential, or open a socket.
        let owned_sandbox = {
            let mut sessions = self
                .owned_sessions
                .lock()
                .map_err(|_| WorkspaceSessionError::StateUnavailable)?;
            sessions
                .remove(session_id.as_str())
                .ok_or(WorkspaceSessionError::Ownership)?
        };
        if owned_sandbox != sandbox_id {
            self.restore_session(&session_id, owned_sandbox)?;
            return Err(WorkspaceSessionError::Ownership);
        }

        let result = self
            .invoke(
                &sandbox_id,
                WorkspaceDaemonRequest::Destroy {
                    workspace_session_id: session_id.clone(),
                },
                &correlation,
            )
            .await;
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                self.restore_session(&session_id, sandbox_id)?;
                return Err(error);
            }
        };
        let wire: DestroySessionWire = match serde_json::from_value(value) {
            Ok(wire) => wire,
            Err(_) => {
                self.restore_session(&session_id, sandbox_id)?;
                return Err(WorkspaceSessionError::ResponseSchema);
            }
        };
        if !wire.destroyed || wire.workspace_session_id != session_id.0 {
            self.restore_session(&session_id, sandbox_id)?;
            return Err(WorkspaceSessionError::ResponseSchema);
        }
        let _ = wire.evicted_upperdir_bytes;
        Ok(())
    }
}

impl WorkspaceSessionAdapter {
    fn restore_session(
        &self,
        session_id: &WorkspaceSessionId,
        sandbox_id: OwnedSandboxId,
    ) -> Result<(), WorkspaceSessionError> {
        let mut sessions = self
            .owned_sessions
            .lock()
            .map_err(|_| WorkspaceSessionError::StateUnavailable)?;
        if sessions.contains_key(session_id.as_str()) {
            return Err(WorkspaceSessionError::Ownership);
        }
        sessions.insert(session_id.0.clone(), sandbox_id);
        Ok(())
    }
}

#[derive(Debug)]
enum WorkspaceDaemonRequest {
    CreateNoOp {
        network: AllowedNetworkProfile,
    },
    Destroy {
        workspace_session_id: WorkspaceSessionId,
    },
}

impl WorkspaceDaemonRequest {
    const fn action(&self) -> WorkspaceAction {
        match self {
            Self::CreateNoOp { .. } => WorkspaceAction::CreateNoOpSession,
            Self::Destroy { .. } => WorkspaceAction::DestroySession,
        }
    }

    fn args(&self) -> Value {
        match self {
            Self::CreateNoOp { network } => json!({ "network_profile": network }),
            Self::Destroy {
                workspace_session_id,
            } => json!({ "workspace_session_id": workspace_session_id.as_str() }),
        }
    }
}

const fn allowlisted_operation(action: WorkspaceAction) -> &'static str {
    match action {
        WorkspaceAction::CreateNoOpSession => CREATE_WORKSPACE_SESSION,
        WorkspaceAction::DestroySession => DESTROY_WORKSPACE_SESSION,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSessionWire {
    workspace_session_id: String,
    network_profile: AllowedNetworkProfile,
    finalize_policy: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DestroySessionWire {
    workspace_session_id: String,
    destroyed: bool,
    evicted_upperdir_bytes: u64,
}

fn validate_loopback_host(host: &str) -> Result<(), WorkspaceSessionError> {
    host.parse::<IpAddr>()
        .ok()
        .filter(IpAddr::is_loopback)
        .map(|_| ())
        .ok_or(WorkspaceSessionError::EndpointUnavailable)
}

async fn lookup_daemon_auth(sandbox_id: &OwnedSandboxId) -> Result<String, WorkspaceSessionError> {
    let format = format!("{{{{ index .Config.Labels {DOCKER_AUTH_LABEL:?} }}}}");
    let mut command = Command::new("docker");
    command
        .arg("inspect")
        .arg("--format")
        .arg(format)
        .arg(sandbox_id.as_str())
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    copy_docker_environment(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| WorkspaceSessionError::CredentialLookup)?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill().await;
            return Err(WorkspaceSessionError::CredentialLookup);
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = child.kill().await;
            return Err(WorkspaceSessionError::CredentialLookup);
        }
    };
    let mut stdout_task = tokio::spawn(read_bounded(stdout, MAX_DOCKER_OUTPUT_BYTES));
    let mut stderr_task = tokio::spawn(read_bounded(stderr, MAX_DOCKER_OUTPUT_BYTES));

    let status = match tokio::time::timeout(DOCKER_INSPECT_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            abort_output_task(&mut stdout_task).await;
            abort_output_task(&mut stderr_task).await;
            return Err(WorkspaceSessionError::CredentialLookup);
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            abort_output_task(&mut stdout_task).await;
            abort_output_task(&mut stderr_task).await;
            return Err(WorkspaceSessionError::Timeout);
        }
    };
    let stdout_result = join_output_task(&mut stdout_task).await;
    let stderr_result = join_output_task(&mut stderr_task).await;
    let (stdout, stdout_truncated) = stdout_result?;
    let (_, stderr_truncated) = stderr_result?;
    if !status.success() || stdout_truncated || stderr_truncated {
        return Err(WorkspaceSessionError::CredentialLookup);
    }
    let token = String::from_utf8(stdout)
        .map_err(|_| WorkspaceSessionError::CredentialLookup)?
        .trim()
        .to_owned();
    if token.is_empty()
        || token == "<no value>"
        || token.len() > MAX_AUTH_TOKEN_BYTES
        || token.chars().any(char::is_control)
    {
        return Err(WorkspaceSessionError::CredentialLookup);
    }
    Ok(token)
}

fn copy_docker_environment(command: &mut Command) {
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

async fn read_bounded<R>(
    mut reader: R,
    limit: usize,
) -> Result<(Vec<u8>, bool), WorkspaceSessionError>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::new();
    let mut truncated = false;
    let mut chunk = [0_u8; 4 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .await
            .map_err(|_| WorkspaceSessionError::CredentialLookup)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&chunk[..keep]);
        truncated |= keep < read;
    }
    Ok((retained, truncated))
}

async fn join_output_task(
    task: &mut tokio::task::JoinHandle<Result<(Vec<u8>, bool), WorkspaceSessionError>>,
) -> Result<(Vec<u8>, bool), WorkspaceSessionError> {
    match tokio::time::timeout(OUTPUT_DRAIN_TIMEOUT, &mut *task).await {
        Ok(result) => result.map_err(|_| WorkspaceSessionError::CredentialLookup)?,
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(WorkspaceSessionError::CredentialLookup)
        }
    }
}

async fn abort_output_task(
    task: &mut tokio::task::JoinHandle<Result<(Vec<u8>, bool), WorkspaceSessionError>>,
) {
    task.abort();
    let _ = task.await;
}

fn authenticated_request_line(
    request: &OperationRequest,
    auth_token: &str,
) -> Result<Vec<u8>, WorkspaceSessionError> {
    let mut value = serde_json::to_value(request).map_err(|_| WorkspaceSessionError::Transport)?;
    let object = value
        .as_object_mut()
        .ok_or(WorkspaceSessionError::Transport)?;
    object.insert(
        DAEMON_AUTH_FIELD.to_owned(),
        Value::String(auth_token.to_owned()),
    );
    let mut line = serde_json::to_vec(&value).map_err(|_| WorkspaceSessionError::Transport)?;
    line.push(b'\n');
    if line.len() > sandbox_operation_client::MAX_REQUEST_BYTES {
        return Err(WorkspaceSessionError::Transport);
    }
    Ok(line)
}

async fn daemon_exchange(
    host: &str,
    port: u16,
    request_line: &[u8],
) -> Result<Value, WorkspaceSessionError> {
    if port == 0 {
        return Err(WorkspaceSessionError::EndpointUnavailable);
    }
    let mut stream = TcpStream::connect((host, port))
        .await
        .map_err(|_| WorkspaceSessionError::Transport)?;
    stream
        .write_all(request_line)
        .await
        .map_err(|_| WorkspaceSessionError::Transport)?;
    stream
        .shutdown()
        .await
        .map_err(|_| WorkspaceSessionError::Transport)?;

    let limit = u64::try_from(sandbox_operation_client::MAX_REQUEST_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut response = Vec::new();
    stream
        .take(limit)
        .read_to_end(&mut response)
        .await
        .map_err(|_| WorkspaceSessionError::Transport)?;
    if response.is_empty()
        || response.len() > sandbox_operation_client::MAX_REQUEST_BYTES
        || !response.ends_with(b"\n")
        || response[..response.len() - 1]
            .iter()
            .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        return Err(WorkspaceSessionError::ResponseSchema);
    }
    serde_json::from_slice(&response).map_err(|_| WorkspaceSessionError::ResponseSchema)
}

fn reject_credential_echo(value: &Value, auth_token: &str) -> Result<(), WorkspaceSessionError> {
    let encoded =
        serde_json::to_string(value).map_err(|_| WorkspaceSessionError::ResponseSchema)?;
    if encoded.contains(auth_token) {
        Err(WorkspaceSessionError::CredentialEcho)
    } else {
        Ok(())
    }
}

fn reject_product_error(value: &Value) -> Result<(), WorkspaceSessionError> {
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
    Err(WorkspaceSessionError::Product { kind })
}

fn map_gateway_error(_error: GatewayError) -> WorkspaceSessionError {
    WorkspaceSessionError::Gateway
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_actions_map_only_to_fixed_internal_operations() {
        assert_eq!(
            allowlisted_operation(WorkspaceAction::CreateNoOpSession),
            CREATE_WORKSPACE_SESSION
        );
        assert_eq!(
            allowlisted_operation(WorkspaceAction::DestroySession),
            DESTROY_WORKSPACE_SESSION
        );
    }

    #[test]
    fn session_ids_and_daemon_hosts_are_bounded() {
        assert!(WorkspaceSessionId::parse("session-1".to_owned()).is_ok());
        assert!(WorkspaceSessionId::parse("../escape".to_owned()).is_err());
        assert!(validate_loopback_host("127.0.0.1").is_ok());
        assert!(validate_loopback_host("::1").is_ok());
        assert!(validate_loopback_host("localhost").is_err());
        assert!(validate_loopback_host("192.0.2.1").is_err());
    }

    #[test]
    fn daemon_request_has_exact_auth_field_and_single_newline_frame() {
        let request = OperationRequest::new(
            CREATE_WORKSPACE_SESSION,
            "request-1",
            OperationScope::sandbox("sandbox-1"),
            json!({ "network_profile": "isolated" }),
        );
        let line = authenticated_request_line(&request, "top-secret")
            .expect("fixed request must serialize");

        assert!(line.ends_with(b"\n"));
        assert_eq!(line.iter().filter(|byte| **byte == b'\n').count(), 1);
        let value: Value = serde_json::from_slice(&line).expect("request line must be JSON");
        assert_eq!(value.get(DAEMON_AUTH_FIELD), Some(&json!("top-secret")));
    }

    #[test]
    fn daemon_responses_are_strict_and_cannot_echo_credentials() {
        let response = json!({
            "workspace_session_id": "session-1",
            "network_profile": "isolated",
            "finalize_policy": "no_op",
            "unexpected": true,
        });
        assert!(serde_json::from_value::<CreateSessionWire>(response).is_err());
        assert!(matches!(
            reject_credential_echo(&json!({ "nested": "top-secret" }), "top-secret"),
            Err(WorkspaceSessionError::CredentialEcho)
        ));
    }
}
