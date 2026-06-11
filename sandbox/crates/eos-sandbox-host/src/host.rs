//! Host engine facade, registry, endpoint cache, forwarding, and recovery.

use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::protocol::{
    is_success, stamped_envelope_bytes, ClientError, ProtocolClient, CONNECT_RETRY_DELAYS_S,
    DEFAULT_LAYER_STACK_ROOT, HEARTBEAT_OP, READY_OP,
};
use crate::runtime::{
    container_labels, docker, resolve_published_addr, running_container_ids, ContainerLifetime,
    ContainerSpec, DaemonContainer, DaemonSpec,
};

/// Engine configuration (one fleet, one image).
#[derive(Debug, Clone)]
pub struct HostConfig {
    pub image: String,
    pub platform: Option<String>,
    pub eosd_path: PathBuf,
    pub config_yaml_path: PathBuf,
    pub remote_daemon_dir: PathBuf,
    pub remote_eosd_path: PathBuf,
    pub remote_config_path: PathBuf,
    pub tcp_port: u16,
    pub ready_timeout: Duration,
    pub request_timeout: Duration,
    pub created_by: String,
    pub state_dir: PathBuf,
}

impl HostConfig {
    fn daemon_spec(&self, tcp_port: u16) -> DaemonSpec {
        DaemonSpec {
            eosd_path: self.eosd_path.clone(),
            remote_daemon_dir: self.remote_daemon_dir.clone(),
            remote_eosd_path: self.remote_eosd_path.clone(),
            remote_config_path: self.remote_config_path.clone(),
            config_yaml: String::new(),
            extra_dirs: Vec::new(),
            tcp_port,
            ready_timeout: self.ready_timeout,
            request_timeout: self.request_timeout,
        }
    }
}

/// Host view of one sandbox (the `sandbox.status` payload source).
#[derive(Debug)]
pub struct SandboxStatus {
    pub sandbox_id: String,
    pub container: String,
    pub endpoint: Option<SocketAddr>,
    pub created_by: String,
    pub daemon: Value,
}

/// The host engine: owns and reaches sandboxes.
pub struct SandboxHost {
    config: HostConfig,
    config_yaml: String,
    registry: SandboxRegistry,
}

impl SandboxHost {
    pub fn open(config: HostConfig) -> Result<Self> {
        let config_yaml = fs::read_to_string(&config.config_yaml_path).with_context(|| {
            format!(
                "read daemon config document {}",
                config.config_yaml_path.display()
            )
        })?;
        let registry = SandboxRegistry::open(config.state_dir.clone())?;
        registry.rebuild_from_docker();
        Ok(Self {
            config,
            config_yaml,
            registry,
        })
    }

    pub fn acquire(&self) -> Result<String> {
        let sandbox_id = format!("sb-{}", random_hex(16)?);
        let token = random_hex(32)?;
        let container = ContainerSpec {
            name: sandbox_id.clone(),
            image: self.config.image.clone(),
            platform: self.config.platform.clone(),
            cap_add: Vec::new(),
            security_opt: Vec::new(),
            tmpfs: Vec::new(),
            labels: vec![
                (SANDBOX_ID_LABEL.to_owned(), sandbox_id.clone()),
                (TCP_PORT_LABEL.to_owned(), self.config.tcp_port.to_string()),
                (CREATED_BY_LABEL.to_owned(), self.config.created_by.clone()),
            ],
            lifetime: ContainerLifetime::Keep,
        };
        let mut daemon = self.config.daemon_spec(self.config.tcp_port);
        daemon.config_yaml = self.config_yaml.clone();
        let started = match DaemonContainer::start(&container, &daemon, token.clone()) {
            Ok(started) => started,
            Err(err) => {
                // Keep-lifetime containers survive drop; reap the failed one.
                let _ = docker(&["rm".to_owned(), "-f".to_owned(), sandbox_id.clone()]);
                return Err(err);
            }
        };
        let record = SandboxRecord::new(
            sandbox_id.clone(),
            sandbox_id.clone(),
            token,
            self.config.tcp_port,
            self.config.created_by.clone(),
            Some(started.client().addr()),
        );
        self.registry.insert(record)?;
        Ok(sandbox_id)
    }

    pub fn release(&self, sandbox_id: &str) -> bool {
        let Some(record) = self.registry.remove(sandbox_id) else {
            return false;
        };
        let _ = docker(&["rm".to_owned(), "-f".to_owned(), record.container.clone()]);
        true
    }

    #[must_use]
    pub fn status(&self, sandbox_id: &str) -> Option<SandboxStatus> {
        let record = self.registry.get(sandbox_id)?;
        let daemon = self.probe_readiness(&record);
        Some(SandboxStatus {
            sandbox_id: record.sandbox_id.clone(),
            container: record.container.clone(),
            endpoint: record.cached_endpoint(),
            created_by: record.created_by.clone(),
            daemon,
        })
    }

    #[must_use]
    pub fn list(&self) -> Vec<SandboxStatus> {
        self.registry
            .list()
            .into_iter()
            .map(|record| SandboxStatus {
                sandbox_id: record.sandbox_id.clone(),
                container: record.container.clone(),
                endpoint: record.cached_endpoint(),
                created_by: record.created_by.clone(),
                daemon: Value::Null,
            })
            .collect()
    }

    pub fn forward(
        &self,
        sandbox_id: &str,
        mutates_state: bool,
        op: &str,
        invocation_id: &str,
        args: &Value,
    ) -> Option<Result<Value, ForwardError>> {
        let record = self.registry.get(sandbox_id)?;
        Some(forward_request(
            &record,
            &self.config,
            mutates_state,
            op,
            invocation_id,
            args,
        ))
    }

    fn probe_readiness(&self, record: &SandboxRecord) -> Value {
        let Some(endpoint) = record.cached_endpoint() else {
            return json!({"ready": false, "error": "endpoint not resolved"});
        };
        let client = ProtocolClient::new(
            endpoint,
            Some(record.token.clone()),
            self.config.request_timeout,
        );
        match client.request_unstamped(
            READY_OP,
            "status-probe",
            &json!({"layer_stack_root": DEFAULT_LAYER_STACK_ROOT}),
        ) {
            Ok(resp) if is_success(&resp) => resp,
            Ok(resp) => json!({"ready": false, "error": resp}),
            Err(err) => json!({"ready": false, "error": err.to_string()}),
        }
    }
}

fn random_hex(bytes: usize) -> Result<String> {
    use std::io::Read;

    let mut buf = vec![0_u8; bytes];
    fs::File::open("/dev/urandom")
        .context("open /dev/urandom")?
        .read_exact(&mut buf)
        .context("read /dev/urandom")?;
    Ok(buf.iter().map(|byte| format!("{byte:02x}")).collect())
}

const SANDBOX_ID_LABEL: &str = "eos.sandbox_id";
const TCP_PORT_LABEL: &str = "eos.tcp_port";
const CREATED_BY_LABEL: &str = "eos.created_by";

#[derive(Debug)]
struct SandboxRecord {
    sandbox_id: String,
    container: String,
    token: String,
    tcp_port: u16,
    created_by: String,
    endpoint: Mutex<Option<SocketAddr>>,
}

impl SandboxRecord {
    fn new(
        sandbox_id: String,
        container: String,
        token: String,
        tcp_port: u16,
        created_by: String,
        endpoint: Option<SocketAddr>,
    ) -> Self {
        Self {
            sandbox_id,
            container,
            token,
            tcp_port,
            created_by,
            endpoint: Mutex::new(endpoint),
        }
    }

    #[must_use]
    fn cached_endpoint(&self) -> Option<SocketAddr> {
        *self.endpoint.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn cache_endpoint(&self, addr: SocketAddr) {
        *self.endpoint.lock().unwrap_or_else(PoisonError::into_inner) = Some(addr);
    }

    fn invalidate_endpoint(&self) {
        *self.endpoint.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

struct SandboxRegistry {
    state_dir: PathBuf,
    records: Mutex<HashMap<String, Arc<SandboxRecord>>>,
}

impl SandboxRegistry {
    fn open(state_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&state_dir)
            .with_context(|| format!("create host state dir {}", state_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o700);
            fs::set_permissions(&state_dir, perms)
                .with_context(|| format!("chmod 700 {}", state_dir.display()))?;
        }
        Ok(Self {
            state_dir,
            records: Mutex::new(HashMap::new()),
        })
    }

    fn rebuild_from_docker(&self) -> usize {
        let ids = running_container_ids(&[SANDBOX_ID_LABEL.to_owned()]);
        let Ok(label_maps) = container_labels(&ids) else {
            return 0;
        };
        let mut adopted = 0;
        for labels in label_maps {
            let label = |key: &str| labels.get(key).and_then(Value::as_str);
            let Some(sandbox_id) = label(SANDBOX_ID_LABEL) else {
                continue;
            };
            let Ok(token) = self.load_token(sandbox_id) else {
                continue;
            };
            let Some(tcp_port) = label(TCP_PORT_LABEL).and_then(|port| port.parse::<u16>().ok())
            else {
                continue;
            };
            let created_by = label(CREATED_BY_LABEL).unwrap_or("unknown").to_owned();
            // Container NAME is the docker handle the engine commands use; the
            // provision flow names containers after their sandbox id.
            let record = SandboxRecord::new(
                sandbox_id.to_owned(),
                sandbox_id.to_owned(),
                token,
                tcp_port,
                created_by,
                None,
            );
            self.lock().insert(sandbox_id.to_owned(), Arc::new(record));
            adopted += 1;
        }
        adopted
    }

    fn insert(&self, record: SandboxRecord) -> Result<Arc<SandboxRecord>> {
        self.persist_token(&record.sandbox_id, &record.token)?;
        let record = Arc::new(record);
        self.lock()
            .insert(record.sandbox_id.clone(), Arc::clone(&record));
        Ok(record)
    }

    #[must_use]
    fn get(&self, sandbox_id: &str) -> Option<Arc<SandboxRecord>> {
        self.lock().get(sandbox_id).cloned()
    }

    fn remove(&self, sandbox_id: &str) -> Option<Arc<SandboxRecord>> {
        let removed = self.lock().remove(sandbox_id);
        if removed.is_some() {
            let _ = fs::remove_file(self.token_path(sandbox_id));
        }
        removed
    }

    #[must_use]
    fn list(&self) -> Vec<Arc<SandboxRecord>> {
        let mut records: Vec<_> = self.lock().values().cloned().collect();
        records.sort_by(|a, b| a.sandbox_id.cmp(&b.sandbox_id));
        records
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<SandboxRecord>>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn token_path(&self, sandbox_id: &str) -> PathBuf {
        self.state_dir.join(format!("{sandbox_id}.token"))
    }

    fn persist_token(&self, sandbox_id: &str, token: &str) -> Result<()> {
        let path = self.token_path(sandbox_id);
        fs::write(&path, token).with_context(|| format!("write token {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 600 {}", path.display()))?;
        }
        Ok(())
    }

    fn load_token(&self, sandbox_id: &str) -> Result<String> {
        let path = self.token_path(sandbox_id);
        let token =
            fs::read_to_string(&path).with_context(|| format!("read token {}", path.display()))?;
        Ok(token.trim().to_owned())
    }
}

fn cached_or_resolve_endpoint(record: &SandboxRecord) -> Result<SocketAddr> {
    if let Some(addr) = record.cached_endpoint() {
        return Ok(addr);
    }
    resolve_endpoint(record)
}

fn resolve_endpoint(record: &SandboxRecord) -> Result<SocketAddr> {
    record.invalidate_endpoint();
    match resolve_published_addr(&record.container, record.tcp_port)? {
        Some(addr) => {
            record.cache_endpoint(addr);
            Ok(addr)
        }
        None => bail!(
            "no published port {} for container {}",
            record.tcp_port,
            record.container
        ),
    }
}

fn forward_request(
    record: &SandboxRecord,
    config: &HostConfig,
    mutates_state: bool,
    op: &str,
    invocation_id: &str,
    args: &Value,
) -> Result<Value, ForwardError> {
    let mut tcp_line = stamped_envelope_bytes(op, invocation_id, args, Some(&record.token));
    tcp_line.push(b'\n');
    let attempt = ForwardAttempt {
        record,
        config,
        mutates_state,
        tcp_line,
        op,
        invocation_id,
        args,
    };
    run_recovery(&attempt)
}

/// Terminal failure of a forwarded request after recovery.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    /// Recovery exhausted: the sandbox cannot be reached or respawned.
    #[error("sandbox unavailable: {0}")]
    SandboxUnavailable(String),
    /// A mutating op was sent but its outcome is unknowable; NOT retried.
    #[error("uncertain outcome: {0}")]
    UncertainOutcome(String),
}

struct ForwardAttempt<'a> {
    record: &'a SandboxRecord,
    config: &'a HostConfig,
    mutates_state: bool,
    tcp_line: Vec<u8>,
    op: &'a str,
    invocation_id: &'a str,
    args: &'a Value,
}

fn run_recovery(attempt: &ForwardAttempt<'_>) -> Result<Value, ForwardError> {
    let unavailable = |context: &str, err: &dyn std::fmt::Display| {
        ForwardError::SandboxUnavailable(format!(
            "{} ({context}): {err}",
            attempt.record.sandbox_id
        ))
    };

    let endpoint = match cached_or_resolve_endpoint(attempt.record) {
        Ok(addr) => addr,
        Err(err) => return fallback_chain(attempt, &unavailable("resolve endpoint", &err)),
    };
    match tcp_with_connect_backoff(attempt, endpoint) {
        Ok(value) => Ok(value),
        Err(err) if err.is_connect_failure() => {
            // Invalidate, re-resolve, retry once.
            match resolve_endpoint(attempt.record) {
                Ok(addr) => match tcp_once(attempt, addr) {
                    Ok(value) => Ok(value),
                    Err(err) => {
                        fallback_chain(attempt, &unavailable("retry after re-resolve", &err))
                    }
                },
                Err(err) => fallback_chain(attempt, &unavailable("re-resolve endpoint", &err)),
            }
        }
        Err(err) => {
            // The request may have been delivered: fail closed for writes.
            // The op is never replayed, but the sandbox is still restored
            // (probe, respawn only when dead) so the NEXT call finds a live
            // daemon instead of an eternally failing one.
            if attempt.mutates_state {
                restore_if_unreachable(attempt);
                return Err(ForwardError::UncertainOutcome(format!(
                    "{}: {err}",
                    attempt.record.sandbox_id
                )));
            }
            fallback_chain(attempt, &unavailable("tcp request", &err))
        }
    }
}

/// Best-effort sandbox restoration after an ambiguous mutating-op failure:
/// one short liveness probe, then an in-place respawn only when the daemon is
/// actually unreachable (a healthy-but-slow daemon is never killed).
fn restore_if_unreachable(attempt: &ForwardAttempt<'_>) {
    let probe = resolve_endpoint(attempt.record).ok().and_then(|endpoint| {
        let client = ProtocolClient::new(endpoint, None, Duration::from_secs(2));
        let mut line = stamped_envelope_bytes(
            HEARTBEAT_OP,
            "recovery-probe",
            &Value::Object(serde_json::Map::new()),
            Some(&attempt.record.token),
        );
        line.push(b'\n');
        client.request_raw(&line).ok()
    });
    if probe.is_some_and(|resp| is_success(&resp)) {
        return;
    }
    let _ = respawn_and_gate(attempt);
}

fn tcp_with_connect_backoff(
    attempt: &ForwardAttempt<'_>,
    endpoint: std::net::SocketAddr,
) -> Result<Value, ClientError> {
    let mut last = match tcp_once(attempt, endpoint) {
        Ok(value) => return Ok(value),
        Err(err) if err.is_connect_failure() => err,
        Err(err) => return Err(err),
    };
    for delay_s in CONNECT_RETRY_DELAYS_S {
        std::thread::sleep(Duration::from_secs_f64(delay_s));
        match tcp_once(attempt, endpoint) {
            Ok(value) => return Ok(value),
            Err(err) if err.is_connect_failure() => last = err,
            Err(err) => return Err(err),
        }
    }
    Err(last)
}

fn tcp_once(
    attempt: &ForwardAttempt<'_>,
    endpoint: std::net::SocketAddr,
) -> Result<Value, ClientError> {
    let client = ProtocolClient::new(endpoint, None, attempt.config.request_timeout);
    client.request_raw(&attempt.tcp_line)
}

fn fallback_chain(
    attempt: &ForwardAttempt<'_>,
    failure: &ForwardError,
) -> Result<Value, ForwardError> {
    if let Ok(value) = exec_thin_client(attempt) {
        return Ok(value);
    }
    respawn_and_gate(attempt).map_err(|err| {
        ForwardError::SandboxUnavailable(format!("{failure}; respawn failed: {err:#}"))
    })?;
    if attempt.mutates_state {
        return Err(ForwardError::UncertainOutcome(format!(
            "{}: daemon respawned after a delivery-ambiguous failure; the original outcome is unknowable",
            attempt.record.sandbox_id
        )));
    }
    let endpoint = resolve_endpoint(attempt.record).map_err(|err| {
        ForwardError::SandboxUnavailable(format!("resolve after respawn: {err:#}"))
    })?;
    tcp_once(attempt, endpoint)
        .map_err(|err| ForwardError::SandboxUnavailable(format!("replay after respawn: {err}")))
}

/// `docker exec <container> eosd daemon --client <socket> <payload>` — the
/// daemon binary as its own thin client over its in-container AF_UNIX socket.
/// The payload is the stamped envelope WITHOUT the auth token (AF_UNIX carries
/// no auth), built here so the happy path never pays for it.
fn exec_thin_client(attempt: &ForwardAttempt<'_>) -> anyhow::Result<Value> {
    let container = handle(attempt);
    let socket = attempt
        .config
        .remote_daemon_dir
        .join("runtime.sock")
        .to_string_lossy()
        .into_owned();
    let eosd = attempt
        .config
        .remote_eosd_path
        .to_string_lossy()
        .into_owned();
    let payload = String::from_utf8(stamped_envelope_bytes(
        attempt.op,
        attempt.invocation_id,
        attempt.args,
        None,
    ))?;
    let stdout = container.exec(&[&eosd, "daemon", "--client", &socket, &payload])?;
    Ok(serde_json::from_str(stdout.trim())?)
}

fn respawn_and_gate(attempt: &ForwardAttempt<'_>) -> anyhow::Result<()> {
    let daemon = attempt.config.daemon_spec(attempt.record.tcp_port);
    handle(attempt).restart_daemon(&daemon)
}

fn handle(attempt: &ForwardAttempt<'_>) -> DaemonContainer {
    DaemonContainer::for_engine(
        attempt.record.container.clone(),
        attempt.record.token.clone(),
        &attempt.config.daemon_spec(attempt.record.tcp_port),
        attempt.record.cached_endpoint(),
    )
}

#[cfg(test)]
#[path = "../tests/unit/host.rs"]
mod tests;
