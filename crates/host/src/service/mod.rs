use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::protocol::catalog::{
    BuiltinOp, OpContract, HOST_CONTAINER_ADOPT, HOST_CONTAINER_LIST, HOST_CONTAINER_REMOVE,
    HOST_CONTAINER_START, HOST_CONTAINER_STOP, HOST_IMAGE_LIST, HOST_IMAGE_PROFILES_LIST,
    HOST_IMAGE_PULL, HOST_SANDBOX_ACQUIRE, HOST_SANDBOX_RELEASE, HOST_TRACE_REQUESTS,
    HOST_TRACE_SHOW, HOST_TRACE_VERIFY,
};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::container::{
    docker, resolve_published_addr, ContainerLifetime, ContainerSpec, DaemonContainer, DaemonSpec,
};
use crate::daemon_wire::{
    response_is_accepted, ProtocolClient, DEFAULT_LAYER_STACK_ROOT, READY_OP,
};
use crate::trace_store::{
    RequestStartInput, ResponseMissingInput, ResponsePersistedInput, TraceEventInput, TraceStore,
};
use trace::{RequestId, TraceId};

mod forward;
mod registry;
mod trace_drain;

pub use forward::ForwardError;

use forward::{forward_request, ForwardRequestInput};
use registry::{
    SandboxRecord, SandboxRegistry, CREATED_BY_LABEL, SANDBOX_ID_LABEL, TCP_PORT_LABEL,
};
use trace_drain::TraceExportDrainer;

const TRACE_SHOW_DEFAULT_SECTION_LIMIT: usize = 1_000;
const TRACE_SHOW_MAX_SECTION_LIMIT: usize = 5_000;

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub image: String,
    pub platform: Option<String>,
    pub docker_privileged: bool,
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
    pub(crate) fn daemon_spec(&self, tcp_port: u16) -> DaemonSpec {
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
    pub(crate) gateway_events: Vec<ForwardTraceEvent>,
}

impl ForwardTraceContext {
    #[must_use]
    pub fn new(invocation_id: &str) -> Self {
        Self {
            trace_id: TraceId::new(),
            request_id: RequestId::parse(invocation_id.to_owned()).unwrap_or_default(),
            parent_span_id: None,
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
pub(crate) struct ForwardTraceEvent {
    pub module: String,
    pub event: String,
    pub details: Value,
}

pub struct HostForwardRequest<'a> {
    pub sandbox_id: &'a str,
    pub mutates_state: bool,
    pub family: &'a str,
    pub op: &'a str,
    pub invocation_id: &'a str,
    pub args: &'a Value,
    pub trace: ForwardTraceContext,
}

pub struct SandboxHost {
    config: HostConfig,
    config_yaml: String,
    registry: Arc<SandboxRegistry>,
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
        let registry = Arc::new(SandboxRegistry::open(config.state_dir.clone())?);
        registry.rebuild_from_docker();
        let trace_store = Arc::new(TraceStore::open(&config.state_dir)?);
        let trace_drainer = TraceExportDrainer::default();
        trace_drainer.spawn_periodic(
            Arc::clone(&registry),
            config.clone(),
            Arc::clone(&trace_store),
        );
        Ok(Self {
            config,
            config_yaml,
            registry,
            trace_store,
            trace_drainer,
        })
    }

    pub fn acquire(&self) -> Result<String> {
        let trace = ForwardTraceContext::new("host-acquire");
        self.acquire_with_trace(&trace, &json!({}))
    }

    pub fn acquire_with_trace(&self, trace: &ForwardTraceContext, args: &Value) -> Result<String> {
        let sandbox_id = format!("sb-{}", random_hex(16)?);
        let contract = BuiltinOp::HostSandboxAcquire.contract();
        let (image, platform) = self.resolve_image_profile(args)?;
        self.start_managed_sandbox(
            contract,
            trace,
            args,
            sandbox_id,
            image,
            platform,
            HOST_SANDBOX_ACQUIRE,
        )
    }

    fn start_managed_sandbox(
        &self,
        contract: &OpContract,
        trace: &ForwardTraceContext,
        args: &Value,
        sandbox_id: String,
        image: String,
        platform: Option<String>,
        response_op: &str,
    ) -> Result<String> {
        validate_container_name(&sandbox_id)?;
        let op_started = Instant::now();
        self.trace_store.prepare_forward(RequestStartInput {
            sandbox_id: &sandbox_id,
            trace_id: trace.trace_id.clone(),
            request_id: trace.request_id.clone(),
            op: contract.name,
            family: contract.family.as_str(),
            caller_id: args.get("caller_id").and_then(Value::as_str),
            mutates_state: contract.mutates_state,
            args: args.clone(),
        })?;
        self.record_host_gateway_events(&sandbox_id, trace);
        let token = random_hex(32)?;
        let forward_token = random_hex(32)?;
        let container = ContainerSpec {
            name: sandbox_id.clone(),
            image: image.clone(),
            platform: platform.clone(),
            privileged: self.config.docker_privileged,
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
        let record = SandboxRecord::new_with_forward_token(
            sandbox_id.clone(),
            sandbox_id.clone(),
            token.clone(),
            forward_token.clone(),
            self.config.tcp_port,
            self.config.created_by.clone(),
            None,
        );
        let record = self.registry.insert(record)?;
        self.record_host_lifecycle_event(
            &sandbox_id,
            trace,
            "container_start_started",
            json!({"image": image, "platform": platform, "tcp_port": self.config.tcp_port}),
        );
        let started_container = match DaemonContainer::start_with_forward_token(
            &container,
            &daemon,
            token.clone(),
            forward_token.clone(),
        ) {
            Ok(started) => started,
            Err(err) => {
                self.registry.remove(&sandbox_id);
                let _ = docker(["rm", "-f", sandbox_id.as_str()]);
                let response = host_error_response(
                    response_op,
                    trace,
                    "sandbox_unavailable",
                    &format!("container start failed: {err:#}"),
                );
                let _ =
                    self.record_host_response_or_missing(&sandbox_id, trace, &response, op_started);
                return Err(err);
            }
        };
        self.record_host_lifecycle_event(
            &sandbox_id,
            trace,
            "container_start_finished",
            json!({"container": sandbox_id.clone(), "endpoint": started_container.client().addr().to_string()}),
        );
        record.cache_endpoint(started_container.client().addr());
        let response = host_ok_response(
            response_op,
            trace,
            json!({"sandbox_id": sandbox_id.clone()}),
        );
        self.record_host_response_or_missing(&sandbox_id, trace, &response, op_started)?;
        Ok(sandbox_id)
    }

    pub fn release(&self, sandbox_id: &str) -> bool {
        let trace = ForwardTraceContext::new("host-release");
        self.release_with_trace(sandbox_id, &trace, &json!({}))
            .unwrap_or(false)
    }

    pub fn release_with_trace(
        &self,
        sandbox_id: &str,
        trace: &ForwardTraceContext,
        args: &Value,
    ) -> Result<bool> {
        let op_started = Instant::now();
        let contract = BuiltinOp::HostSandboxRelease.contract();
        self.trace_store.prepare_forward(RequestStartInput {
            sandbox_id,
            trace_id: trace.trace_id.clone(),
            request_id: trace.request_id.clone(),
            op: contract.name,
            family: contract.family.as_str(),
            caller_id: args.get("caller_id").and_then(Value::as_str),
            mutates_state: contract.mutates_state,
            args: args.clone(),
        })?;
        self.record_host_gateway_events(sandbox_id, trace);
        let Some(record) = self.registry.get(sandbox_id) else {
            let response = host_error_response(
                HOST_SANDBOX_RELEASE,
                trace,
                "unknown_sandbox",
                &format!("unknown sandbox: {sandbox_id}"),
            );
            self.record_host_response_or_missing(sandbox_id, trace, &response, op_started)?;
            return Ok(false);
        };
        self.record_host_lifecycle_event(
            sandbox_id,
            trace,
            "container_removal_started",
            json!({"container": record.container.clone()}),
        );
        let docker_result = docker(["rm", "-f", record.container.as_str()]);
        self.record_host_lifecycle_event(
            sandbox_id,
            trace,
            "container_removal_finished",
            json!({
                "container": record.container.clone(),
                "removed": docker_result.is_ok(),
                "error": docker_result.as_ref().err().map(ToString::to_string),
            }),
        );
        match docker_result {
            Ok(_) => {
                self.registry.remove(sandbox_id);
                let response = host_ok_response(
                    HOST_SANDBOX_RELEASE,
                    trace,
                    json!({"sandbox_id": sandbox_id}),
                );
                self.record_host_response_or_missing(sandbox_id, trace, &response, op_started)?;
                Ok(true)
            }
            Err(err) => {
                let message = format!("container removal failed: {err:#}");
                let response = host_error_response(
                    HOST_SANDBOX_RELEASE,
                    trace,
                    "sandbox_unavailable",
                    &message,
                );
                self.record_host_response_or_missing(sandbox_id, trace, &response, op_started)?;
                Err(err.context(format!("remove sandbox container {}", record.container)))
            }
        }
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

    pub fn image_profiles_list(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let result: Result<Value> = Ok(json!({
            "profiles": [{
                "name": "default",
                "image": self.config.image.clone(),
                "platform": self.config.platform.clone(),
                "default": true,
            }]
        }));
        self.record_operator_trace_read(
            None,
            trace,
            HOST_IMAGE_PROFILES_LIST,
            args,
            json!({"status": "ok", "result_count": 1}),
        );
        result
    }

    pub fn image_list(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let result = docker(["image", "ls", "--format", "{{json .}}"])
            .and_then(|out| parse_json_lines(&out).map(|images| json!({"images": images})));
        self.record_operator_trace_read(
            None,
            trace,
            HOST_IMAGE_LIST,
            args,
            host_result_summary(&result),
        );
        result
    }

    pub fn image_pull(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let result = (|| {
            let image = required_string_arg(args, "image")?;
            self.ensure_operator_image_allowed(image)?;
            let platform =
                optional_string_arg(args, "platform").or(self.config.platform.as_deref());
            let mut pull = vec!["pull".to_owned()];
            if let Some(platform) = platform {
                pull.extend(["--platform".to_owned(), platform.to_owned()]);
            }
            pull.push(image.to_owned());
            let output = docker(pull)?;
            Ok(json!({
                "image": image,
                "platform": platform,
                "pulled": true,
                "output": output,
            }))
        })();
        self.record_operator_trace_read(
            None,
            trace,
            HOST_IMAGE_PULL,
            args,
            host_result_summary(&result),
        );
        result
    }

    pub fn container_list(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let result = docker(["ps", "-a", "--format", "{{json .}}"]).and_then(|out| {
            let mut containers = parse_json_lines(&out)?;
            mark_managed_containers(&mut containers, &self.registry.list());
            Ok(json!({"containers": containers}))
        });
        self.record_operator_trace_read(
            None,
            trace,
            HOST_CONTAINER_LIST,
            args,
            host_result_summary(&result),
        );
        result
    }

    pub fn container_start(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let result = (|| {
            let image = required_string_arg(args, "image")?;
            let platform = optional_string_arg(args, "platform")
                .map(str::to_owned)
                .or_else(|| self.config.platform.clone());
            let sandbox_id = match optional_string_arg(args, "name") {
                Some(name) => name.to_owned(),
                None => format!("sb-{}", random_hex(16)?),
            };
            let contract = BuiltinOp::HostContainerStart.contract();
            let sandbox_id = self.start_managed_sandbox(
                contract,
                trace,
                args,
                sandbox_id,
                image.to_owned(),
                platform.clone(),
                HOST_CONTAINER_START,
            )?;
            Ok(json!({
                "sandbox_id": sandbox_id,
                "container": sandbox_id,
                "image": image,
                "platform": platform,
            }))
        })();
        self.record_operator_trace_read(
            None,
            trace,
            HOST_CONTAINER_START,
            args,
            host_result_summary(&result),
        );
        result
    }

    pub fn container_adopt(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let result = (|| {
            let container = required_string_arg(args, "container")?;
            let sandbox_id = optional_string_arg(args, "sandbox_id").unwrap_or(container);
            validate_container_name(sandbox_id)?;
            let tcp_port = optional_u16_arg(args, "tcp_port")?.unwrap_or(self.config.tcp_port);
            let token = optional_string_arg(args, "auth_token")
                .map(str::to_owned)
                .or_else(|| self.registry.load_token(sandbox_id).ok())
                .with_context(|| {
                    format!(
                        "auth_token is required when no persisted token exists for {sandbox_id}"
                    )
                })?;
            let forward_token = optional_string_arg(args, "forward_auth_token")
                .map(str::to_owned)
                .or_else(|| self.registry.load_forward_token(sandbox_id).ok())
                .unwrap_or_else(|| token.clone());
            let endpoint = resolve_published_addr(container, tcp_port)?.with_context(|| {
                format!("no published port {tcp_port} for container {container}")
            })?;
            let record = SandboxRecord::new_with_forward_token(
                sandbox_id.to_owned(),
                container.to_owned(),
                token,
                forward_token,
                tcp_port,
                self.config.created_by.clone(),
                Some(endpoint),
            );
            let record = self.registry.insert(record)?;
            let readiness = self.probe_readiness(&record);
            if !response_is_accepted(&readiness) {
                self.registry.remove(sandbox_id);
                bail!("container {container} daemon readiness failed: {readiness}");
            }
            Ok(json!({
                "sandbox_id": sandbox_id,
                "container": container,
                "endpoint": endpoint.to_string(),
                "daemon": readiness,
            }))
        })();
        self.record_operator_trace_read(
            None,
            trace,
            HOST_CONTAINER_ADOPT,
            args,
            host_result_summary(&result),
        );
        result
    }

    pub fn container_stop(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let result = (|| {
            let target = self.resolve_container_target(args)?;
            docker(["stop", target.container.as_str()])?;
            Ok(json!({
                "container": target.container,
                "sandbox_id": target.sandbox_id,
                "stopped": true,
            }))
        })();
        self.record_operator_trace_read(
            None,
            trace,
            HOST_CONTAINER_STOP,
            args,
            host_result_summary(&result),
        );
        result
    }

    pub fn container_remove(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let result = (|| {
            let target = self.resolve_container_target(args)?;
            docker(["rm", "-f", target.container.as_str()])?;
            if let Some(sandbox_id) = &target.sandbox_id {
                self.registry.remove(sandbox_id);
            }
            Ok(json!({
                "container": target.container,
                "sandbox_id": target.sandbox_id,
                "removed": true,
            }))
        })();
        self.record_operator_trace_read(
            None,
            trace,
            HOST_CONTAINER_REMOVE,
            args,
            host_result_summary(&result),
        );
        result
    }

    fn resolve_image_profile(&self, args: &Value) -> Result<(String, Option<String>)> {
        let profile = optional_string_arg(args, "image_profile").unwrap_or("default");
        if profile != "default" {
            bail!("unknown image_profile: {profile}");
        }
        Ok((self.config.image.clone(), self.config.platform.clone()))
    }

    fn ensure_operator_image_allowed(&self, image: &str) -> Result<()> {
        if image == self.config.image {
            return Ok(());
        }
        bail!("image {image:?} is not approved by host policy")
    }

    fn resolve_container_target(&self, args: &Value) -> Result<HostContainerTarget> {
        if let Some(sandbox_id) = optional_string_arg(args, "sandbox_id") {
            let record = self
                .registry
                .get(sandbox_id)
                .with_context(|| format!("unknown sandbox_id: {sandbox_id}"))?;
            return Ok(HostContainerTarget {
                sandbox_id: Some(sandbox_id.to_owned()),
                container: record.container.clone(),
            });
        }
        let container = required_string_arg(args, "container")?;
        validate_container_name(container)?;
        let sandbox_id = self
            .registry
            .list()
            .into_iter()
            .find(|record| record.container == container)
            .map(|record| record.sandbox_id.clone());
        Ok(HostContainerTarget {
            sandbox_id,
            container: container.to_owned(),
        })
    }

    pub fn trace_requests(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let sandbox_id = args.get("sandbox_id").and_then(Value::as_str);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(100)
            .clamp(1, 1_000) as usize;
        let result = self.trace_store.recent_requests(sandbox_id, limit);
        self.record_operator_trace_read(
            sandbox_id,
            trace,
            HOST_TRACE_REQUESTS,
            args,
            match &result {
                Ok(requests) => json!({"status": "ok", "result_count": requests.len()}),
                Err(err) => json!({"status": "error", "message": err.to_string()}),
            },
        );
        Ok(result.map(|requests| json!({"requests": requests}))?)
    }

    pub fn trace_show(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let trace_id = required_string_arg(args, "trace_id")?;
        let section_limit = trace_show_section_limit(args);
        let query_limit = section_limit.saturating_add(1);
        let result: std::result::Result<Value, crate::trace_store::TraceStoreError> = (|| {
            let (requests, requests_truncated) = trim_limited(
                self.trace_store
                    .requests_for_trace_limited(trace_id, query_limit)?,
                section_limit,
            );
            let (spans, spans_truncated) = trim_limited(
                self.trace_store
                    .spans_for_trace_limited(trace_id, query_limit)?,
                section_limit,
            );
            let (events, events_truncated) = trim_limited(
                self.trace_store
                    .events_for_trace_limited(trace_id, query_limit)?,
                section_limit,
            );
            let (resources, resources_truncated) = trim_limited(
                self.trace_store
                    .resources_for_trace_limited(trace_id, query_limit)?,
                section_limit,
            );
            let (links, links_truncated) = trim_limited(
                self.trace_store
                    .links_for_trace_limited(trace_id, query_limit)?,
                section_limit,
            );
            let (audit_entries, audit_entries_truncated) = trim_limited(
                self.trace_store
                    .audit_entries_for_trace_limited(trace_id, query_limit)?,
                section_limit,
            );
            Ok(json!({
                "trace_id": trace_id,
                "limits": {
                    "per_section": section_limit,
                },
                "counts": {
                    "requests": requests.len(),
                    "spans": spans.len(),
                    "events": events.len(),
                    "resources": resources.len(),
                    "links": links.len(),
                    "audit_entries": audit_entries.len(),
                },
                "truncated": {
                    "requests": requests_truncated,
                    "spans": spans_truncated,
                    "events": events_truncated,
                    "resources": resources_truncated,
                    "links": links_truncated,
                    "audit_entries": audit_entries_truncated,
                },
                "requests": requests,
                "spans": spans,
                "events": events,
                "resources": resources,
                "links": links,
                "audit_entries": audit_entries,
            }))
        })();
        self.record_operator_trace_read(
            args.get("sandbox_id").and_then(Value::as_str),
            trace,
            HOST_TRACE_SHOW,
            args,
            match &result {
                Ok(value) => json!({
                    "status": "ok",
                    "request_count": value["requests"].as_array().map_or(0, Vec::len),
                    "span_count": value["spans"].as_array().map_or(0, Vec::len),
                    "event_count": value["events"].as_array().map_or(0, Vec::len),
                    "audit_entry_count": value["audit_entries"].as_array().map_or(0, Vec::len),
                    "truncated": value["truncated"].clone(),
                    "limit": section_limit,
                }),
                Err(err) => json!({"status": "error", "message": err.to_string()}),
            },
        );
        Ok(result?)
    }

    pub fn trace_verify(&self, trace: &ForwardTraceContext, args: &Value) -> Result<Value> {
        let trace_id = args.get("trace_id").and_then(Value::as_str);
        let report = self.trace_store.verify_audit(trace_id)?;
        self.record_operator_trace_read(
            args.get("sandbox_id").and_then(Value::as_str),
            trace,
            HOST_TRACE_VERIFY,
            args,
            json!({
                "status": "ok",
                "ok": report.ok,
                "scope": report.scope.as_str(),
                "checked_entries": report.checked_entries,
                "first_error_kind": report.first_error.as_ref().map(|failure| failure.kind.as_str()),
            }),
        );
        Ok(serde_json::to_value(report)?)
    }

    pub fn forward_with_trace(
        &self,
        request: HostForwardRequest<'_>,
    ) -> Option<Result<Value, ForwardError>> {
        let HostForwardRequest {
            sandbox_id,
            mutates_state,
            family,
            op,
            invocation_id,
            args,
            trace,
        } = request;
        let record = self.registry.get(sandbox_id)?;
        Some(forward_request(ForwardRequestInput {
            record,
            config: &self.config,
            trace_store: &self.trace_store,
            trace_drainer: &self.trace_drainer,
            trace_context: trace,
            mutates_state,
            family,
            op,
            invocation_id,
            args,
        }))
    }

    pub fn record_trace_event(
        &self,
        sandbox_id: &str,
        trace: &ForwardTraceContext,
        module: &str,
        event: &str,
        details: Value,
    ) {
        let _ = self
            .trace_store
            .append_trace_event_or_loss(TraceEventInput {
                sandbox_id,
                trace_id: &trace.trace_id,
                request_id: Some(&trace.request_id),
                span_id: None,
                module,
                event,
                details,
            });
    }

    fn record_operator_trace_read(
        &self,
        sandbox_id: Option<&str>,
        trace: &ForwardTraceContext,
        op: &str,
        args: &Value,
        outcome: Value,
    ) {
        let _ = self
            .trace_store
            .append_trace_event_or_loss(TraceEventInput {
                sandbox_id: sandbox_id.unwrap_or("_host"),
                trace_id: &trace.trace_id,
                request_id: Some(&trace.request_id),
                span_id: None,
                module: "host.trace_query",
                event: "operator_read",
                details: json!({
                    "op": op,
                    "args": trace::budget::redact_for_audit(args.clone()),
                    "outcome": outcome,
                }),
            });
    }

    fn record_host_gateway_events(&self, sandbox_id: &str, trace: &ForwardTraceContext) {
        for event in &trace.gateway_events {
            self.record_host_lifecycle_event(
                sandbox_id,
                trace,
                &event.event,
                json!({"module": event.module.clone(), "details": event.details.clone()}),
            );
        }
    }

    fn record_host_lifecycle_event(
        &self,
        sandbox_id: &str,
        trace: &ForwardTraceContext,
        event: &str,
        details: Value,
    ) {
        let _ = self
            .trace_store
            .append_trace_event_or_loss(TraceEventInput {
                sandbox_id,
                trace_id: &trace.trace_id,
                request_id: Some(&trace.request_id),
                span_id: None,
                module: "host.lifecycle",
                event,
                details,
            });
    }

    fn record_host_response(
        &self,
        sandbox_id: &str,
        trace: &ForwardTraceContext,
        response: &Value,
        started: Instant,
    ) -> Result<()> {
        let raw_response_bytes = serde_json::to_vec(response)?;
        self.trace_store
            .record_response_persisted(ResponsePersistedInput {
                sandbox_id,
                trace_id: &trace.trace_id,
                request_id: &trace.request_id,
                response,
                raw_response_bytes: &raw_response_bytes,
                host_rtt_ms: duration_ms(started.elapsed()),
            })?;
        Ok(())
    }

    fn record_host_response_or_missing(
        &self,
        sandbox_id: &str,
        trace: &ForwardTraceContext,
        response: &Value,
        started: Instant,
    ) -> Result<()> {
        if let Err(err) = self.record_host_response(sandbox_id, trace, response, started) {
            let message = format!("host response persistence failed after lifecycle result: {err}");
            let _ = self
                .trace_store
                .record_response_missing(ResponseMissingInput {
                    sandbox_id,
                    trace_id: &trace.trace_id,
                    request_id: &trace.request_id,
                    status: "uncertain",
                    error_kind: "trace_response_persist_failed",
                    message: &message,
                });
            return Err(anyhow!(message));
        }
        Ok(())
    }

    fn probe_readiness(&self, record: &SandboxRecord) -> Value {
        let Some(endpoint) = record.cached_endpoint() else {
            return json!({"ready": false, "error": "endpoint not resolved"});
        };
        let client = ProtocolClient::new_forward_authorized(
            endpoint,
            Some(record.forward_token.clone()),
            self.config.request_timeout,
        );
        match client.request(
            READY_OP,
            "status-probe",
            &json!({"layer_stack_root": DEFAULT_LAYER_STACK_ROOT}),
        ) {
            Ok(resp) if response_is_accepted(&resp) => resp,
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

fn host_ok_response(op: &str, trace: &ForwardTraceContext, result: Value) -> Value {
    json!({
        "status": "ok",
        "result": result,
        "meta": host_response_meta(op, trace),
    })
}

fn host_error_response(op: &str, trace: &ForwardTraceContext, kind: &str, message: &str) -> Value {
    json!({
        "status": "error",
        "error": {
            "kind": kind,
            "message": message,
            "details": {},
        },
        "meta": host_response_meta(op, trace),
    })
}

fn host_response_meta(op: &str, trace: &ForwardTraceContext) -> Value {
    json!({
        "op": op,
        "request_id": trace.request_id.as_str(),
        "trace": {
            "trace_id": trace.trace_id.as_str(),
            "request_id": trace.request_id.as_str(),
            "store": "local_sqlite",
            "degraded": false,
        },
    })
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn trace_show_section_limit(args: &Value) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(u64::try_from(TRACE_SHOW_DEFAULT_SECTION_LIMIT).unwrap_or(u64::MAX))
        .clamp(
            1,
            u64::try_from(TRACE_SHOW_MAX_SECTION_LIMIT).unwrap_or(u64::MAX),
        ) as usize
}

fn trim_limited<T>(mut rows: Vec<T>, limit: usize) -> (Vec<T>, bool) {
    let truncated = rows.len() > limit;
    if truncated {
        rows.truncate(limit);
    }
    (rows, truncated)
}

fn required_string_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    match args.get(name) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value),
        Some(_) => bail!("{name} must be a non-empty string"),
        None => bail!("{name} is required"),
    }
}

fn optional_string_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    match args.get(name) {
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

fn optional_u16_arg(args: &Value, name: &str) -> Result<Option<u16>> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let raw = value
        .as_u64()
        .with_context(|| format!("{name} must be an integer"))?;
    let value = u16::try_from(raw).with_context(|| format!("{name} is out of range"))?;
    Ok(Some(value))
}

fn validate_container_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('-')
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("container name must contain only ASCII letters, digits, '.', '_' or '-'");
    }
    Ok(())
}

fn parse_json_lines(output: &str) -> Result<Vec<Value>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).with_context(|| format!("parse docker JSON: {line}"))
        })
        .collect()
}

fn mark_managed_containers(containers: &mut [Value], records: &[Arc<SandboxRecord>]) {
    for container in containers {
        let names = container
            .get("Names")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_start_matches('/');
        if let Some(record) = records
            .iter()
            .find(|record| record.container == names || record.sandbox_id == names)
        {
            container["managed"] = json!(true);
            container["sandbox_id"] = json!(record.sandbox_id.clone());
        } else {
            container["managed"] = json!(false);
        }
    }
}

fn host_result_summary(result: &Result<Value>) -> Value {
    match result {
        Ok(value) => json!({"status": "ok", "result": value}),
        Err(err) => json!({"status": "error", "message": err.to_string()}),
    }
}

struct HostContainerTarget {
    sandbox_id: Option<String>,
    container: String,
}

#[cfg(test)]
#[path = "../../tests/unit/host.rs"]
mod tests;
