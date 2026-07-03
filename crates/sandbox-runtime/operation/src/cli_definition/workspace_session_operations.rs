use serde_json::{json, Value};

use crate::operation::OperationEntry;
use crate::workspace_crate::{DestroyWorkspaceResult, NetworkProfile, WorkspaceSessionId};
use crate::workspace_session::{
    CreateSessionRequest, FinalizePolicy, WorkspaceSessionError, WorkspaceSessionHandler,
};
use crate::SandboxRuntimeOperations;
use sandbox_protocol::{Request, Response};
use sandbox_runtime_operations::{CREATE_WORKSPACE_SESSION_SPEC, DESTROY_WORKSPACE_SESSION_SPEC};

const CREATE_WORKSPACE_SESSION: OperationEntry = OperationEntry::cli(
    &CREATE_WORKSPACE_SESSION_SPEC,
    dispatch_create_workspace_session,
);
const DESTROY_WORKSPACE_SESSION: OperationEntry = OperationEntry::cli(
    &DESTROY_WORKSPACE_SESSION_SPEC,
    dispatch_destroy_workspace_session,
);

const OPERATIONS: &[OperationEntry] = &[CREATE_WORKSPACE_SESSION, DESTROY_WORKSPACE_SESSION];

pub(crate) const fn operation_entries() -> &'static [OperationEntry] {
    OPERATIONS
}

fn dispatch_create_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &Request,
) -> Response {
    let network = match parse_workspace_profile(request) {
        Ok(network) => network,
        Err(response) => return response,
    };
    workspace_session_handler_response(operations.workspace_session.create_workspace_session(
        CreateSessionRequest {
            network,
            finalize_policy: FinalizePolicy::NoOp,
        },
    ))
}

fn dispatch_destroy_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &Request,
) -> Response {
    let input = match parse_destroy_workspace_session(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    match operations
        .workspace_session
        .guarded_destroy(input.workspace_session_id, input.grace_s)
    {
        Ok(result) => Response::ok(destroy_workspace_session_value(result)),
        Err(WorkspaceSessionError::ActiveCommands {
            active_command_session_ids,
            ..
        }) => active_command_rejection(&active_command_session_ids),
        Err(error) => workspace_session_error_response(error),
    }
}

fn parse_workspace_profile(request: &Request) -> Result<NetworkProfile, Response> {
    match request.optional_string("network_profile")? {
        None => Ok(NetworkProfile::Shared),
        Some(value) if value == NetworkProfile::Shared.as_str() => Ok(NetworkProfile::Shared),
        Some(value) if value == NetworkProfile::Isolated.as_str() => Ok(NetworkProfile::Isolated),
        Some(_) => {
            Err(request.invalid_argument("network_profile must be one of shared or isolated"))
        }
    }
}

fn parse_destroy_workspace_session(
    request: &Request,
) -> Result<DestroyWorkspaceSessionInput, Response> {
    let workspace_session_id = WorkspaceSessionId(request.required_string("workspace_session_id")?);
    let grace_s = request.optional_f64("grace_s")?;
    if matches!(grace_s, Some(value) if value < 0.0) {
        return Err(request.invalid_argument("grace_s must be non-negative"));
    }
    Ok(DestroyWorkspaceSessionInput {
        workspace_session_id,
        grace_s,
    })
}

struct DestroyWorkspaceSessionInput {
    workspace_session_id: WorkspaceSessionId,
    grace_s: Option<f64>,
}

fn workspace_session_handler_response(
    result: Result<WorkspaceSessionHandler, WorkspaceSessionError>,
) -> Response {
    match result {
        Ok(handler) => Response::ok(create_workspace_session_value(handler)),
        Err(error) => workspace_session_error_response(error),
    }
}

fn workspace_session_error_response(error: WorkspaceSessionError) -> Response {
    Response::fault_with_details("operation_failed", error.to_string(), json!({}))
}

fn active_command_rejection(
    active_command_session_ids: &[sandbox_runtime_namespace_execution::NamespaceExecutionId],
) -> Response {
    Response::fault_with_details(
        "operation_failed",
        "workspace session has active command sessions",
        json!({
            "active_command_session_ids": active_command_session_ids
                .iter()
                .map(|command_session_id| command_session_id.0.as_str())
                .collect::<Vec<_>>(),
        }),
    )
}

fn create_workspace_session_value(handler: WorkspaceSessionHandler) -> Value {
    json!({
        "workspace_session_id": handler.workspace_session_id.0,
        "network_profile": handler.handle.network.as_str(),
        "finalize_policy": FinalizePolicy::NoOp.as_str(),
    })
}

fn destroy_workspace_session_value(result: DestroyWorkspaceResult) -> Value {
    json!({
        "workspace_session_id": result.workspace_session_id.0,
        "destroyed": true,
        "evicted_upperdir_bytes": result.evicted_upperdir_bytes,
    })
}
