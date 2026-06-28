use std::path::PathBuf;

use sandbox_protocol::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationFamilySpec, CliOperationSpec, CliSpec, Request,
    Response,
};
use serde_json::{json, Value};

use crate::operation::dispatch::ManagerOperationEntry;
use crate::operation::management::{
    create_sandbox, destroy_sandbox, get_observability_tree, inspect_sandbox, list_sandboxes,
    CreateSandboxInput, TreeOptions,
};
use crate::operation::ManagerServices;
use crate::{ManagerError, ProgressSink, SandboxDaemonEndpoint, SandboxId, SandboxRecord};

pub(crate) const MANAGEMENT_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "management",
    title: "Management",
    summary: "Create, destroy, list, and inspect sandbox records.",
    description: "Create, destroy, list, and inspect sandbox records. Daemons are managed as part of sandbox lifecycle behavior, not as standalone manager operations.",
};

const CREATE_SANDBOX_SPEC: CliOperationSpec = CliOperationSpec {
    name: "create_sandbox",
    family: "management",
    summary: "Create a host-side sandbox record and runtime sandbox.",
    description:
        "Create a host-side sandbox record, create the runtime sandbox, and start its daemon.",
    args: CREATE_SANDBOX_ARGS,
    cli: Some(CREATE_SANDBOX_CLI),
    related: &["list_sandboxes", "inspect_sandbox", "destroy_sandbox"],
};

const CREATE_SANDBOX_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "image",
        ArgKind::String,
        "Container image used to create the sandbox.",
        Some(ArgCliSpec {
            flag: Some("--image"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "workspace_root",
        ArgKind::Path,
        "Absolute workspace root mounted inside this sandbox.",
        Some(ArgCliSpec {
            flag: Some("--workspace-root"),
            positional: None,
        }),
    ),
];

const CREATE_SANDBOX_CLI: CliSpec = CliSpec {
    path: &["manager", "create_sandbox"],
    usage: "sandbox-cli manager create_sandbox --image IMAGE --workspace-root PATH",
    examples: &[
        "sandbox-cli manager create_sandbox --image ubuntu:24.04 --workspace-root /testbed",
    ],
};

const DESTROY_SANDBOX_SPEC: CliOperationSpec = CliOperationSpec {
    name: "destroy_sandbox",
    family: "management",
    summary: "Destroy a host-side sandbox and remove it from the registry.",
    description: "Stop the sandbox daemon, destroy the runtime sandbox, and remove the host-side sandbox record.",
    args: DESTROY_SANDBOX_ARGS,
    cli: Some(DESTROY_SANDBOX_CLI),
    related: &["list_sandboxes", "inspect_sandbox"],
};

const DESTROY_SANDBOX_ARGS: &[ArgSpec] = &[ArgSpec::required(
    "sandbox_id",
    ArgKind::String,
    "Sandbox id.",
    Some(ArgCliSpec {
        flag: Some("--sandbox-id"),
        positional: None,
    }),
)];

const DESTROY_SANDBOX_CLI: CliSpec = CliSpec {
    path: &["manager", "destroy_sandbox"],
    usage: "sandbox-cli manager destroy_sandbox --sandbox-id ID",
    examples: &["sandbox-cli manager destroy_sandbox --sandbox-id sbox-1"],
};

const GET_OBSERVABILITY_TREE_SPEC: CliOperationSpec = CliOperationSpec {
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
    usage: "sandbox-cli manager get_observability_tree [--sandbox-id ID] [--resource-window-ms MS]",
    examples: &[
        "sandbox-cli manager get_observability_tree",
        "sandbox-cli manager get_observability_tree --sandbox-id sbox-1",
        "sandbox-cli manager get_observability_tree --resource-window-ms 60000",
    ],
};

const LIST_SANDBOXES_SPEC: CliOperationSpec = CliOperationSpec {
    name: "list_sandboxes",
    family: "management",
    summary: "List sandbox records known to the manager.",
    description: "List sandbox records known to the manager, including lifecycle state and configured daemon endpoint metadata.",
    args: &[],
    cli: Some(LIST_SANDBOXES_CLI),
    related: &["inspect_sandbox", "create_sandbox"],
};

const LIST_SANDBOXES_CLI: CliSpec = CliSpec {
    path: &["manager", "list_sandboxes"],
    usage: "sandbox-cli manager list_sandboxes",
    examples: &["sandbox-cli manager list_sandboxes"],
};

const INSPECT_SANDBOX_SPEC: CliOperationSpec = CliOperationSpec {
    name: "inspect_sandbox",
    family: "management",
    summary: "Inspect one sandbox record.",
    description: "Inspect one sandbox record, including lifecycle state, workspace root, and configured daemon endpoint metadata.",
    args: INSPECT_SANDBOX_ARGS,
    cli: Some(INSPECT_SANDBOX_CLI),
    related: &["list_sandboxes"],
};

const INSPECT_SANDBOX_ARGS: &[ArgSpec] = &[ArgSpec::required(
    "sandbox_id",
    ArgKind::String,
    "Sandbox id.",
    Some(ArgCliSpec {
        flag: Some("--sandbox-id"),
        positional: None,
    }),
)];

const INSPECT_SANDBOX_CLI: CliSpec = CliSpec {
    path: &["manager", "inspect_sandbox"],
    usage: "sandbox-cli manager inspect_sandbox --sandbox-id ID",
    examples: &["sandbox-cli manager inspect_sandbox --sandbox-id sbox-1"],
};

const FAMILIES: &[&CliOperationFamilySpec] = &[&MANAGEMENT_FAMILY];

const SPECS: &[&CliOperationSpec] = &[
    &CREATE_SANDBOX_SPEC,
    &DESTROY_SANDBOX_SPEC,
    &GET_OBSERVABILITY_TREE_SPEC,
    &LIST_SANDBOXES_SPEC,
    &INSPECT_SANDBOX_SPEC,
];

const OPERATIONS: &[ManagerOperationEntry] = &[
    ManagerOperationEntry::new(&CREATE_SANDBOX_SPEC, dispatch_create_sandbox),
    ManagerOperationEntry::new(&DESTROY_SANDBOX_SPEC, dispatch_destroy_sandbox),
    ManagerOperationEntry::new(
        &GET_OBSERVABILITY_TREE_SPEC,
        dispatch_get_observability_tree,
    ),
    ManagerOperationEntry::new(&LIST_SANDBOXES_SPEC, dispatch_list_sandboxes),
    ManagerOperationEntry::new(&INSPECT_SANDBOX_SPEC, dispatch_inspect_sandbox),
];

pub(crate) const fn cli_operation_families() -> &'static [&'static CliOperationFamilySpec] {
    FAMILIES
}

pub(crate) const fn cli_operation_specs() -> &'static [&'static CliOperationSpec] {
    SPECS
}

pub(crate) fn operation_entries() -> &'static [ManagerOperationEntry] {
    OPERATIONS
}

fn dispatch_create_sandbox(services: &ManagerServices, request: &Request) -> Response {
    dispatch_create_sandbox_with_progress(services, request, &ProgressSink::noop())
}

pub(crate) fn dispatch_create_sandbox_with_progress(
    services: &ManagerServices,
    request: &Request,
    progress: &ProgressSink,
) -> Response {
    let image = match image(request) {
        Ok(image) => image,
        Err(response) => return response,
    };
    let workspace_root = match workspace_root(request) {
        Ok(workspace_root) => workspace_root,
        Err(response) => return response,
    };
    match create_sandbox(
        services,
        CreateSandboxInput {
            image,
            workspace_root,
        },
        progress,
    ) {
        Ok(record) => Response::ok(record_value(record)),
        Err(error) => error.into_response(),
    }
}

fn dispatch_destroy_sandbox(services: &ManagerServices, request: &Request) -> Response {
    let id = match sandbox_id(request) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match destroy_sandbox(services, id) {
        Ok(record) => Response::ok(record_value(record)),
        Err(error) => error.into_response(),
    }
}

fn dispatch_inspect_sandbox(services: &ManagerServices, request: &Request) -> Response {
    let id = match sandbox_id(request) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match inspect_sandbox(services, &id) {
        Ok(record) => Response::ok(record_value(record)),
        Err(error) => error.into_response(),
    }
}

fn dispatch_list_sandboxes(services: &ManagerServices, _request: &Request) -> Response {
    match list_sandboxes(services) {
        Ok(records) => Response::ok(records_value(records)),
        Err(error) => error.into_response(),
    }
}

fn dispatch_get_observability_tree(services: &ManagerServices, request: &Request) -> Response {
    let options = match tree_options(request) {
        Ok(options) => options,
        Err(response) => return response,
    };
    match get_observability_tree(services, options, &request.request_id) {
        Ok(sandboxes) => Response::ok(json!({ "sandboxes": sandboxes })),
        Err(error) => error.into_response(),
    }
}

fn sandbox_id(request: &Request) -> Result<SandboxId, Response> {
    request
        .required_string("sandbox_id")
        .and_then(|value| SandboxId::new(value).map_err(ManagerError::into_response))
}

fn workspace_root(request: &Request) -> Result<PathBuf, Response> {
    let raw = request.required_string("workspace_root")?;
    let path = PathBuf::from(&raw);
    if !path.is_absolute() {
        return Err(ManagerError::InvalidWorkspaceRoot { value: raw }.into_response());
    }
    Ok(path)
}

fn image(request: &Request) -> Result<String, Response> {
    let image = request.required_string("image")?;
    if image.trim().is_empty() {
        return Err(ManagerError::InvalidImage { value: image }.into_response());
    }
    Ok(image)
}

fn tree_options(request: &Request) -> Result<TreeOptions, Response> {
    let sandbox_id = request
        .optional_string("sandbox_id")?
        .map(SandboxId::new)
        .transpose()
        .map_err(ManagerError::into_response)?;
    Ok(TreeOptions {
        sandbox_id,
        resource_window_ms: request.optional_u64("resource_window_ms")?,
    })
}

fn records_value(records: Vec<SandboxRecord>) -> Value {
    json!({
        "sandboxes": records.into_iter().map(record_value).collect::<Vec<_>>(),
    })
}

fn record_value(record: SandboxRecord) -> Value {
    json!({
        "id": record.id.as_str(),
        "workspace_root": record.workspace_root.to_string_lossy(),
        "state": record.state.as_str(),
        "daemon": record.daemon.map(endpoint_value),
    })
}

fn endpoint_value(endpoint: SandboxDaemonEndpoint) -> Value {
    json!({
        "host": endpoint.host,
        "port": endpoint.port,
    })
}
