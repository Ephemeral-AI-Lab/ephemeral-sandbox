use crate::{CreateSandboxRequest, SandboxState};

use super::{image, record_value, workspace_root};
use sandbox_protocol::{ArgCliSpec, ArgKind, ArgSpec, CliOperationSpec, CliSpec};

pub(crate) const SPEC: CliOperationSpec = CliOperationSpec {
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

pub(crate) fn dispatch(
    services: &crate::operation::ManagerServices,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    let image = match image(request) {
        Ok(image) => image,
        Err(response) => return response,
    };
    let workspace_root = match workspace_root(request) {
        Ok(workspace_root) => workspace_root,
        Err(response) => return response,
    };
    let create_request = CreateSandboxRequest {
        image,
        workspace_root: workspace_root.clone(),
    };
    match services.runtime.create_sandbox(&create_request) {
        Ok(created) => {
            let id = created.id;
            if let Err(error) = services.store.create(id.clone(), workspace_root.clone()) {
                return error.into_response();
            }
            let record = match services.store.transition_state(
                &id,
                SandboxState::Creating,
                SandboxState::Ready,
            ) {
                Ok(record) => record,
                Err(error) => return error.into_response(),
            };
            if let Err(error) = services.daemon_installer.install_daemon(&record) {
                return error.into_response();
            }
            let endpoint = match services.daemon_installer.start_daemon(&record) {
                Ok(endpoint) => endpoint,
                Err(error) => return error.into_response(),
            };
            if let Err(error) = services.daemon_installer.check_daemon(&endpoint) {
                return error.into_response();
            }
            match services.store.update_endpoint(&id, Some(endpoint)) {
                Ok(record) => sandbox_protocol::Response::ok(record_value(record)),
                Err(error) => error.into_response(),
            }
        }
        Err(error) => error.into_response(),
    }
}
