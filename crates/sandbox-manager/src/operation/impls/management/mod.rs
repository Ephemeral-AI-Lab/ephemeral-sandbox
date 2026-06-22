mod create_sandbox;
mod destroy_sandbox;
mod inspect_sandbox;
mod list_sandboxes;

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::operation::dispatch::ManagerOperationEntry;
use crate::{ManagerError, SandboxDaemonEndpoint, SandboxId, SandboxRecord};
use sandbox_protocol::{CliOperationSpec, OperationFamilySpec};

pub(crate) const MANAGEMENT_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "management",
    title: "Management",
    summary: "Create, destroy, list, and inspect sandbox records.",
    description: "Create, destroy, list, and inspect sandbox records. Daemons are managed as part of sandbox lifecycle behavior, not as standalone manager operations.",
};

const FAMILIES: &[&OperationFamilySpec] = &[&MANAGEMENT_FAMILY];

const SPECS: &[&CliOperationSpec] = &[
    &create_sandbox::SPEC,
    &destroy_sandbox::SPEC,
    &list_sandboxes::SPEC,
    &inspect_sandbox::SPEC,
];

pub(crate) const OPERATIONS: &[ManagerOperationEntry] = &[
    ManagerOperationEntry::new(&create_sandbox::SPEC, create_sandbox::dispatch),
    ManagerOperationEntry::new(&destroy_sandbox::SPEC, destroy_sandbox::dispatch),
    ManagerOperationEntry::new(&list_sandboxes::SPEC, list_sandboxes::dispatch),
    ManagerOperationEntry::new(&inspect_sandbox::SPEC, inspect_sandbox::dispatch),
];

pub(crate) const fn operation_families() -> &'static [&'static OperationFamilySpec] {
    FAMILIES
}

pub(crate) const fn operation_specs() -> &'static [&'static CliOperationSpec] {
    SPECS
}

pub(crate) const fn operation_entries() -> &'static [ManagerOperationEntry] {
    OPERATIONS
}

fn sandbox_id(
    request: &sandbox_protocol::Request,
) -> Result<SandboxId, sandbox_protocol::Response> {
    request
        .required_string("sandbox_id")
        .and_then(|value| SandboxId::new(value).map_err(ManagerError::into_response))
}

fn workspace_root(
    request: &sandbox_protocol::Request,
) -> Result<PathBuf, sandbox_protocol::Response> {
    let raw = request.required_string("workspace_root")?;
    let path = PathBuf::from(&raw);
    if !path.is_absolute() {
        return Err(ManagerError::InvalidWorkspaceRoot { value: raw }.into_response());
    }
    Ok(path)
}

fn image(request: &sandbox_protocol::Request) -> Result<String, sandbox_protocol::Response> {
    let image = request.required_string("image")?;
    if image.trim().is_empty() {
        return Err(ManagerError::InvalidImage { value: image }.into_response());
    }
    Ok(image)
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
        "socket_path": endpoint.socket_path.to_string_lossy(),
        "auth_token_configured": endpoint.auth_token.as_ref().is_some_and(|token| !token.is_empty()),
    })
}
