use sandbox_operation_catalog::internal;
use sandbox_operation_catalog::runtime::{
    CREATE_MPLA_WORKSPACE_SESSION_SPEC, CREATE_WORKSPACE_SESSION_SPEC,
    DESTROY_WORKSPACE_SESSION_SPEC, EXEC_COMMAND_SPEC, FILE_EDIT_SPEC, FILE_WRITE_SPEC,
    MPLA_STORAGE_ADMIN_SPEC, PUBLISH_WORKSPACE_SESSION_SPEC, WRITE_STDIN_SPEC,
};
use sandbox_operation_contract::{OperationRequest, OperationResponse, OperationScope};

use crate::{ManagerError, ManagerServices, SandboxDaemonEndpoint, SandboxId, SandboxState};

pub(crate) fn forward_sandbox_request(
    services: &ManagerServices,
    request: OperationRequest,
) -> Result<OperationResponse, ManagerError> {
    let id = sandbox_id(&request.scope)?;
    let endpoint = daemon_endpoint(services, &id)?;
    let operation = request.op.clone();
    let response = services.daemon_client.invoke(&endpoint, request, None)?;
    if advances_activity_revision(&response, &operation) {
        services.store.advance_activity_revision(&id)?;
    }
    Ok(response)
}

fn advances_activity_revision(response: &OperationResponse, operation: &str) -> bool {
    if !is_mutation(operation) {
        return false;
    }
    let value = response.as_json_value();
    if value.get("error").is_none() {
        return true;
    }
    operation == PUBLISH_WORKSPACE_SESSION_SPEC.name
        && value["error"]["details"]["stage"].as_str() == Some("destroy")
        && value["error"]["details"]["publish_completed"].as_bool() == Some(true)
}

fn is_mutation(operation: &str) -> bool {
    [
        EXEC_COMMAND_SPEC.name,
        WRITE_STDIN_SPEC.name,
        FILE_WRITE_SPEC.name,
        FILE_EDIT_SPEC.name,
        CREATE_WORKSPACE_SESSION_SPEC.name,
        CREATE_MPLA_WORKSPACE_SESSION_SPEC.name,
        PUBLISH_WORKSPACE_SESSION_SPEC.name,
        DESTROY_WORKSPACE_SESSION_SPEC.name,
        MPLA_STORAGE_ADMIN_SPEC.name,
        internal::runtime::SQUASH_LAYERSTACK,
    ]
    .contains(&operation)
}

fn sandbox_id(scope: &OperationScope) -> Result<SandboxId, ManagerError> {
    match scope {
        OperationScope::Sandbox { sandbox_id } => SandboxId::new(sandbox_id.clone()),
        OperationScope::System => Err(ManagerError::InvalidSandboxId {
            value: "system".to_owned(),
        }),
    }
}

fn daemon_endpoint(
    services: &ManagerServices,
    id: &SandboxId,
) -> Result<SandboxDaemonEndpoint, ManagerError> {
    let record = services.store.inspect(id)?;
    if record.state != SandboxState::Ready {
        return Err(ManagerError::InvalidStateTransition {
            id: id.clone(),
            from: record.state,
            to: SandboxState::Ready,
        });
    }
    record
        .daemon
        .ok_or_else(|| ManagerError::DaemonUnavailable { id: id.clone() })
}
