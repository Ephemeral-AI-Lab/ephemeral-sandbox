use sandbox_protocol::{OperationScope, SandboxRequest};

use crate::{
    ManagerError, ManagerResult, ManagerServices, SandboxDaemonEndpoint, SandboxId, SandboxState,
};

pub(super) fn forward_sandbox_request(
    services: &ManagerServices,
    request: SandboxRequest,
) -> ManagerResult<sandbox_protocol::SandboxResponse> {
    let id = sandbox_id(&request.scope)?;
    let endpoint = daemon_endpoint(services, &id)?;
    services.daemon_client.invoke(&endpoint, request)
}

fn sandbox_id(scope: &OperationScope) -> ManagerResult<SandboxId> {
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
) -> ManagerResult<SandboxDaemonEndpoint> {
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
