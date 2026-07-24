use serde_json::{json, Value};

use crate::operations::dispatch::OperationEntry;
use crate::workspace_crate::{DestroyWorkspaceResult, NetworkProfile, WorkspaceSessionId};
use crate::workspace_session::{
    CreateSessionRequest, FinalizePolicy, PublishWorkspaceSessionResult, WorkspaceSessionError,
    WorkspaceSessionHandler, WorkspaceSessionPublishDetails,
};
use crate::SandboxRuntimeOperations;
use sandbox_operation_catalog::internal::runtime::CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER;
use sandbox_operation_catalog::runtime::{
    CREATE_WORKSPACE_SESSION_SPEC, DESTROY_WORKSPACE_SESSION_SPEC, PUBLISH_WORKSPACE_SESSION_SPEC,
};
use sandbox_operation_contract::OperationScopeKind;
use sandbox_operation_contract::{OperationRequest, OperationResponse};

const CREATE_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &CREATE_WORKSPACE_SESSION_SPEC,
    dispatch_create_workspace_session,
);
const DESTROY_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &DESTROY_WORKSPACE_SESSION_SPEC,
    dispatch_destroy_workspace_session,
);
const PUBLISH_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &PUBLISH_WORKSPACE_SESSION_SPEC,
    dispatch_publish_workspace_session,
);
const CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER_ENTRY: OperationEntry = OperationEntry {
    scope_kind: OperationScopeKind::Sandbox,
    name: CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER,
    spec: None,
    dispatch: dispatch_create_workspace_session_legacy_scratch_adapter,
};

const PUBLIC_OPERATIONS: &[OperationEntry] = &[
    CREATE_WORKSPACE_SESSION_ENTRY,
    PUBLISH_WORKSPACE_SESSION_ENTRY,
    DESTROY_WORKSPACE_SESSION_ENTRY,
];
const INTERNAL_OPERATIONS: &[OperationEntry] =
    &[CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER_ENTRY];

pub(crate) const fn public_operation_entries() -> &'static [OperationEntry] {
    PUBLIC_OPERATIONS
}

pub(crate) const fn internal_operation_entries() -> &'static [OperationEntry] {
    INTERNAL_OPERATIONS
}

fn dispatch_create_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
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

fn dispatch_create_workspace_session_legacy_scratch_adapter(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let network = match parse_workspace_profile(request) {
        Ok(network) => network,
        Err(response) => return response,
    };
    workspace_session_handler_response(
        operations
            .workspace_session
            .create_workspace_session_legacy_scratch_adapter(CreateSessionRequest {
                network,
                finalize_policy: FinalizePolicy::NoOp,
            }),
    )
}

fn dispatch_destroy_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let input = match parse_workspace_session_disposition(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    match operations
        .workspace_session
        .guarded_destroy(input.workspace_session_id, input.grace_s)
    {
        Ok(result) => OperationResponse::ok(destroy_workspace_session_value(result)),
        Err(error) => workspace_session_error_response(error),
    }
}

fn dispatch_publish_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let input = match parse_workspace_session_disposition(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    match operations
        .workspace_session
        .publish_workspace_session(input.workspace_session_id, input.grace_s)
    {
        Ok(result) => OperationResponse::ok(publish_workspace_session_value(result)),
        Err(error) => workspace_session_error_response(error),
    }
}

fn parse_workspace_profile(
    request: &OperationRequest,
) -> Result<NetworkProfile, OperationResponse> {
    match request.optional_string("network_profile")? {
        None => Ok(NetworkProfile::Shared),
        Some(value) if value == NetworkProfile::Shared.as_str() => Ok(NetworkProfile::Shared),
        Some(value) if value == NetworkProfile::Isolated.as_str() => Ok(NetworkProfile::Isolated),
        Some(_) => {
            Err(request.invalid_argument("network_profile must be one of shared or isolated"))
        }
    }
}

fn parse_workspace_session_disposition(
    request: &OperationRequest,
) -> Result<WorkspaceSessionDispositionInput, OperationResponse> {
    let workspace_session_id = WorkspaceSessionId(request.required_string("workspace_session_id")?);
    let grace_s = request.optional_f64("grace_s")?;
    if matches!(grace_s, Some(value) if value < 0.0) {
        return Err(request.invalid_argument("grace_s must be non-negative"));
    }
    Ok(WorkspaceSessionDispositionInput {
        workspace_session_id,
        grace_s,
    })
}

struct WorkspaceSessionDispositionInput {
    workspace_session_id: WorkspaceSessionId,
    grace_s: Option<f64>,
}

fn workspace_session_handler_response(
    result: Result<WorkspaceSessionHandler, WorkspaceSessionError>,
) -> OperationResponse {
    match result {
        Ok(handler) => OperationResponse::ok(create_workspace_session_value(handler)),
        Err(error) => workspace_session_error_response(error),
    }
}

fn workspace_session_error_response(error: WorkspaceSessionError) -> OperationResponse {
    match error {
        WorkspaceSessionError::ActiveCommands {
            workspace_session_id,
            active_command_session_ids,
        } => OperationResponse::fault_with_details(
            "operation_failed",
            "workspace session has active command sessions",
            json!({
                "workspace_session_id": workspace_session_id.0,
                "active_command_session_ids": active_command_session_ids
                    .iter()
                    .map(|command_session_id| command_session_id.0.as_str())
                    .collect::<Vec<_>>(),
            }),
        ),
        WorkspaceSessionError::NotFound {
            workspace_session_id,
        } => OperationResponse::fault_with_details(
            "operation_failed",
            format!("workspace session not found: {workspace_session_id:?}"),
            json!({ "workspace_session_id": workspace_session_id.0 }),
        ),
        WorkspaceSessionError::PublishRetained {
            workspace_session_id,
            stage,
            publish_rejection,
            ..
        } => {
            let mut details = json!({
                "workspace_session_id": workspace_session_id.0,
                "stage": stage.as_str(),
                "session_retained": true,
            });
            if let Some(rejection) = publish_rejection {
                details["publish_rejection"] =
                    super::command_operations::publish_reject_value(&rejection);
            }
            OperationResponse::fault_with_details(
                "operation_failed",
                "workspace session publish was rejected",
                details,
            )
        }
        WorkspaceSessionError::PublishedButNotClosed {
            workspace_session_id,
            publish,
            ..
        } => OperationResponse::fault_with_details(
            "operation_failed",
            "workspace session published but could not be closed",
            json!({
                "workspace_session_id": workspace_session_id.0,
                "stage": "destroy",
                "publish_completed": true,
                "layer_committed": !publish.no_op,
                "publish": workspace_session_publish_value(&publish),
                "destroyed": false,
                "session_state": "finalize_failed",
                "recovery_operation": "destroy_workspace_session",
            }),
        ),
        error => {
            OperationResponse::fault_with_details("operation_failed", error.to_string(), json!({}))
        }
    }
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

fn publish_workspace_session_value(result: PublishWorkspaceSessionResult) -> Value {
    json!({
        "workspace_session_id": result.workspace_session_id.0,
        "publish": workspace_session_publish_value(&result.publish),
        "destroyed": true,
        "evicted_upperdir_bytes": result.evicted_upperdir_bytes,
    })
}

fn workspace_session_publish_value(publish: &WorkspaceSessionPublishDetails) -> Value {
    json!({
        "no_op": publish.no_op,
        "revision": {
            "manifest_version": publish.revision.manifest_version,
            "root_hash": publish.revision.root_hash,
            "layer_count": publish.revision.layer_count,
        },
        "route_summary": {
            "source_count": publish.route_summary.source_count,
            "ignored_count": publish.route_summary.ignored_count,
        },
    })
}
