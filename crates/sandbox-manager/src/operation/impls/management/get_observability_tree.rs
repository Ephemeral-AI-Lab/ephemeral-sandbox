use std::sync::Arc;
use std::time::Duration;

use crate::{
    ManagerError, SandboxDaemonClient, SandboxDaemonEndpoint, SandboxId, SandboxRecord,
    SandboxState,
};
use sandbox_protocol::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationScope, CliOperationSpec, CliSpec, Request, Response,
};
use serde_json::{json, Map, Value};

const PRIVATE_DAEMON_OBSERVABILITY_SNAPSHOT_OP: &str = "get_observability_snapshot";
const MAX_CONCURRENT_DAEMON_SNAPSHOT_REQUESTS: usize = 8;
const DEFAULT_DAEMON_SNAPSHOT_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_TRACE_LIMIT: usize = 20;
const MAX_TRACE_LIMIT: usize = 100;
const MAX_RESOURCE_WINDOW_MS: u64 = 600_000;
const MAX_NODE_ERROR_BYTES: usize = 4_096;

pub(crate) const SPEC: CliOperationSpec = CliOperationSpec {
    name: "get_observability_tree",
    family: "management",
    summary: "Aggregate daemon observability snapshots for manager-known sandboxes.",
    description: "Aggregate daemon-local observability snapshots for ready manager-known sandboxes without reading daemon storage from the manager.",
    args: GET_OBSERVABILITY_TREE_ARGS,
    cli: Some(GET_OBSERVABILITY_TREE_CLI),
    related: &["list_sandboxes", "inspect_sandbox"],
};

const GET_OBSERVABILITY_TREE_ARGS: &[ArgSpec] = &[
    ArgSpec::optional(
        "sandbox_id",
        ArgKind::String,
        "Optional manager sandbox id. When omitted, all ready sandboxes with daemon endpoints are queried.",
        None,
        Some(ArgCliSpec {
            flag: Some("--sandbox-id"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "include_recent_traces",
        ArgKind::Integer,
        "Set to 1 to include bounded recent trace summaries.",
        Some("0"),
        Some(ArgCliSpec {
            flag: Some("--include-recent-traces"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "trace_limit",
        ArgKind::Integer,
        "Maximum recent trace summaries per daemon before manager and daemon caps.",
        Some("20"),
        Some(ArgCliSpec {
            flag: Some("--trace-limit"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "resource_window_ms",
        ArgKind::Integer,
        "Optional bounded resource history window in milliseconds.",
        None,
        Some(ArgCliSpec {
            flag: Some("--resource-window-ms"),
            positional: None,
        }),
    ),
];

const GET_OBSERVABILITY_TREE_CLI: CliSpec = CliSpec {
    path: &["manager", "get_observability_tree"],
    usage: "sandbox-cli manager get_observability_tree [--sandbox-id ID] [--include-recent-traces 1] [--trace-limit N] [--resource-window-ms MS]",
    examples: &[
        "sandbox-cli manager get_observability_tree",
        "sandbox-cli manager get_observability_tree --sandbox-id sbox-1",
        "sandbox-cli manager get_observability_tree --include-recent-traces 1 --trace-limit 20",
        "sandbox-cli manager get_observability_tree --resource-window-ms 60000",
    ],
};

#[derive(Clone, Debug)]
struct TreeOptions {
    sandbox_id: Option<SandboxId>,
    include_recent_traces: bool,
    trace_limit: usize,
    resource_window_ms: Option<u64>,
}

pub(crate) fn dispatch(
    services: &crate::operation::ManagerServices,
    request: &Request,
) -> Response {
    let options = match tree_options(request) {
        Ok(options) => options,
        Err(response) => return response,
    };
    let records = match selected_records(services, options.sandbox_id.as_ref()) {
        Ok(records) => records,
        Err(error) => return error.into_response(),
    };
    let sandboxes = aggregate_records(
        records,
        options,
        Arc::clone(&services.daemon_client),
        &request.request_id,
    );
    Response::ok(json!({ "sandboxes": sandboxes }))
}

fn tree_options(request: &Request) -> Result<TreeOptions, Response> {
    let sandbox_id = request
        .optional_string("sandbox_id")?
        .map(SandboxId::new)
        .transpose()
        .map_err(ManagerError::into_response)?;
    Ok(TreeOptions {
        sandbox_id,
        include_recent_traces: request.optional_u64("include_recent_traces")?.unwrap_or(0) != 0,
        trace_limit: request
            .optional_usize("trace_limit")?
            .unwrap_or(DEFAULT_TRACE_LIMIT)
            .min(MAX_TRACE_LIMIT),
        resource_window_ms: request
            .optional_u64("resource_window_ms")?
            .map(|window_ms| window_ms.min(MAX_RESOURCE_WINDOW_MS)),
    })
}

fn selected_records(
    services: &crate::operation::ManagerServices,
    sandbox_id: Option<&SandboxId>,
) -> Result<Vec<SandboxRecord>, ManagerError> {
    match sandbox_id {
        Some(sandbox_id) => services
            .store
            .inspect(sandbox_id)
            .map(|record| vec![record]),
        None => Ok(services
            .store
            .list()?
            .into_iter()
            .filter(|record| record.state == SandboxState::Ready && record.daemon.is_some())
            .collect()),
    }
}

fn aggregate_records(
    records: Vec<SandboxRecord>,
    options: TreeOptions,
    daemon_client: Arc<dyn SandboxDaemonClient>,
    request_id: &str,
) -> Vec<Value> {
    let mut nodes = Vec::with_capacity(records.len());
    for chunk in records.chunks(MAX_CONCURRENT_DAEMON_SNAPSHOT_REQUESTS) {
        std::thread::scope(|scope| {
            let handles = chunk
                .iter()
                .cloned()
                .map(|record| {
                    let panic_record = record.clone();
                    let worker_options = options.clone();
                    let worker_client = Arc::clone(&daemon_client);
                    let worker_request_id = request_id.to_owned();
                    let handle = scope.spawn(move || {
                        sandbox_node(record, &worker_options, worker_client, &worker_request_id)
                    });
                    (panic_record, handle)
                })
                .collect::<Vec<_>>();
            for (record, handle) in handles {
                match handle.join() {
                    Ok(node) => nodes.push(node),
                    Err(_) => nodes.push(unavailable_node(
                        &record,
                        record.daemon.as_ref(),
                        "manager observability aggregation worker panicked",
                    )),
                }
            }
        });
    }
    nodes
}

fn sandbox_node(
    record: SandboxRecord,
    options: &TreeOptions,
    daemon_client: Arc<dyn SandboxDaemonClient>,
    request_id: &str,
) -> Value {
    if record.state != SandboxState::Ready {
        return unavailable_node(
            &record,
            record.daemon.as_ref(),
            format!("sandbox lifecycle state is {}", record.state),
        );
    }
    let Some(endpoint) = record.daemon.clone() else {
        return unavailable_node(&record, None, "sandbox daemon endpoint is unavailable");
    };
    let request = private_snapshot_request(&record, options, request_id);
    match daemon_client.invoke_with_timeout(
        &endpoint,
        request,
        Duration::from_millis(DEFAULT_DAEMON_SNAPSHOT_TIMEOUT_MS),
    ) {
        Ok(response) => node_from_daemon_response(&record, &endpoint, response.into_json_value()),
        Err(error) => unavailable_node(&record, Some(&endpoint), error.to_string()),
    }
}

fn private_snapshot_request(
    record: &SandboxRecord,
    options: &TreeOptions,
    request_id: &str,
) -> Request {
    Request::new(
        PRIVATE_DAEMON_OBSERVABILITY_SNAPSHOT_OP,
        format!(
            "{}:{}:observability_snapshot",
            request_id,
            record.id.as_str()
        ),
        CliOperationScope::sandbox(record.id.as_str()),
        json!({
            "include_recent_traces": options.include_recent_traces,
            "trace_limit": options.trace_limit,
            "resource_window_ms": options.resource_window_ms,
        }),
    )
}

fn node_from_daemon_response(
    record: &SandboxRecord,
    endpoint: &SandboxDaemonEndpoint,
    value: Value,
) -> Value {
    if let Some(error) = value.get("error") {
        return unavailable_node(record, Some(endpoint), response_error_message(error));
    }
    let Value::Object(mut object) = value else {
        return unavailable_node(
            record,
            Some(endpoint),
            "daemon snapshot response was not an object",
        );
    };
    object.insert("sandbox_id".to_owned(), json!(record.id.as_str()));
    object.insert("lifecycle_state".to_owned(), json!(record.state.as_str()));
    normalize_availability(&mut object);
    object
        .entry("errors".to_owned())
        .or_insert_with(|| json!([]));
    object
        .entry("daemon".to_owned())
        .or_insert_with(|| daemon_value(Some(endpoint)));
    object
        .entry("resources".to_owned())
        .or_insert_with(empty_resources_value);
    object
        .entry("workspaces".to_owned())
        .or_insert_with(|| json!([]));
    object
        .entry("recent_traces".to_owned())
        .or_insert_with(|| json!([]));
    Value::Object(object)
}

fn normalize_availability(object: &mut Map<String, Value>) {
    match object.get("availability").and_then(Value::as_str) {
        Some("available" | "partial" | "unavailable") => {}
        _ => {
            object.insert("availability".to_owned(), json!("partial"));
            push_node_error(object, "daemon snapshot availability was malformed");
        }
    }
}

fn unavailable_node(
    record: &SandboxRecord,
    endpoint: Option<&SandboxDaemonEndpoint>,
    error: impl Into<String>,
) -> Value {
    json!({
        "sandbox_id": record.id.as_str(),
        "lifecycle_state": record.state.as_str(),
        "availability": "unavailable",
        "sampled_at_unix_ms": Value::Null,
        "errors": [bound_node_error(error.into())],
        "daemon": daemon_value(endpoint),
        "resources": empty_resources_value(),
        "workspaces": [],
        "recent_traces": [],
    })
}

fn daemon_value(endpoint: Option<&SandboxDaemonEndpoint>) -> Value {
    json!({
        "socket_path": endpoint.map(|endpoint| endpoint.socket_path.to_string_lossy().into_owned()),
        "pid_path": Value::Null,
        "daemon_pid": Value::Null,
        "runtime_dir": Value::Null,
    })
}

fn empty_resources_value() -> Value {
    json!({
        "latest": Value::Null,
        "history": [],
    })
}

fn response_error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("daemon returned an error response")
        .to_owned()
}

fn push_node_error(object: &mut Map<String, Value>, error: impl Into<String>) {
    let error = json!(bound_node_error(error.into()));
    match object.get_mut("errors").and_then(Value::as_array_mut) {
        Some(errors) => errors.push(error),
        None => {
            object.insert("errors".to_owned(), json!([error]));
        }
    }
}

fn bound_node_error(value: String) -> String {
    if value.len() <= MAX_NODE_ERROR_BYTES {
        return value;
    }
    let mut end = MAX_NODE_ERROR_BYTES;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_owned()
}
