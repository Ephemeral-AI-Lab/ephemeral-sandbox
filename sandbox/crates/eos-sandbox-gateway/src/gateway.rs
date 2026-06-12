use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use eos_sandbox_host::protocol::strip_trace_sidecar;
use eos_sandbox_host::{ForwardError, ForwardTraceContext, SandboxHost, SandboxStatus};

const OPS_JSON: &str = include_str!("../../eos-operation/ops.json");
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_BYTES: usize = eos_sandbox_host::MAX_REQUEST_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Visibility {
    Public,
    Operator,
    Internal,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostVerb {
    Acquire,
    Release,
    Status,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Host(HostVerb),
    Daemon,
}

#[derive(Debug)]
pub(crate) struct OpEntry {
    pub(crate) name: String,
    pub(crate) route: Route,
    pub(crate) visibility: Visibility,
    mutates_state: bool,
}

pub(crate) struct Catalog {
    by_name: HashMap<String, Arc<OpEntry>>,
}

impl Catalog {
    pub(crate) fn load_builtin() -> Result<Self> {
        Self::parse(OPS_JSON)
    }

    fn parse(ops_json: &str) -> Result<Self> {
        let document: Value = serde_json::from_str(ops_json).context("parse ops.json")?;
        let ops = document
            .get("ops")
            .and_then(Value::as_array)
            .context("ops.json must carry an `ops` array")?;
        let mut by_name = HashMap::new();
        for op in ops {
            let name = str_field(op, "name")?.to_owned();
            let route = match str_field(op, "served_by")? {
                "daemon" => Route::Daemon,
                "host" => Route::Host(host_verb(&name)?),
                other => bail!("op {name}: unknown served_by {other:?}"),
            };
            let visibility = match str_field(op, "visibility")? {
                "public" => Visibility::Public,
                "operator" => Visibility::Operator,
                "internal" => Visibility::Internal,
                "test" => Visibility::Test,
                other => bail!("op {name}: unknown visibility {other:?}"),
            };
            let mutates_state = op
                .get("mutates_state")
                .and_then(Value::as_bool)
                .with_context(|| format!("op {name}: missing mutates_state"))?;
            let entry = Arc::new(OpEntry {
                name: name.clone(),
                route,
                visibility,
                mutates_state,
            });
            if by_name.insert(name.clone(), entry).is_some() {
                bail!("catalog name claimed twice: {name}");
            }
        }
        Ok(Self { by_name })
    }

    pub(crate) fn lookup(&self, op: &str) -> Option<&Arc<OpEntry>> {
        self.by_name.get(op)
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<&Arc<OpEntry>> {
        self.by_name.values().collect()
    }
}

fn host_verb(name: &str) -> Result<HostVerb> {
    match name {
        "sandbox.acquire" => Ok(HostVerb::Acquire),
        "sandbox.release" => Ok(HostVerb::Release),
        "sandbox.status" => Ok(HostVerb::Status),
        "sandbox.list" => Ok(HostVerb::List),
        other => bail!("host-served op {other} has no router implementation"),
    }
}

fn str_field<'a>(op: &'a Value, field: &str) -> Result<&'a str> {
    op.get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("catalog op missing string field {field}"))
}

pub(crate) trait Engine: Send + Sync {
    fn acquire(&self) -> Result<String>;
    fn release(&self, sandbox_id: &str) -> bool;
    fn status(&self, sandbox_id: &str) -> Option<Value>;
    fn list(&self) -> Vec<Value>;
    fn forward(
        &self,
        sandbox_id: &str,
        mutates_state: bool,
        op: &str,
        invocation_id: &str,
        args: &Value,
        trace: ForwardTraceContext,
    ) -> Option<Result<Value, ForwardError>>;

    fn record_trace_event(
        &self,
        _sandbox_id: &str,
        _trace: &ForwardTraceContext,
        _module: &str,
        _event: &str,
        _details: Value,
    ) {
    }
}

impl Engine for SandboxHost {
    fn acquire(&self) -> Result<String> {
        SandboxHost::acquire(self)
    }

    fn release(&self, sandbox_id: &str) -> bool {
        SandboxHost::release(self, sandbox_id)
    }

    fn status(&self, sandbox_id: &str) -> Option<Value> {
        SandboxHost::status(self, sandbox_id).map(|status| status_value(&status, true))
    }

    fn list(&self) -> Vec<Value> {
        SandboxHost::list(self)
            .iter()
            .map(|status| status_value(status, false))
            .collect()
    }

    fn forward(
        &self,
        sandbox_id: &str,
        mutates_state: bool,
        op: &str,
        invocation_id: &str,
        args: &Value,
        trace: ForwardTraceContext,
    ) -> Option<Result<Value, ForwardError>> {
        SandboxHost::forward_with_trace(
            self,
            sandbox_id,
            mutates_state,
            op,
            invocation_id,
            args,
            trace,
        )
    }

    fn record_trace_event(
        &self,
        sandbox_id: &str,
        trace: &ForwardTraceContext,
        module: &str,
        event: &str,
        details: Value,
    ) {
        SandboxHost::record_trace_event(self, sandbox_id, trace, module, event, details);
    }
}

fn status_value(status: &SandboxStatus, embed_daemon: bool) -> Value {
    let mut value = json!({
        "success": true,
        "sandbox_id": status.sandbox_id,
        "container": status.container,
        "endpoint": status.endpoint.map(|addr| addr.to_string()),
        "created_by": status.created_by,
    });
    if embed_daemon {
        value["daemon"] = status.daemon.clone();
    }
    value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    Client,
    Operator,
}

impl Surface {
    const fn allows(self, visibility: Visibility) -> bool {
        match visibility {
            Visibility::Public => true,
            Visibility::Operator => matches!(self, Self::Operator),
            Visibility::Internal | Visibility::Test => false,
        }
    }
}

pub(crate) fn handle(
    catalog: &Catalog,
    engine: &dyn Engine,
    surface: Surface,
    request: &ClientRequest,
) -> Value {
    let Some(entry) = catalog.lookup(&request.op) else {
        if request.op.starts_with("plugin.") {
            return forward(engine, request, true);
        }
        return error_response("unknown_op", &format!("unknown op: {}", request.op));
    };
    if !surface.allows(entry.visibility) {
        return error_response(
            "forbidden",
            &format!("op {} is not served on this socket", entry.name),
        );
    }
    match entry.route {
        Route::Daemon => forward(engine, request, entry.mutates_state),
        Route::Host(verb) => host_call(engine, verb, request),
    }
}

fn forward(engine: &dyn Engine, request: &ClientRequest, mutates_state: bool) -> Value {
    let Some(sandbox_id) = request.sandbox_id.as_deref() else {
        return error_response("invalid_request", "sandbox_id is required for this op");
    };
    let mut trace = request.trace.clone();
    trace.push_gateway_event(
        "gateway.route",
        "route_selected",
        json!({
            "op": request.op,
            "sandbox_id": sandbox_id,
            "route": "daemon",
            "visibility": "public",
            "mutates_state": mutates_state,
        }),
    );
    trace.push_gateway_event(
        "gateway.route",
        "engine_forward_started",
        json!({"op": request.op, "sandbox_id": sandbox_id, "mutates_state": mutates_state}),
    );
    let trace_for_result = trace.clone();
    let started = Instant::now();
    match engine.forward(
        sandbox_id,
        mutates_state,
        &request.op,
        &request.invocation_id,
        &request.args,
        trace,
    ) {
        Some(Ok(mut response)) => {
            engine.record_trace_event(
                sandbox_id,
                &trace_for_result,
                "gateway.route",
                "engine_forward_finished",
                json!({
                    "op": request.op,
                    "sandbox_id": sandbox_id,
                    "mutates_state": mutates_state,
                    "duration_us": elapsed_us(started),
                }),
            );
            strip_trace_sidecar(&mut response);
            response
        }
        Some(Err(ForwardError::TraceUnavailable(message))) => {
            engine.record_trace_event(
                sandbox_id,
                &trace_for_result,
                "gateway.route",
                "engine_forward_failed",
                json!({"op": request.op, "sandbox_id": sandbox_id, "error_kind": "trace_unavailable", "duration_us": elapsed_us(started)}),
            );
            error_response("trace_unavailable", &message.to_string())
        }
        Some(Err(ForwardError::UncertainOutcome(message))) => {
            engine.record_trace_event(
                sandbox_id,
                &trace_for_result,
                "gateway.route",
                "engine_forward_failed",
                json!({"op": request.op, "sandbox_id": sandbox_id, "error_kind": "uncertain_outcome", "duration_us": elapsed_us(started)}),
            );
            error_response("uncertain_outcome", &message)
        }
        Some(Err(ForwardError::SandboxUnavailable(message))) => {
            engine.record_trace_event(
                sandbox_id,
                &trace_for_result,
                "gateway.route",
                "engine_forward_failed",
                json!({"op": request.op, "sandbox_id": sandbox_id, "error_kind": "sandbox_unavailable", "duration_us": elapsed_us(started)}),
            );
            error_response("sandbox_unavailable", &message)
        }
        None => unknown_sandbox(sandbox_id),
    }
}

fn host_call(engine: &dyn Engine, verb: HostVerb, request: &ClientRequest) -> Value {
    match verb {
        HostVerb::Acquire => match engine.acquire() {
            Ok(sandbox_id) => json!({"success": true, "sandbox_id": sandbox_id}),
            Err(err) => error_response("sandbox_unavailable", &format!("acquire failed: {err:#}")),
        },
        HostVerb::List => json!({"success": true, "sandboxes": engine.list()}),
        HostVerb::Release | HostVerb::Status => {
            let Some(sandbox_id) = request.sandbox_id.as_deref() else {
                return error_response("invalid_request", "sandbox_id is required for this op");
            };
            match verb {
                HostVerb::Release => {
                    if engine.release(sandbox_id) {
                        json!({"success": true, "sandbox_id": sandbox_id})
                    } else {
                        unknown_sandbox(sandbox_id)
                    }
                }
                HostVerb::Status => match engine.status(sandbox_id) {
                    Some(status) => status,
                    None => unknown_sandbox(sandbox_id),
                },
                HostVerb::Acquire | HostVerb::List => unreachable!(),
            }
        }
    }
}

fn unknown_sandbox(sandbox_id: &str) -> Value {
    error_response("unknown_sandbox", &format!("unknown sandbox: {sandbox_id}"))
}

pub(crate) fn operator_socket_path(listen: &Path) -> PathBuf {
    let mut name = listen.file_name().unwrap_or_default().to_os_string();
    name.push(".operator");
    listen.with_file_name(name)
}

pub(crate) fn serve(listen: &Path, engine: Arc<dyn Engine>) -> Result<()> {
    let catalog = Arc::new(Catalog::load_builtin()?);
    serve_with_catalog(listen, catalog, engine)
}

pub(crate) fn serve_with_catalog(
    listen: &Path,
    catalog: Arc<Catalog>,
    engine: Arc<dyn Engine>,
) -> Result<()> {
    let operator = bind(&operator_socket_path(listen))?;
    {
        let catalog = Arc::clone(&catalog);
        let engine = Arc::clone(&engine);
        std::thread::spawn(move || accept_loop(&operator, Surface::Operator, catalog, engine));
    }
    let client = bind(listen)?;
    eprintln!(
        "eos-sandbox-gateway: serving {} (operator: {})",
        listen.display(),
        operator_socket_path(listen).display()
    );
    accept_loop(&client, Surface::Client, catalog, engine);
    Ok(())
}

fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("remove stale socket {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket dir {}", parent.display()))?;
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    Ok(listener)
}

fn accept_loop(
    listener: &UnixListener,
    surface: Surface,
    catalog: Arc<Catalog>,
    engine: Arc<dyn Engine>,
) {
    loop {
        let Ok((stream, _)) = listener.accept() else {
            continue;
        };
        let catalog = Arc::clone(&catalog);
        let engine = Arc::clone(&engine);
        std::thread::spawn(move || handle_connection(stream, surface, &catalog, &*engine));
    }
}

fn handle_connection(stream: UnixStream, surface: Surface, catalog: &Catalog, engine: &dyn Engine) {
    let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
    let read_started = Instant::now();
    let parsed = read_request_line(&stream).and_then(|line| {
        let request_bytes = line.len();
        parse_request(&line).map(|mut request| {
            request.trace.push_gateway_event(
                "gateway.transport",
                "accepted",
                json!({"surface": surface.label()}),
            );
            request.trace.push_gateway_event(
                "gateway.transport",
                "request_read",
                json!({
                    "surface": surface.label(),
                    "request_bytes": request_bytes,
                    "read_duration_us": elapsed_us(read_started),
                }),
            );
            request
        })
    });
    let (response, trace_target) = match parsed {
        Ok(request) => {
            let trace_target = request
                .sandbox_id
                .clone()
                .map(|sandbox_id| (sandbox_id, request.trace.clone()));
            (handle(catalog, engine, surface, &request), trace_target)
        }
        Err(err) => (error_response(err.kind, &err.message), None),
    };
    let mut stream = stream;
    let line = response_line(&response);
    let write_started = Instant::now();
    let write_result = stream.write_all(&line);
    if let Some((sandbox_id, trace)) = trace_target {
        match &write_result {
            Ok(()) => engine.record_trace_event(
                &sandbox_id,
                &trace,
                "gateway.transport",
                "response_written",
                json!({
                    "surface": surface.label(),
                    "response_bytes": line.len(),
                    "write_duration_us": elapsed_us(write_started),
                }),
            ),
            Err(err) => engine.record_trace_event(
                &sandbox_id,
                &trace,
                "gateway.transport",
                "write_failed",
                json!({
                    "surface": surface.label(),
                    "response_bytes": line.len(),
                    "write_duration_us": elapsed_us(write_started),
                    "error_kind": "write_failed",
                    "message": err.to_string(),
                }),
            ),
        }
    }
    if write_result.is_ok() {
        let _ = stream.flush();
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

#[derive(Debug)]
pub(crate) struct ClientRequest {
    pub(crate) op: String,
    sandbox_id: Option<String>,
    invocation_id: String,
    args: Value,
    trace: ForwardTraceContext,
}

#[derive(Debug)]
pub(crate) struct WireError {
    kind: &'static str,
    message: String,
}

impl WireError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

fn read_request_line(stream: impl Read) -> Result<Vec<u8>, WireError> {
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES as u64 + 1));
    let mut line = Vec::new();
    reader
        .read_until(b'\n', &mut line)
        .map_err(|err| WireError::new("invalid_request", format!("read request: {err}")))?;
    if line.is_empty() {
        return Err(WireError::new(
            "invalid_request",
            "connection closed before a request line",
        ));
    }
    if line.len() > MAX_REQUEST_BYTES {
        return Err(WireError::new(
            "request_too_large",
            format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
        ));
    }
    Ok(line)
}

pub(crate) fn parse_request(line: &[u8]) -> Result<ClientRequest, WireError> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|err| WireError::new("bad_json", format!("request is not valid JSON: {err}")))?;
    let Value::Object(mut object) = value else {
        return Err(WireError::new(
            "invalid_request",
            "request must be a JSON object",
        ));
    };
    let op = take_string(&mut object, "op")?;
    if op.trim().is_empty() {
        return Err(WireError::new("invalid_request", "op is required"));
    }
    let invocation_id = take_string(&mut object, "invocation_id")?;
    let sandbox_id = match object.remove("sandbox_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id),
        Some(_) => {
            return Err(WireError::new(
                "invalid_request",
                "sandbox_id must be a string",
            ))
        }
    };
    let args = object.remove("args").unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return Err(WireError::new("invalid_request", "args must be an object"));
    }
    Ok(ClientRequest {
        op,
        sandbox_id,
        trace: ForwardTraceContext::new(&invocation_id),
        invocation_id,
        args,
    })
}

fn take_string(object: &mut Map<String, Value>, field: &str) -> Result<String, WireError> {
    match object.remove(field) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(WireError::new(
            "invalid_request",
            format!("{field} is required and must be a string"),
        )),
    }
}

fn error_response(kind: &str, message: &str) -> Value {
    json!({
        "success": false,
        "warnings": [],
        "timings": {},
        "error": {
            "kind": kind,
            "message": message,
            "details": {},
        },
    })
}

impl Surface {
    const fn label(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Operator => "operator",
        }
    }
}

fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn response_line(response: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(response).unwrap_or_default();
    line.push(b'\n');
    line
}
