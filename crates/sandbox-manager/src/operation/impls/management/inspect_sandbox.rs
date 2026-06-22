use super::{record_value, sandbox_id};
use sandbox_protocol::{ArgCliSpec, ArgKind, ArgSpec, CliOperationSpec, CliSpec};

pub(crate) const SPEC: CliOperationSpec = CliOperationSpec {
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

pub(crate) fn dispatch(
    services: &crate::operation::ManagerServices,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    let id = match sandbox_id(request) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match services.store.inspect(&id) {
        Ok(record) => sandbox_protocol::Response::ok(record_value(record)),
        Err(error) => error.into_response(),
    }
}
