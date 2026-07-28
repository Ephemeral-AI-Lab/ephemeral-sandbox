use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use sandbox_runtime_mpla_poc::lease::validate_active_storage_admin_lease;
use sandbox_runtime_mpla_poc::storage_admin::StorageAdminInvocation;
use sandbox_runtime_mpla_poc::{
    OperationId, StorageAdminAuthorization, StorageAdminReceipt, StorageAdminRequest,
    STORAGE_ADMIN_TRUSTED_EXECUTABLE,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::operations::dispatch::OperationEntry;
use crate::workspace_crate::{DestroyWorkspaceResult, NetworkProfile, WorkspaceSessionId};
use crate::workspace_session::MplaWorkspaceBinding;
use crate::workspace_session::{
    CreateSessionRequest, FinalizePolicy, PublishWorkspaceSessionResult, WorkspaceSessionError,
    WorkspaceSessionHandler, WorkspaceSessionPublishDetails,
};
use crate::SandboxRuntimeOperations;
use sandbox_operation_catalog::internal::runtime::CREATE_WORKSPACE_SESSION_LEGACY_SCRATCH_ADAPTER;
use sandbox_operation_catalog::runtime::{
    CREATE_MPLA_WORKSPACE_SESSION_SPEC, CREATE_WORKSPACE_SESSION_SPEC,
    DESTROY_WORKSPACE_SESSION_SPEC, MPLA_STORAGE_ADMIN_SPEC, PUBLISH_WORKSPACE_SESSION_SPEC,
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
                "storage_admin_scope": storage_admin_scope,
            })),
            Err(error) => workspace_session_error_response(error),
        },
        Err(error) => workspace_session_error_response(error),
    }
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
    let workspace_session_id = WorkspaceSessionId(submitted.scope.workspace_session_id.clone());
    let result = operations.workspace_session.with_gated_mpla_storage_action(
        &workspace_session_id,
        submitted.action,
        |handler, binding| {
            let receipt =
                bind_and_run_storage_admin(request, sandbox_id, &submitted, handler, binding)?;
            Ok((receipt.clone(), receipt))
        },
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

fn bind_and_run_storage_admin(
    request: &OperationRequest,
    sandbox_id: &str,
    submitted: &StorageAdminRequest,
    handler: &WorkspaceSessionHandler,
    binding: &MplaWorkspaceBinding,
) -> Result<StorageAdminReceipt, String> {
    if submitted.operation_id.as_str() != request.request_id {
        return Err("storage-admin operation_id must equal the routed request_id".to_owned());
    }
    if submitted.scope.sandbox_id != sandbox_id {
        return Err("storage-admin sandbox id does not match the routed sandbox".to_owned());
    }
    if submitted.scope.workspace_session_id != handler.workspace_session_id.0 {
        return Err("storage-admin workspace session is not the live gated session".to_owned());
    }
    if submitted.scope.workspace_root != handler.handle.workspace_root {
        return Err(
            "storage-admin workspace root is not the live session workspace root".to_owned(),
        );
    }
    let holder_pid = u32::try_from(handler.handle.holder_pid)
        .map_err(|_| "storage-admin live holder pid is invalid".to_owned())?;
    if holder_pid == 0 {
        return Err("storage-admin live holder pid is invalid".to_owned());
    }
    let mount_namespace_id = fs::read_link(format!("/proc/{holder_pid}/ns/mnt"))
        .map_err(|error| format!("read live holder mount namespace: {error}"))?
        .to_string_lossy()
        .into_owned();
    if submitted.scope.mount_namespace_id != mount_namespace_id {
        return Err("storage-admin mount namespace does not match the live holder".to_owned());
    }
    require_scoped_mpla_paths(submitted, handler, binding)?;
    validate_active_storage_admin_lease(
        &submitted.scope.allocation_root,
        &submitted.scope.allocation_id,
        &submitted.scope.session_id,
        &submitted.scope.lease_id,
        submitted.scope.lease_epoch,
    )
    .map_err(|error| format!("validate live MPLA lease binding: {error}"))?;
    let expected_request = StorageAdminRequest {
        schema_version: submitted.schema_version,
        interface_version: submitted.interface_version.clone(),
        profile_id: submitted.profile_id.clone(),
        operation_id: OperationId::from_string(request.request_id.clone()),
        action: submitted.action,
        scope: submitted.scope.clone(),
    };
    let authorization = StorageAdminAuthorization {
        authenticated: true,
        actor_id: "sandbox-runtime-storage-admin".to_owned(),
        operation_id: expected_request.operation_id.clone(),
        run_id: expected_request.scope.run_id.clone(),
        sandbox_id: sandbox_id.to_owned(),
        workspace_session_id: handler.workspace_session_id.0.clone(),
        session_id: expected_request.scope.session_id.clone(),
        allocation_id: expected_request.scope.allocation_id.clone(),
        lease_id: expected_request.scope.lease_id.clone(),
        lease_epoch: expected_request.scope.lease_epoch,
        mount_namespace_id,
    };
    let workload_cgroup_procs = handler
        .cgroup_path
        .as_ref()
        .map(|path| path.join("cgroup.procs"))
        .ok_or_else(|| "storage-admin requires a live workload cgroup".to_owned())?;
    let invocation = StorageAdminInvocation {
        expected_request: expected_request.clone(),
        request: expected_request,
        authorization,
        trusted_actor_id: "sandbox-runtime-storage-admin".to_owned(),
        trusted_executable_sha256: trusted_storage_admin_executable_sha256()?,
        workload_cgroup_procs,
        mount_namespace_holder_pid: holder_pid,
    };
    let receipt = run_fixed_storage_admin(&invocation)?;
    if receipt.scope != submitted.scope
        || receipt.action != submitted.action
        || receipt.operation_id != submitted.operation_id
    {
        return Err("storage-admin receipt does not match the bound request".to_owned());
    }
    Ok(receipt)
}

fn trusted_storage_admin_executable_sha256() -> Result<String, String> {
    let mut executable = fs::File::open(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
        .map_err(|error| format!("open fixed storage-admin helper for hashing: {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = executable
            .read(&mut buffer)
            .map_err(|error| format!("read fixed storage-admin helper for hashing: {error}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let mut hash = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        write!(&mut hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(hash)
}

fn require_scoped_mpla_paths(
    submitted: &StorageAdminRequest,
    handler: &WorkspaceSessionHandler,
    binding: &MplaWorkspaceBinding,
) -> Result<(), String> {
    let entry = handler
        .handle
        .entry()
        .map_err(|error| format!("read live workspace launch material: {error}"))?;
    let workspace_root = &handler.handle.workspace_root;
    if entry.workspace_root != *workspace_root
        || submitted.scope.workspace_root != *workspace_root
        || submitted.scope.workspace_root != binding.prepared.workspace_root()
    {
        return Err("storage-admin workspace target is not server-owned".to_owned());
    }
    let allocation_root = &binding.allocation.allocation_root;
    if entry.upperdir != binding.allocation.upper_dir
        || entry.workdir != binding.allocation.work_dir
    {
        return Err("live workspace allocation layout is not MPLA-owned".to_owned());
    }
    if allocation_root.file_name().and_then(|part| part.to_str())
        != Some(submitted.scope.allocation_id.as_str())
        || submitted.scope.allocation_root != *allocation_root
        || submitted.scope.allocation_id != binding.allocation.descriptor.allocation_id
    {
        return Err("storage-admin allocation root is not the live MPLA allocation".to_owned());
    }
    let allocations_root = allocation_root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "live MPLA allocation has no payload allocations root".to_owned())?;
    if allocations_root.file_name().and_then(|part| part.to_str()) != Some("allocations") {
        return Err("live MPLA allocation is outside the payload allocations root".to_owned());
    }
    let payload_root = allocations_root
        .parent()
        .ok_or_else(|| "live MPLA allocations root has no payload root".to_owned())?;
    if submitted.scope.payload_root != payload_root
        || submitted.scope.payload_root != binding.payload_root
    {
        return Err(
            "storage-admin payload root is not derived from the live allocation".to_owned(),
        );
    }
    let session_dir = workspace_root
        .parent()
        .ok_or_else(|| "live workspace target has no MPLA session directory".to_owned())?;
    if workspace_root.file_name().and_then(|part| part.to_str()) != Some("mount")
        || session_dir.file_name().and_then(|part| part.to_str())
            != Some(submitted.scope.session_id.as_str())
    {
        return Err("storage-admin workspace target is not the live MPLA session mount".to_owned());
    }
    let sessions_root = session_dir
        .parent()
        .ok_or_else(|| "live MPLA session has no sessions root".to_owned())?;
    if sessions_root.file_name().and_then(|part| part.to_str()) != Some("sessions") {
        return Err("live MPLA session is outside the control sessions root".to_owned());
    }
    let control_root = sessions_root
        .parent()
        .ok_or_else(|| "live MPLA sessions root has no control root".to_owned())?;
    if submitted.scope.control_root != control_root
        || submitted.scope.control_root != binding.control_root
    {
        return Err("storage-admin control root is not derived from the live session".to_owned());
    }
    if submitted.scope.lower_dirs_newest_first != entry.layer_paths {
        return Err(
            "storage-admin lower directories do not match the live workspace layers".to_owned(),
        );
    }
    if submitted.scope.run_id != binding.run_id
        || submitted.scope.session_id != binding.lease.session_id
        || submitted.scope.lease_id != binding.lease_operation_id.as_str()
        || submitted.scope.lease_epoch != binding.lease.lease_epoch
    {
        return Err(
            "storage-admin request is not bound to the server-owned MPLA run and lease".to_owned(),
        );
    }
    Ok(())
}

fn run_fixed_storage_admin(
    invocation: &StorageAdminInvocation,
) -> Result<StorageAdminReceipt, String> {
    const MAX_INVOCATION_BYTES: usize = 1024 * 1024;
    const MAX_RECEIPT_BYTES: usize = 1024 * 1024;
    let input = serde_json::to_vec(invocation)
        .map_err(|error| format!("encode bound storage-admin invocation: {error}"))?;
    if input.len() > MAX_INVOCATION_BYTES {
        return Err("bound storage-admin invocation exceeds one mebibyte".to_owned());
    }
    let cgroup_procs = &invocation.workload_cgroup_procs;
    let mut child = Command::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
        .args(std::iter::empty::<&str>())
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn fixed storage-admin helper: {error}"))?;
    let helper_pid = child.id();
    if let Err(error) = fs::write(&cgroup_procs, helper_pid.to_string()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "place storage-admin helper in {}: {error}",
            cgroup_procs.display()
        ));
    }
    let cgroup_members = fs::read_to_string(&cgroup_procs)
        .map_err(|error| format!("verify storage-admin cgroup placement: {error}"))?;
    if !cgroup_members
        .lines()
        .any(|member| member.trim() == helper_pid.to_string())
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err("storage-admin helper is not in the bound workload cgroup".to_owned());
    }
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "storage-admin helper stdin is unavailable".to_owned())?;
    stdin
        .write_all(&input)
        .map_err(|error| format!("write bound storage-admin invocation: {error}"))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait for storage-admin helper: {error}"))?;
    if output.stdout.len() > MAX_RECEIPT_BYTES || output.stderr.len() > MAX_RECEIPT_BYTES {
        return Err("storage-admin helper exceeded its bounded response budget".to_owned());
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "fixed storage-admin helper failed: {}",
            stderr.trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("decode fixed storage-admin receipt: {error}"))
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
