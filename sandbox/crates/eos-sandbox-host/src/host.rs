use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::Digest as _;

use crate::protocol::{
    encode_request_with_metadata, encode_request_with_trace_metadata, is_success,
    take_trace_sidecar, ClientError, ProtocolClient, TraceWireContext, TraceWireLinkHint,
    CONNECT_RETRY_DELAYS_S, DEFAULT_LAYER_STACK_ROOT, HEARTBEAT_OP, READY_OP,
};
use crate::runtime::{
    container_labels, docker, resolve_published_addr, running_container_ids, ContainerLifetime,
    ContainerSpec, DaemonContainer, DaemonSpec,
};
use crate::trace_store::{
    RequestStartInput, ResponseMissingInput, ResponsePersistedInput, TraceEventInput, TraceStore,
    TraceStoreError,
};
use eos_trace::{RequestId, TraceId};

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

#[derive(Debug)]
pub struct SandboxStatus {
    pub sandbox_id: String,
    pub container: String,
    pub endpoint: Option<SocketAddr>,
    pub created_by: String,
    pub daemon: Value,
}

#[derive(Debug, Clone)]
pub struct ForwardTraceContext {
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub parent_span_id: Option<u64>,
    pub link_hints: Vec<TraceWireLinkHint>,
    pub gateway_events: Vec<ForwardTraceEvent>,
}

impl ForwardTraceContext {
    #[must_use]
    pub fn new(invocation_id: &str) -> Self {
        Self {
            trace_id: TraceId::new(),
            request_id: RequestId::parse(invocation_id.to_owned()).unwrap_or_default(),
            parent_span_id: None,
            link_hints: Vec::new(),
            gateway_events: Vec::new(),
        }
    }

    pub fn push_gateway_event(&mut self, module: &str, event: &str, details: Value) {
        self.gateway_events.push(ForwardTraceEvent {
            module: module.to_owned(),
            event: event.to_owned(),
            details,
        });
    }
}

#[derive(Debug, Clone)]
pub struct ForwardTraceEvent {
    pub module: String,
    pub event: String,
    pub details: Value,
}

pub struct SandboxHost {
    config: HostConfig,
    config_yaml: String,
    registry: SandboxRegistry,
    trace_store: Arc<TraceStore>,
    trace_drainer: TraceExportDrainer,
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
        let trace_store = Arc::new(TraceStore::open(&config.state_dir)?);
        Ok(Self {
            config,
            config_yaml,
            registry,
            trace_store,
            trace_drainer: TraceExportDrainer::default(),
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
                let _ = docker(["rm", "-f", sandbox_id.as_str()]);
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
        let _ = docker(["rm", "-f", record.container.as_str()]);
        true
    }
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
            &self.trace_store,
            &self.trace_drainer,
            ForwardTraceContext::new(invocation_id),
            mutates_state,
            op,
            invocation_id,
            args,
        ))
    }

    pub fn forward_with_trace(
        &self,
        sandbox_id: &str,
        mutates_state: bool,
        op: &str,
        invocation_id: &str,
        args: &Value,
        trace: ForwardTraceContext,
    ) -> Option<Result<Value, ForwardError>> {
        let record = self.registry.get(sandbox_id)?;
        Some(forward_request(
            &record,
            &self.config,
            &self.trace_store,
            &self.trace_drainer,
            trace,
            mutates_state,
            op,
            invocation_id,
            args,
        ))
    }

    pub fn record_trace_event(
        &self,
        sandbox_id: &str,
        trace: &ForwardTraceContext,
        module: &str,
        event: &str,
        details: Value,
    ) {
        let _ = self.trace_store.append_trace_event(TraceEventInput {
            sandbox_id,
            trace_id: &trace.trace_id,
            request_id: Some(&trace.request_id),
            span_id: None,
            module,
            event,
            details,
        });
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
        let ids = running_container_ids(&[SANDBOX_ID_LABEL]);
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

    fn insert(&self, record: SandboxRecord) -> Result<()> {
        self.persist_token(&record.sandbox_id, &record.token)?;
        self.lock()
            .insert(record.sandbox_id.clone(), Arc::new(record));
        Ok(())
    }
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
    trace_store: &Arc<TraceStore>,
    trace_drainer: &TraceExportDrainer,
    trace_context: ForwardTraceContext,
    mutates_state: bool,
    op: &str,
    invocation_id: &str,
    args: &Value,
) -> Result<Value, ForwardError> {
    let trace = TraceWireContext {
        trace_id: trace_context.trace_id.to_string(),
        request_id: trace_context.request_id.to_string(),
        parent_span_id: trace_context.parent_span_id,
        link_hints: trace_context.link_hints.clone(),
        capture_budget_version: 1,
    };
    let mut tcp_line =
        encode_request_with_trace_metadata(op, invocation_id, args, Some(&record.token), &trace);
    tcp_line.push(b'\n');
    let family = host_family_from_op(op);
    let caller_id = args.get("caller_id").and_then(Value::as_str);
    let decision = trace_store
        .prepare_forward(RequestStartInput {
            sandbox_id: &record.sandbox_id,
            trace_id: trace_context.trace_id.clone(),
            request_id: trace_context.request_id.clone(),
            op,
            family: &family,
            caller_id,
            mutates_state,
            args: args.clone(),
            forwarded_bytes: &tcp_line,
        })
        .map_err(ForwardError::TraceUnavailable)?;
    let attempt = ForwardAttempt {
        record,
        config,
        trace_store: trace_store.as_ref(),
        trace_id: decision.trace_id,
        request_id: decision.request_id,
        mutates_state,
        tcp_line,
        op,
        invocation_id,
        args,
    };
    if decision.degraded {
        record_event(
            &attempt,
            "host.protocol",
            "trace_degraded",
            json!({"op": op, "mutates_state": mutates_state}),
        );
    }
    for event in trace_context.gateway_events {
        record_event(&attempt, &event.module, &event.event, event.details.clone());
    }
    record_event(
        &attempt,
        "host.protocol",
        "forward_started",
        json!({"op": op, "family": family, "mutates_state": mutates_state}),
    );
    let result = run_recovery(&attempt);
    match &result {
        Ok(response) => {
            record_event(
                &attempt,
                "host.protocol",
                "forward_finished",
                json!({"op": op, "status": response_status(response)}),
            );
            trace_drainer.schedule(record, config, Arc::clone(trace_store));
        }
        Err(err) => record_event(
            &attempt,
            "host.protocol",
            "forward_failed",
            json!({"op": op, "error_kind": forward_error_kind(err), "message": err.to_string()}),
        ),
    }
    result
}

#[derive(Clone, Default)]
struct TraceExportDrainer {
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl TraceExportDrainer {
    fn schedule(&self, record: &SandboxRecord, config: &HostConfig, trace_store: Arc<TraceStore>) {
        let sandbox_id = record.sandbox_id.clone();
        {
            let mut in_flight = self
                .in_flight
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !in_flight.insert(sandbox_id.clone()) {
                return;
            }
        }

        let target = TraceDrainTarget {
            sandbox_id,
            token: record.token.clone(),
            endpoint: record.cached_endpoint(),
            request_timeout: config.request_timeout,
        };
        let drainer = self.clone();
        std::thread::spawn(move || {
            let _ = drain_trace_export_once(&target, &trace_store);
            drainer.finish(&target.sandbox_id);
        });
    }

    fn finish(&self, sandbox_id: &str) {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(sandbox_id);
    }
}

struct TraceDrainTarget {
    sandbox_id: String,
    token: String,
    endpoint: Option<SocketAddr>,
    request_timeout: Duration,
}

fn drain_trace_export_once(
    target: &TraceDrainTarget,
    trace_store: &TraceStore,
) -> anyhow::Result<()> {
    let Some(endpoint) = target.endpoint else {
        return Ok(());
    };
    let client = ProtocolClient::new(endpoint, None, target.request_timeout);
    let args = json!({"max_records": 64});
    let mut line = encode_request_with_metadata(
        "sandbox.trace.export",
        "trace-export-drain",
        &args,
        Some(&target.token),
    );
    line.push(b'\n');
    let mut response = client.request_raw_observed(&line)?;
    if let Some(sidecar) = take_trace_sidecar(&mut response.value) {
        let _ = trace_store.ingest_trace_batch(&target.sandbox_id, &sidecar);
    }
    if let Some(encoded) = response
        .value
        .get("trace_batch_base64")
        .and_then(Value::as_str)
    {
        let batch = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        let _ = trace_store.ingest_trace_batch(&target.sandbox_id, &batch);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    #[error("trace store unavailable before forwarding: {0}")]
    TraceUnavailable(TraceStoreError),
    #[error("sandbox unavailable: {0}")]
    SandboxUnavailable(String),
    #[error("uncertain outcome: {0}")]
    UncertainOutcome(String),
}

fn host_family_from_op(op: &str) -> String {
    op.split('.').nth(1).unwrap_or("unknown").to_owned()
}

struct ForwardAttempt<'a> {
    record: &'a SandboxRecord,
    config: &'a HostConfig,
    trace_store: &'a TraceStore,
    trace_id: TraceId,
    request_id: RequestId,
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
        Err(err) => {
            record_event(
                attempt,
                "host.transport",
                "endpoint_refresh_failed",
                json!({"reason": "resolve endpoint", "error": err.to_string()}),
            );
            return fallback_chain(attempt, &unavailable("resolve endpoint", &err));
        }
    };
    match tcp_with_connect_backoff(attempt, endpoint) {
        Ok(value) => Ok(value),
        Err(err) if err.is_connect_failure() => match resolve_endpoint(attempt.record) {
            Ok(addr) => {
                record_event(
                    attempt,
                    "host.transport",
                    "endpoint_refreshed",
                    json!({"old_endpoint": endpoint.to_string(), "new_endpoint": addr.to_string()}),
                );
                match tcp_once(attempt, addr, retry_attempt_index()) {
                    Ok(value) => Ok(value),
                    Err(err) => {
                        fallback_chain(attempt, &unavailable("retry after re-resolve", &err))
                    }
                }
            }
            Err(err) => fallback_chain(attempt, &unavailable("re-resolve endpoint", &err)),
        },
        Err(err) => {
            if attempt.mutates_state {
                restore_if_unreachable(attempt);
                record_missing(
                    attempt,
                    "uncertain",
                    client_error_kind(&err),
                    &format!("delivery-ambiguous daemon transport failure: {err}"),
                );
                record_event(
                    attempt,
                    "host.protocol",
                    "uncertain_outcome",
                    json!({"error_kind": client_error_kind(&err), "message": err.to_string()}),
                );
                return Err(ForwardError::UncertainOutcome(format!(
                    "{}: {err}",
                    attempt.record.sandbox_id
                )));
            }
            fallback_chain(attempt, &unavailable("tcp request", &err))
        }
    }
}

fn restore_if_unreachable(attempt: &ForwardAttempt<'_>) {
    let probe = resolve_endpoint(attempt.record).ok().and_then(|endpoint| {
        let client = ProtocolClient::new(endpoint, None, Duration::from_secs(2));
        let mut line = encode_request_with_metadata(
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
    let _ = respawn_and_gate_traced(attempt);
}

fn tcp_with_connect_backoff(
    attempt: &ForwardAttempt<'_>,
    endpoint: std::net::SocketAddr,
) -> Result<Value, ClientError> {
    let mut attempt_index = 0_u32;
    let mut last = match tcp_once(attempt, endpoint, attempt_index) {
        Ok(value) => return Ok(value),
        Err(err) if err.is_connect_failure() => err,
        Err(err) => return Err(err),
    };
    for delay_s in CONNECT_RETRY_DELAYS_S {
        attempt_index = attempt_index.saturating_add(1);
        record_event(
            attempt,
            "host.transport",
            "retry_scheduled",
            json!({"attempt_index": attempt_index, "delay_ms": duration_ms(Duration::from_secs_f64(delay_s)), "reason": client_error_kind(&last)}),
        );
        std::thread::sleep(Duration::from_secs_f64(delay_s));
        match tcp_once(attempt, endpoint, attempt_index) {
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
    attempt_index: u32,
) -> Result<Value, ClientError> {
    let client = ProtocolClient::new(endpoint, None, attempt.config.request_timeout);
    record_event(
        attempt,
        "host.transport",
        "connect_started",
        json!({
            "sandbox_id": attempt.record.sandbox_id,
            "endpoint": endpoint.to_string(),
            "resolved_addr": endpoint.to_string(),
            "attempt_index": attempt_index,
            "timeout_ms": duration_ms(attempt.config.request_timeout),
        }),
    );
    let started = Instant::now();
    let mut response = match client.request_raw_observed(&attempt.tcp_line) {
        Ok(response) => response,
        Err(err) => {
            record_client_error(attempt, endpoint, attempt_index, started, &err);
            return Err(err);
        }
    };
    let elapsed = elapsed_us(started);
    record_event(
        attempt,
        "host.transport",
        "connect_finished",
        json!({
            "sandbox_id": attempt.record.sandbox_id,
            "endpoint": endpoint.to_string(),
            "resolved_addr": endpoint.to_string(),
            "attempt_index": attempt_index,
            "connect_duration_us": elapsed,
        }),
    );
    record_event(
        attempt,
        "host.transport",
        "request_written",
        json!({
            "request_bytes": attempt.tcp_line.len(),
            "protocol_version": crate::protocol::DAEMON_PROTOCOL_VERSION,
            "auth_token_present": true,
            "write_duration_us": elapsed,
        }),
    );
    let sidecar_ingested = ingest_and_strip_sidecar(attempt, &mut response.value);
    record_event(
        attempt,
        "host.transport",
        "response_read",
        json!({
            "response_bytes": response.raw_bytes.len(),
            "read_duration_us": elapsed,
            "response_digest": sha256_hex(&response.raw_bytes),
            "sidecar_ingested": sidecar_ingested,
        }),
    );
    record_response_persisted(
        attempt,
        &response.value,
        &response.raw_bytes,
        elapsed / 1000,
    );
    Ok(response.value)
}

fn fallback_chain(
    attempt: &ForwardAttempt<'_>,
    failure: &ForwardError,
) -> Result<Value, ForwardError> {
    record_event(
        attempt,
        "host.transport",
        "fallback_chain_started",
        json!({"sandbox_id": attempt.record.sandbox_id, "reason": failure.to_string()}),
    );
    if let Ok(value) = exec_thin_client(attempt) {
        return Ok(value);
    }
    respawn_and_gate_traced(attempt).map_err(|err| {
        let message = format!("{failure}; respawn failed: {err:#}");
        record_missing(attempt, "error", "sandbox_unavailable", &message);
        ForwardError::SandboxUnavailable(message)
    })?;
    if attempt.mutates_state {
        record_missing(
            attempt,
            "uncertain",
            "uncertain_outcome",
            "daemon respawned after a delivery-ambiguous failure",
        );
        return Err(ForwardError::UncertainOutcome(format!(
            "{}: daemon respawned after a delivery-ambiguous failure; the original outcome is unknowable",
            attempt.record.sandbox_id
        )));
    }
    let endpoint = resolve_endpoint(attempt.record).map_err(|err| {
        ForwardError::SandboxUnavailable(format!("resolve after respawn: {err:#}"))
    })?;
    tcp_once(attempt, endpoint, retry_attempt_index()).map_err(|err| {
        let message = format!("replay after respawn: {err}");
        record_missing(attempt, "error", client_error_kind(&err), &message);
        ForwardError::SandboxUnavailable(message)
    })
}

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
    let trace = TraceWireContext {
        trace_id: attempt.trace_id.to_string(),
        request_id: attempt.request_id.to_string(),
        parent_span_id: None,
        link_hints: Vec::new(),
        capture_budget_version: 1,
    };
    let payload = String::from_utf8(encode_request_with_trace_metadata(
        attempt.op,
        attempt.invocation_id,
        attempt.args,
        None,
        &trace,
    ))?;
    record_event(
        attempt,
        "host.transport",
        "exec_client_started",
        json!({
            "sandbox_id": attempt.record.sandbox_id,
            "container": attempt.record.container,
            "remote_socket_path": socket,
            "mutates_state": attempt.mutates_state,
        }),
    );
    let started = Instant::now();
    let stdout = match container.exec(&[&eosd, "daemon", "--client", &socket, &payload]) {
        Ok(stdout) => stdout,
        Err(err) => {
            record_event(
                attempt,
                "host.transport",
                "exec_client_failed",
                json!({"duration_us": elapsed_us(started), "error_kind": "exec_failed", "message": err.to_string()}),
            );
            return Err(err);
        }
    };
    let mut value = serde_json::from_str(stdout.trim())?;
    let sidecar_ingested = ingest_and_strip_sidecar(attempt, &mut value);
    record_event(
        attempt,
        "host.transport",
        "exec_client_finished",
        json!({"duration_us": elapsed_us(started), "sidecar_ingested": sidecar_ingested}),
    );
    record_response_persisted(
        attempt,
        &value,
        stdout.as_bytes(),
        elapsed_us(started) / 1000,
    );
    Ok(value)
}

fn ingest_and_strip_sidecar(attempt: &ForwardAttempt<'_>, response: &mut Value) -> bool {
    if let Some(batch) = take_trace_sidecar(response) {
        let _ = attempt
            .trace_store
            .ingest_trace_batch(&attempt.record.sandbox_id, &batch);
        return true;
    }
    false
}

fn respawn_and_gate_traced(attempt: &ForwardAttempt<'_>) -> anyhow::Result<()> {
    let daemon = attempt.config.daemon_spec(attempt.record.tcp_port);
    record_event(
        attempt,
        "host.transport",
        "daemon_respawn_started",
        json!({"sandbox_id": attempt.record.sandbox_id, "container": attempt.record.container}),
    );
    let started = Instant::now();
    match handle(attempt).restart_daemon(&daemon) {
        Ok(()) => {
            record_event(
                attempt,
                "host.transport",
                "daemon_respawn_finished",
                json!({"duration_us": elapsed_us(started)}),
            );
            Ok(())
        }
        Err(err) => {
            record_event(
                attempt,
                "host.transport",
                "daemon_respawn_failed",
                json!({"duration_us": elapsed_us(started), "error_kind": "respawn_failed", "message": err.to_string()}),
            );
            Err(err)
        }
    }
}

fn handle(attempt: &ForwardAttempt<'_>) -> DaemonContainer {
    DaemonContainer::for_engine(
        attempt.record.container.clone(),
        attempt.record.token.clone(),
        &attempt.config.daemon_spec(attempt.record.tcp_port),
        attempt.record.cached_endpoint(),
    )
}

fn record_client_error(
    attempt: &ForwardAttempt<'_>,
    endpoint: SocketAddr,
    attempt_index: u32,
    started: Instant,
    error: &ClientError,
) {
    let details = json!({
        "sandbox_id": attempt.record.sandbox_id,
        "endpoint": endpoint.to_string(),
        "resolved_addr": endpoint.to_string(),
        "attempt_index": attempt_index,
        "error_kind": client_error_kind(error),
        "duration_us": elapsed_us(started),
        "message": error.to_string(),
    });
    let mut details = match details {
        Value::Object(details) => details,
        _ => unreachable!("json object"),
    };
    let (event, duration_field) = match error {
        ClientError::Connect { .. } => ("connect_failed", "connect_duration_us"),
        ClientError::Write(_) => ("write_failed", "write_duration_us"),
        ClientError::EmptyResponse => ("empty_response", "read_duration_us"),
        ClientError::Decode { .. } => ("decode_failed", "read_duration_us"),
        ClientError::Read(_) | ClientError::Io(_) => ("read_failed", "read_duration_us"),
    };
    details.insert(duration_field.to_owned(), json!(elapsed_us(started)));
    record_event(attempt, "host.transport", event, Value::Object(details));
}

fn record_event(attempt: &ForwardAttempt<'_>, module: &str, event: &str, details: Value) {
    let _ = attempt.trace_store.append_trace_event(TraceEventInput {
        sandbox_id: &attempt.record.sandbox_id,
        trace_id: &attempt.trace_id,
        request_id: Some(&attempt.request_id),
        span_id: None,
        module,
        event,
        details,
    });
}

fn record_response_persisted(
    attempt: &ForwardAttempt<'_>,
    response: &Value,
    raw_response_bytes: &[u8],
    host_rtt_ms: u64,
) {
    let _ = attempt
        .trace_store
        .record_response_persisted(ResponsePersistedInput {
            sandbox_id: &attempt.record.sandbox_id,
            trace_id: &attempt.trace_id,
            request_id: &attempt.request_id,
            response,
            raw_response_bytes,
            host_rtt_ms,
        });
}

fn record_missing(attempt: &ForwardAttempt<'_>, status: &str, error_kind: &str, message: &str) {
    record_event(
        attempt,
        "host.protocol",
        "response_missing",
        json!({"status": status, "error_kind": error_kind, "message": message}),
    );
    let _ = attempt
        .trace_store
        .record_response_missing(ResponseMissingInput {
            sandbox_id: &attempt.record.sandbox_id,
            trace_id: &attempt.trace_id,
            request_id: &attempt.request_id,
            status,
            error_kind,
            message,
        });
}

fn client_error_kind(error: &ClientError) -> &'static str {
    match error {
        ClientError::Connect { .. } => "connect_failed",
        ClientError::Io(_) => "transport_io",
        ClientError::Write(_) => "write_failed",
        ClientError::Read(source) if source.kind() == std::io::ErrorKind::TimedOut => {
            "read_timeout"
        }
        ClientError::Read(_) => "read_failed",
        ClientError::EmptyResponse => "empty_response",
        ClientError::Decode { .. } => "decode_failed",
    }
}

fn forward_error_kind(error: &ForwardError) -> &'static str {
    match error {
        ForwardError::TraceUnavailable(_) => "trace_unavailable",
        ForwardError::SandboxUnavailable(_) => "sandbox_unavailable",
        ForwardError::UncertainOutcome(_) => "uncertain_outcome",
    }
}

fn response_status(response: &Value) -> String {
    if response.get("success") == Some(&Value::Bool(false)) {
        "error".to_owned()
    } else {
        response
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("ok")
            .to_owned()
    }
}

fn retry_attempt_index() -> u32 {
    u32::try_from(CONNECT_RETRY_DELAYS_S.len()).unwrap_or(u32::MAX)
}

fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(test)]
#[path = "../tests/unit/host.rs"]
mod tests;
