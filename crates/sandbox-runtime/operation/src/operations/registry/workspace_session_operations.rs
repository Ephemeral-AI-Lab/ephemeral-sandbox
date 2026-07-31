use std::time::Instant;

use sandbox_runtime_mpla_poc::{OperationId, StorageAdminRequest, PREPARED_FIXTURE_PROFILE};
use serde_json::{json, Value};

use crate::operations::dispatch::OperationEntry;
use crate::workspace_crate::{DestroyWorkspaceResult, NetworkProfile, WorkspaceSessionId};
use crate::workspace_session::{
    ActivateMplaWorkspaceSessionResult, AttachMplaPreparedFixtureResult, CreateSessionRequest,
    FinalizePolicy, ForkMplaWorkspaceSessionResult, MplaLifecycleReceipt,
    PublishMplaWorkspaceSessionResult, PublishWorkspaceSessionResult,
    RollbackMplaWorkspaceSessionResult, SquashMplaBranchResult, WorkspaceSessionError,
    WorkspaceSessionHandler, WorkspaceSessionPublishDetails,
};
use crate::SandboxRuntimeOperations;
use sandbox_operation_catalog::internal::runtime::CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER;
use sandbox_operation_catalog::runtime::{
    ACTIVATE_WORKSPACE_SESSION_SPEC, ATTACH_MPLA_PREPARED_FIXTURE_SPEC,
    CREATE_MPLA_WORKSPACE_SESSION_SPEC, CREATE_WORKSPACE_SESSION_SPEC,
    DESTROY_WORKSPACE_SESSION_SPEC, FORK_WORKSPACE_SESSION_SPEC, MPLA_STORAGE_ADMIN_SPEC,
    PUBLISH_MPLA_WORKSPACE_SESSION_SPEC, PUBLISH_WORKSPACE_SESSION_SPEC,
    ROLLBACK_WORKSPACE_SESSION_SPEC, SQUASH_MPLA_BRANCH_SPEC,
};
use sandbox_operation_contract::OperationScopeKind;
use sandbox_operation_contract::{OperationRequest, OperationResponse};

const CREATE_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &CREATE_WORKSPACE_SESSION_SPEC,
    dispatch_create_workspace_session,
);
const CREATE_MPLA_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &CREATE_MPLA_WORKSPACE_SESSION_SPEC,
    dispatch_create_mpla_workspace_session,
);
const ATTACH_MPLA_PREPARED_FIXTURE_ENTRY: OperationEntry = OperationEntry::public(
    &ATTACH_MPLA_PREPARED_FIXTURE_SPEC,
    dispatch_attach_mpla_prepared_fixture,
);
const ACTIVATE_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &ACTIVATE_WORKSPACE_SESSION_SPEC,
    dispatch_activate_workspace_session,
);
const FORK_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &FORK_WORKSPACE_SESSION_SPEC,
    dispatch_fork_workspace_session,
);
const ROLLBACK_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &ROLLBACK_WORKSPACE_SESSION_SPEC,
    dispatch_rollback_workspace_session,
);
const SQUASH_MPLA_BRANCH_ENTRY: OperationEntry =
    OperationEntry::public(&SQUASH_MPLA_BRANCH_SPEC, dispatch_squash_mpla_branch);
const PUBLISH_MPLA_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &PUBLISH_MPLA_WORKSPACE_SESSION_SPEC,
    dispatch_publish_mpla_workspace_session,
);
const DESTROY_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &DESTROY_WORKSPACE_SESSION_SPEC,
    dispatch_destroy_workspace_session,
);
const PUBLISH_WORKSPACE_SESSION_ENTRY: OperationEntry = OperationEntry::public(
    &PUBLISH_WORKSPACE_SESSION_SPEC,
    dispatch_publish_workspace_session,
);
const MPLA_STORAGE_ADMIN_ENTRY: OperationEntry =
    OperationEntry::public(&MPLA_STORAGE_ADMIN_SPEC, dispatch_mpla_storage_admin);
const CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER_ENTRY: OperationEntry = OperationEntry {
    scope_kind: OperationScopeKind::Sandbox,
    name: CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER,
    spec: None,
    dispatch: dispatch_create_workspace_session_legacy_scratch_adapter,
};

const PUBLIC_OPERATIONS: &[OperationEntry] = &[
    CREATE_WORKSPACE_SESSION_ENTRY,
    CREATE_MPLA_WORKSPACE_SESSION_ENTRY,
    ATTACH_MPLA_PREPARED_FIXTURE_ENTRY,
    ACTIVATE_WORKSPACE_SESSION_ENTRY,
    FORK_WORKSPACE_SESSION_ENTRY,
    ROLLBACK_WORKSPACE_SESSION_ENTRY,
    SQUASH_MPLA_BRANCH_ENTRY,
    PUBLISH_MPLA_WORKSPACE_SESSION_ENTRY,
    PUBLISH_WORKSPACE_SESSION_ENTRY,
    DESTROY_WORKSPACE_SESSION_ENTRY,
    MPLA_STORAGE_ADMIN_ENTRY,
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

fn dispatch_create_mpla_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let run_id = match request.required_string("run_id") {
        Ok(value) if !value.trim().is_empty() => {
            match sandbox_runtime_mpla_poc::RunId::parse(value) {
                Ok(run_id) => run_id,
                Err(error) => return request.invalid_argument(format!("invalid run_id: {error}")),
            }
        }
        Ok(_) => return request.invalid_argument("run_id must not be empty"),
        Err(response) => return response,
    };
    let sandbox_id = match request.scope.sandbox_id() {
        Some(sandbox_id) => sandbox_id,
        None => {
            return request.invalid_argument("create_mpla_workspace_session requires sandbox scope")
        }
    };
    let selected_profile = match operations.workspace_session.mpla_storage_admin_profile() {
        Ok(profile) => profile,
        Err(error) => return workspace_session_error_response(error),
    };
    match operations
        .workspace_session
        .create_mpla_workspace_session(run_id, OperationId::from_string(request.request_id.clone()))
    {
        Ok(handler) => match operations
            .workspace_session
            .mpla_storage_scope(&handler.workspace_session_id, sandbox_id)
        {
            Ok(storage_admin_scope) => OperationResponse::ok(json!({
                "workspace_session_id": handler.workspace_session_id.0,
                "network_profile": handler.handle.network.as_str(),
                "finalize_policy": FinalizePolicy::NoOp.as_str(),
                "storage_admin_profile_id": selected_profile.profile_id(),
                "storage_admin_scope": storage_admin_scope,
            })),
            Err(error) => workspace_session_error_response(error),
        },
        Err(error) => workspace_session_error_response(error),
    }
}

fn dispatch_attach_mpla_prepared_fixture(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let run_id = match parse_mpla_run_id(request) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let fixture_profile = match request.required_string("fixture_profile") {
        Ok(value) if value == PREPARED_FIXTURE_PROFILE => PREPARED_FIXTURE_PROFILE,
        Ok(_) => {
            return request.invalid_argument(&format!(
                "fixture_profile must be {PREPARED_FIXTURE_PROFILE}"
            ))
        }
        Err(response) => return response,
    };
    if request.scope.sandbox_id().is_none() {
        return request.invalid_argument("attach_mpla_prepared_fixture requires sandbox scope");
    }
    match operations.workspace_session.attach_mpla_prepared_fixture(
        run_id,
        fixture_profile,
        OperationId::from_string(request.request_id.clone()),
    ) {
        Ok(result) => OperationResponse::ok(attach_mpla_prepared_fixture_value(result)),
        Err(error) => workspace_session_error_response(error),
    }
}

fn dispatch_activate_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let run_id = match parse_mpla_run_id(request) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let branch = match parse_mpla_branch(request, "branch") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let sandbox_id = match request.scope.sandbox_id() {
        Some(value) => value,
        None => {
            return request.invalid_argument("activate_workspace_session requires sandbox scope")
        }
    };
    match operations
        .workspace_session
        .activate_mpla_workspace_session(
            run_id,
            &branch,
            sandbox_id,
            OperationId::from_string(request.request_id.clone()),
        ) {
        Ok(result) => {
            let selected_profile = match operations.workspace_session.mpla_storage_admin_profile() {
                Ok(profile) => profile,
                Err(error) => return workspace_session_error_response(error),
            };
            match operations
                .workspace_session
                .mpla_storage_scope(&result.workspace_session_id, sandbox_id)
            {
                Ok(storage_admin_scope) => {
                    let mut value = activate_mpla_workspace_session_value(result);
                    if let Err(error) = attach_storage_admin_authority(
                        &mut value,
                        selected_profile.profile_id(),
                        &storage_admin_scope,
                    ) {
                        return OperationResponse::fault_with_details(
                            "operation_failed",
                            error,
                            json!({}),
                        );
                    }
                    OperationResponse::ok(value)
                }
                Err(error) => workspace_session_error_response(error),
            }
        }
        Err(error) => workspace_session_error_response(error),
    }
}

fn dispatch_fork_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let run_id = match parse_mpla_run_id(request) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let source_branch = match parse_mpla_branch(request, "source_branch") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let branch = match parse_mpla_branch(request, "branch") {
        Ok(value) => value,
        Err(response) => return response,
    };
    if request.scope.sandbox_id().is_none() {
        return request.invalid_argument("fork_workspace_session requires sandbox scope");
    }
    match operations.workspace_session.fork_mpla_workspace_session(
        run_id,
        &source_branch,
        &branch,
        OperationId::from_string(request.request_id.clone()),
    ) {
        Ok(result) => OperationResponse::ok(fork_mpla_workspace_session_value(result)),
        Err(error) => workspace_session_error_response(error),
    }
}

fn dispatch_rollback_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let run_id = match parse_mpla_run_id(request) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let branch = match parse_mpla_branch(request, "branch") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let target_branch = match parse_mpla_branch(request, "target_branch") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let sandbox_id = match request.scope.sandbox_id() {
        Some(value) => value,
        None => {
            return request.invalid_argument("rollback_workspace_session requires sandbox scope")
        }
    };
    match operations
        .workspace_session
        .rollback_mpla_workspace_session(
            run_id,
            &branch,
            &target_branch,
            sandbox_id,
            OperationId::from_string(request.request_id.clone()),
        ) {
        Ok(result) => {
            let selected_profile = match operations.workspace_session.mpla_storage_admin_profile() {
                Ok(profile) => profile,
                Err(error) => return workspace_session_error_response(error),
            };
            match operations
                .workspace_session
                .mpla_storage_scope(&result.workspace_session_id, sandbox_id)
            {
                Ok(storage_admin_scope) => {
                    let mut value = rollback_mpla_workspace_session_value(result);
                    if let Err(error) = attach_storage_admin_authority(
                        &mut value,
                        selected_profile.profile_id(),
                        &storage_admin_scope,
                    ) {
                        return OperationResponse::fault_with_details(
                            "operation_failed",
                            error,
                            json!({}),
                        );
                    }
                    OperationResponse::ok(value)
                }
                Err(error) => workspace_session_error_response(error),
            }
        }
        Err(error) => workspace_session_error_response(error),
    }
}

fn dispatch_squash_mpla_branch(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let run_id = match parse_mpla_run_id(request) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let branch = match parse_mpla_branch(request, "branch") {
        Ok(value) => value,
        Err(response) => return response,
    };
    if request.scope.sandbox_id().is_none() {
        return request.invalid_argument("squash_mpla_branch requires sandbox scope");
    }
    match operations.workspace_session.squash_mpla_branch(
        run_id,
        &branch,
        OperationId::from_string(request.request_id.clone()),
    ) {
        Ok(result) => OperationResponse::ok(squash_mpla_branch_value(result)),
        Err(error) => workspace_session_error_response(error),
    }
}

fn dispatch_publish_mpla_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let started = Instant::now();
    let workspace_session_id =
        WorkspaceSessionId(match request.required_string("workspace_session_id") {
            Ok(value) if !value.trim().is_empty() => value,
            Ok(_) => {
                return request.invalid_argument("workspace_session_id must not be empty");
            }
            Err(response) => return response,
        });
    let branch = match parse_mpla_branch(request, "branch") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let sandbox_id = match request.scope.sandbox_id() {
        Some(value) => value,
        None => {
            return request
                .invalid_argument("publish_mpla_workspace_session requires sandbox scope");
        }
    };
    let result = operations.workspace_session.publish_mpla_workspace_session(
        &workspace_session_id,
        &branch,
        sandbox_id,
        OperationId::from_string(request.request_id.clone()),
    );
    publication_dispatch_checkpoint(
        operations,
        request,
        &workspace_session_id,
        "service_returned",
        &started,
    );
    match result {
        Ok(result) => {
            publication_dispatch_checkpoint(
                operations,
                request,
                &workspace_session_id,
                "response_value_started",
                &started,
            );
            let value = publish_mpla_workspace_session_value(result);
            publication_dispatch_checkpoint(
                operations,
                request,
                &workspace_session_id,
                "response_value_built",
                &started,
            );
            let response = OperationResponse::ok(value);
            publication_dispatch_checkpoint(
                operations,
                request,
                &workspace_session_id,
                "response_wrapped",
                &started,
            );
            response
        }
        Err(error) => workspace_session_error_response(error),
    }
}

fn publication_dispatch_checkpoint(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
    workspace_session_id: &WorkspaceSessionId,
    phase: &'static str,
    started: &Instant,
) {
    operations.workspace_session.obs().event(
        "mpla_publication.dispatch_checkpoint",
        json!({
            "schema_version": 1,
            "request_id": request.request_id,
            "workspace_session_id": workspace_session_id.0,
            "phase": phase,
            "elapsed_ns": u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        }),
    );
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

/// Public, fail-closed bridge to the one fixed MPLA authority helper.
///
/// The request body is untrusted.  The daemon reconstructs the fields it can
/// own from the live workspace-session admission gate before the helper sees
/// it: the routed sandbox id, runtime request id, session id, holder mount
/// namespace, workspace target, and workload cgroup.  This operation never
/// accepts a caller-provided executable, capability set, seccomp policy, or
/// command line.
fn dispatch_mpla_storage_admin(
    operations: &SandboxRuntimeOperations,
    request: &OperationRequest,
) -> OperationResponse {
    let request_json = match parse_exact_storage_admin_argument(request) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let submitted: StorageAdminRequest = match serde_json::from_str(&request_json) {
        Ok(value) => value,
        Err(error) => {
            return request.invalid_argument(format!(
                "request_json must be one exact m2r-iface-v1 StorageAdminRequest: {error}"
            ));
        }
    };
    let sandbox_id = match request.scope.sandbox_id() {
        Some(value) => value,
        None => return request.invalid_argument("mpla_storage_admin requires sandbox scope"),
    };
    let result = operations.workspace_session.execute_mpla_storage_admin(
        &OperationId::from_string(request.request_id.clone()),
        sandbox_id,
        &submitted,
    );
    match result {
        Ok(receipt) => match serde_json::to_value(receipt) {
            Ok(value) => OperationResponse::ok(value),
            Err(error) => OperationResponse::fault_with_details(
                "operation_failed",
                format!("encode storage-admin receipt: {error}"),
                json!({}),
            ),
        },
        Err(error) => workspace_session_error_response(error),
    }
}

fn parse_exact_storage_admin_argument(
    request: &OperationRequest,
) -> Result<String, OperationResponse> {
    let Some(arguments) = request.args.as_object() else {
        return Err(request.invalid_argument("args must be an object"));
    };
    if arguments.len() != 1 || !arguments.contains_key("request_json") {
        return Err(request
            .invalid_argument("mpla_storage_admin accepts exactly one request_json argument"));
    }
    request.required_string("request_json")
}

fn parse_mpla_run_id(
    request: &OperationRequest,
) -> Result<sandbox_runtime_mpla_poc::RunId, OperationResponse> {
    let value = request.required_string("run_id")?;
    if value.trim().is_empty() {
        return Err(request.invalid_argument("run_id must not be empty"));
    }
    sandbox_runtime_mpla_poc::RunId::parse(value)
        .map_err(|error| request.invalid_argument(format!("invalid run_id: {error}")))
}

fn parse_mpla_branch(
    request: &OperationRequest,
    argument: &str,
) -> Result<String, OperationResponse> {
    let value = request.required_string(argument)?;
    let valid = !value.is_empty()
        && value.len() <= 255
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(value)
    } else {
        Err(request.invalid_argument(format!(
            "{argument} must be one safe non-empty path component"
        )))
    }
}

fn attach_storage_admin_authority(
    value: &mut Value,
    profile_id: &str,
    storage_admin_scope: &impl serde::Serialize,
) -> Result<(), &'static str> {
    let object = value
        .as_object_mut()
        .ok_or("MPLA lifecycle response must be an object")?;
    object.insert("storage_admin_profile_id".to_owned(), json!(profile_id));
    object.insert(
        "storage_admin_scope".to_owned(),
        serde_json::to_value(storage_admin_scope)
            .map_err(|_| "MPLA storage-admin scope must serialize")?,
    );
    Ok(())
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

fn activate_mpla_workspace_session_value(result: ActivateMplaWorkspaceSessionResult) -> Value {
    json!({
        "workspace_session_id": result.workspace_session_id.0,
        "fresh_allocation_id": result.fresh_allocation_id,
        "run_id": result.run_id,
        "branch": result.branch,
        "projection": result.projection,
        "lifecycle": mpla_lifecycle_receipt_value(&result.lifecycle),
        "timings": {
            "admission_elapsed_ns": result.timings.admission_elapsed_ns,
            "projection_elapsed_ns": result.timings.projection_elapsed_ns,
            "session_create_elapsed_ns": result.timings.session_create_elapsed_ns,
            "session_identity_elapsed_ns": result.timings.session_identity_elapsed_ns,
            "allocation_create_elapsed_ns": result.timings.allocation_create_elapsed_ns,
            "allocation_lease_elapsed_ns": result.timings.allocation_lease_elapsed_ns,
            "projection_metadata_elapsed_ns": result.timings.projection_metadata_elapsed_ns,
            "external_session_prepare_elapsed_ns": result.timings.external_session_prepare_elapsed_ns,
            "durability_commit_elapsed_ns": result.timings.durability_commit_elapsed_ns,
            "workspace_create_elapsed_ns": result.timings.workspace_create_elapsed_ns,
            "launch_material_elapsed_ns": result.timings.launch_material_elapsed_ns,
            "cgroup_prepare_elapsed_ns": result.timings.cgroup_prepare_elapsed_ns,
            "session_register_elapsed_ns": result.timings.session_register_elapsed_ns,
            "session_commit_elapsed_ns": result.timings.session_commit_elapsed_ns,
            "storage_mount_elapsed_ns": result.timings.storage_mount_elapsed_ns,
            "outcome_persist_elapsed_ns": result.timings.outcome_persist_elapsed_ns,
            "response_elapsed_ns": result.timings.response_elapsed_ns,
        },
        "service_elapsed_ns": result.service_elapsed_ns,
    })
}

fn attach_mpla_prepared_fixture_value(result: AttachMplaPreparedFixtureResult) -> Value {
    json!({
        "run_id": result.run_id,
        "fixture_profile": result.fixture_profile,
        "attached_branches": result.attached_branches,
        "cached_allocation_count": result.cached_allocation_count,
        "payload_bytes_copied": result.payload_bytes_copied,
        "service_elapsed_ns": result.service_elapsed_ns,
    })
}

fn fork_mpla_workspace_session_value(result: ForkMplaWorkspaceSessionResult) -> Value {
    json!({
        "run_id": result.run_id,
        "source_branch": result.source_branch,
        "branch": result.branch,
        "lifecycle": mpla_lifecycle_receipt_value(&result.lifecycle),
        "service_elapsed_ns": result.service_elapsed_ns,
    })
}

fn rollback_mpla_workspace_session_value(result: RollbackMplaWorkspaceSessionResult) -> Value {
    json!({
        "workspace_session_id": result.workspace_session_id.0,
        "fresh_allocation_id": result.fresh_allocation_id,
        "run_id": result.run_id,
        "branch": result.branch,
        "target_branch": result.target_branch,
        "projection": result.projection,
        "lifecycle": mpla_lifecycle_receipt_value(&result.lifecycle),
        "service_elapsed_ns": result.service_elapsed_ns,
    })
}

fn publish_mpla_workspace_session_value(result: PublishMplaWorkspaceSessionResult) -> Value {
    let matched_publication = result.matched_publication_span.map(|span| {
        json!({
            "start_boundary": sandbox_runtime_mpla_poc::MATCHED_PUBLICATION_START_BOUNDARY,
            "stop_boundary": sandbox_runtime_mpla_poc::MATCHED_PUBLICATION_STOP_BOUNDARY,
            "admission_gate_included": true,
            "durable_root_committed": true,
            "session_closed": true,
            "span": span,
        })
    });
    let semantic = result.semantic.map(|receipt| {
        let mut value = serde_json::to_value(receipt)
            .expect("serialize MPLA semantic receipt for publish response");
        attach_incremental_semantic_counters(
            &mut value,
            result.semantic_affected_record_count,
            result.affected_input_bytes,
        );
        value
    });
    json!({
        "workspace_session_id": result.workspace_session_id.0,
        "run_id": result.run_id,
        "branch": result.branch,
        "lifecycle": mpla_lifecycle_receipt_value(&result.lifecycle),
        "affected_path_count": result.affected_path_count,
        "roots": result.roots,
        "semantic": semantic,
        "stationary": result.stationary,
        "affected_payload_bytes_read": result.affected_payload_bytes_read,
        "affected_input_bytes": result.affected_input_bytes,
        "prior_node_bytes_read": result.prior_node_bytes_read,
        "immutable_payload_bytes_read": result.immutable_payload_bytes_read,
        "semantic_root_record_debug": result.semantic_root_record_debug,
        "destroyed": result.destroyed,
        "evicted_upperdir_bytes": result.evicted_upperdir_bytes,
        "phase_elapsed_ns": {
            "pre_storage": result.pre_storage_elapsed_ns,
            "storage_sequence": result.storage_sequence_elapsed_ns,
            "storage_helper_to_unmount": result.storage_helper_to_unmount_elapsed_ns,
            "storage_stable_callback": result.storage_stable_callback_elapsed_ns,
            "storage_helper_cleanup": result.storage_helper_cleanup_elapsed_ns,
            "storage_helper_input_encode": result.storage_helper_input_encode_elapsed_ns,
            "storage_helper_launch": result.storage_helper_launch_elapsed_ns,
            "storage_helper_cgroup_placement": result.storage_helper_cgroup_placement_elapsed_ns,
            "storage_helper_request_write": result.storage_helper_request_write_elapsed_ns,
            "storage_helper_response_wait": result.storage_helper_response_wait_elapsed_ns,
            "storage_helper_unmount_response_decode": result.storage_helper_unmount_response_decode_elapsed_ns,
            "storage_helper_cgroup_release": result.storage_helper_cgroup_release_elapsed_ns,
            "storage_helper_input_decode": result.storage_helper_input_decode_elapsed_ns,
            "storage_helper_validation": result.storage_helper_validation_elapsed_ns,
            "storage_helper_process_preparation": result.storage_helper_process_preparation_elapsed_ns,
            "storage_quiesce_lifecycle": result.storage_quiesce_lifecycle_elapsed_ns,
            "storage_quiesce_operation": result.storage_quiesce_operation_elapsed_ns,
            "storage_strict_unmount_lifecycle": result.storage_strict_unmount_lifecycle_elapsed_ns,
            "storage_strict_unmount_operation": result.storage_strict_unmount_operation_elapsed_ns,
            "semantic_adoption": result.semantic_adoption_elapsed_ns,
            "stationary_adoption": result.stationary_adoption_elapsed_ns,
            "semantic_build": result.semantic_build_elapsed_ns,
            "ref_commit": result.ref_commit_elapsed_ns,
            "session_destroy": result.session_destroy_elapsed_ns,
            "outcome_persist": result.outcome_persist_elapsed_ns,
        },
        "matched_publication": matched_publication,
        "service_elapsed_ns": result.service_elapsed_ns,
    })
}

fn attach_incremental_semantic_counters(
    semantic: &mut Value,
    affected_record_count: Option<u64>,
    affected_input_bytes: u64,
) {
    let Some(affected_record_count) = affected_record_count else {
        return;
    };
    let semantic = semantic
        .as_object_mut()
        .expect("serialize MPLA semantic receipt as an object");
    semantic.insert(
        "affected_record_count".to_owned(),
        json!(affected_record_count),
    );
    semantic.insert(
        "affected_stream_bytes_read".to_owned(),
        json!(affected_input_bytes),
    );
}

fn squash_mpla_branch_value(result: SquashMplaBranchResult) -> Value {
    json!({
        "run_id": result.run_id,
        "branch": result.branch,
        "lifecycle": mpla_lifecycle_receipt_value(&result.lifecycle),
        "service_elapsed_ns": result.service_elapsed_ns,
    })
}

fn mpla_lifecycle_receipt_value(receipt: &MplaLifecycleReceipt) -> Value {
    json!({
        "operation_id": receipt.operation_id,
        "committed": receipt.committed,
        "idempotent_replay": receipt.idempotent_replay,
        "selected_ref": receipt.selected_ref,
        "service_elapsed_ns": receipt.service_elapsed_ns,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpla_lifecycle_response_preserves_server_owned_storage_authority() {
        let mut response = json!({"workspace_session_id": "session-1"});
        let scope = json!({"allocation_id": "allocation-1"});

        attach_storage_admin_authority(
            &mut response,
            "mpla-storage-admin-overlayfs-dac-override-qualification-v1",
            &scope,
        )
        .expect("MPLA response is an object");

        assert_eq!(
            response.get("storage_admin_profile_id"),
            Some(&json!(
                "mpla-storage-admin-overlayfs-dac-override-qualification-v1"
            ))
        );
        assert_eq!(response.get("storage_admin_scope"), Some(&scope));
    }

    #[test]
    fn storage_authority_attachment_rejects_non_object_response() {
        let mut response = json!(null);
        assert_eq!(
            attach_storage_admin_authority(&mut response, "mpla-storage-admin-v1", &json!({}))
                .expect_err("the MPLA lifecycle response must be an object"),
            "MPLA lifecycle response must be an object"
        );
    }

    #[test]
    fn incremental_semantic_reply_preserves_the_service_counters() {
        let mut semantic = json!({"entry_count": 32_831, "bytes_read": 5_737});
        attach_incremental_semantic_counters(&mut semantic, Some(60), 5_737);

        assert_eq!(semantic.pointer("/entry_count"), Some(&json!(32_831)));
        assert_eq!(semantic.pointer("/affected_record_count"), Some(&json!(60)));
        assert_eq!(
            semantic.pointer("/affected_stream_bytes_read"),
            Some(&json!(5_737))
        );
    }
}
