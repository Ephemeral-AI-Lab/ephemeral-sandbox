use crate::{ManagerError, SandboxState};

use super::{record_value, sandbox_id};
use sandbox_protocol::{ArgCliSpec, ArgKind, ArgSpec, CliOperationSpec, CliSpec};

pub(crate) const SPEC: CliOperationSpec = CliOperationSpec {
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

pub(crate) fn dispatch(
    services: &crate::operation::ManagerServices,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    let id = match sandbox_id(request) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let current = match services.store.inspect(&id) {
        Ok(record) => record,
        Err(error) => return error.into_response(),
    };
    if matches!(
        current.state,
        SandboxState::Creating | SandboxState::Stopping
    ) {
        return ManagerError::InvalidStateTransition {
            id,
            from: current.state,
            to: SandboxState::Stopping,
        }
        .into_response();
    }
    let stopping =
        match services
            .store
            .transition_state(&current.id, current.state, SandboxState::Stopping)
        {
            Ok(record) => record,
            Err(error) => return error.into_response(),
        };
    if stopping.daemon.is_some() {
        if let Err(error) = services.daemon_installer.stop_daemon(&stopping) {
            return error.into_response();
        }
    }
    match services.runtime.destroy_sandbox(&stopping) {
        Ok(()) => {
            if let Err(error) = services
                .store
                .set_state(&stopping.id, SandboxState::Stopped)
            {
                return error.into_response();
            }
            match services.store.remove(&stopping.id) {
                Ok(record) => sandbox_protocol::Response::ok(record_value(record)),
                Err(error) => error.into_response(),
            }
        }
        Err(error) => {
            let _ = services.store.set_state(&stopping.id, SandboxState::Failed);
            error.into_response()
        }
    }
}
