use serde_json::{json, Value};

use crate::cli_definition::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationFamilySpec, CliOperationSpec, CliSpec,
};
use crate::command::CommandSessionId;
use crate::observability::{measure_optional, OperationTrace};
use crate::operation::OperationEntry;
use crate::workspace_crate::{
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, WorkspaceProfile,
    WorkspaceSessionId,
};
use crate::workspace_session::{WorkspaceSessionError, WorkspaceSessionHandler};
use crate::SandboxRuntimeOperations;
use sandbox_protocol::{Request, Response};

pub(crate) const WORKSPACE_SESSION_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "workspace_session",
    title: "Workspace Session",
    summary: "Create and destroy runtime workspace sessions.",
    description: "Create and destroy user-owned runtime workspace sessions.",
};

const CREATE_SPEC: CliOperationSpec = CliOperationSpec {
    name: "create_workspace_session",
    family: "workspace_session",
    summary: "Create a runtime workspace session.",
    description: "Create a user-owned runtime workspace session. When profile is omitted, the runtime creates a host-compatible workspace.",
    args: CREATE_ARGS,
    cli: Some(CliSpec {
        path: &["runtime", "create_workspace_session"],
        usage: "sandbox-cli runtime create_workspace_session [--profile PROFILE]",
        examples: &[
            "sandbox-cli runtime create_workspace_session",
            "sandbox-cli runtime create_workspace_session --profile host_compatible",
            "sandbox-cli runtime create_workspace_session --profile isolated",
        ],
    }),
    related: &["destroy_workspace_session", "exec_command"],
};

const CREATE_ARGS: &[ArgSpec] = &[ArgSpec::optional(
    "profile",
    ArgKind::String,
    "Workspace profile: host_compatible or isolated. Defaults to host_compatible when omitted.",
    None,
    Some(ArgCliSpec {
        flag: Some("--profile"),
        positional: None,
    }),
)];

const DESTROY_SPEC: CliOperationSpec = CliOperationSpec {
    name: "destroy_workspace_session",
    family: "workspace_session",
    summary: "Destroy a runtime workspace session.",
    description: "Destroy a user-owned runtime workspace session by workspace_session_id when no commands are active in that session.",
    args: DESTROY_ARGS,
    cli: Some(CliSpec {
        path: &["runtime", "destroy_workspace_session"],
        usage: "sandbox-cli runtime destroy_workspace_session --workspace-session-id ID [--grace-s SECONDS]",
        examples: &[
            "sandbox-cli runtime destroy_workspace_session --workspace-session-id ws-1",
            "sandbox-cli runtime destroy_workspace_session --workspace-session-id ws-1 --grace-s 2.5",
        ],
    }),
    related: &["create_workspace_session", "exec_command"],
};

const DESTROY_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "workspace_session_id",
        ArgKind::String,
        "Workspace session id to destroy.",
        Some(ArgCliSpec {
            flag: Some("--workspace-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "grace_s",
        ArgKind::Float,
        "Optional process teardown grace period in seconds.",
        None,
        Some(ArgCliSpec {
            flag: Some("--grace-s"),
            positional: None,
        }),
    ),
];

const CREATE_WORKSPACE_SESSION: OperationEntry =
    OperationEntry::cli(&CREATE_SPEC, dispatch_create_workspace_session);
const DESTROY_WORKSPACE_SESSION: OperationEntry =
    OperationEntry::cli(&DESTROY_SPEC, dispatch_destroy_workspace_session);

const OPERATIONS: &[OperationEntry] = &[CREATE_WORKSPACE_SESSION, DESTROY_WORKSPACE_SESSION];

pub(crate) fn operation_entries() -> &'static [OperationEntry] {
    OPERATIONS
}

fn dispatch_create_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &Request,
    trace: Option<&OperationTrace>,
) -> Response {
    let profile = match parse_workspace_profile(request) {
        Ok(profile) => profile,
        Err(response) => return response,
    };
    workspace_session_handler_response(measure_optional(
        trace,
        "WorkspaceSessionService::create_workspace_session",
        || {
            operations
                .workspace_session
                .create_workspace_session(CreateWorkspaceRequest { profile })
        },
    ))
}

fn dispatch_destroy_workspace_session(
    operations: &SandboxRuntimeOperations,
    request: &Request,
    trace: Option<&OperationTrace>,
) -> Response {
    let input = match parse_destroy_workspace_session(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    operations.command.with_workspace_destroy_admission(
        &input.workspace_session_id,
        |active_command_session_ids| {
            if !active_command_session_ids.is_empty() {
                return active_command_rejection(active_command_session_ids);
            }

            let handler = match operations
                .workspace_session
                .resolve_session(input.workspace_session_id.clone())
            {
                Ok(handler) => handler,
                Err(error) => return workspace_session_error_response(error),
            };
            destroy_workspace_session_response(measure_optional(
                trace,
                "WorkspaceSessionService::destroy_session",
                || {
                    operations.workspace_session.destroy_session(
                        handler,
                        DestroyWorkspaceRequest {
                            grace_s: input.grace_s,
                        },
                    )
                },
            ))
        },
    )
}

fn parse_workspace_profile(request: &Request) -> Result<WorkspaceProfile, Response> {
    match request.optional_string("profile")? {
        None => Ok(WorkspaceProfile::HostCompatible),
        Some(profile) if profile == WorkspaceProfile::HostCompatible.as_str() => {
            Ok(WorkspaceProfile::HostCompatible)
        }
        Some(profile) if profile == WorkspaceProfile::Isolated.as_str() => {
            Ok(WorkspaceProfile::Isolated)
        }
        Some(_) => {
            Err(request.invalid_argument("profile must be one of host_compatible or isolated"))
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

fn destroy_workspace_session_response(
    result: Result<DestroyWorkspaceResult, WorkspaceSessionError>,
) -> Response {
    match result {
        Ok(result) => Response::ok(destroy_workspace_session_value(result)),
        Err(error) => workspace_session_error_response(error),
    }
}

fn workspace_session_error_response(error: WorkspaceSessionError) -> Response {
    Response::fault_with_details("operation_failed", error.to_string(), json!({}))
}

fn active_command_rejection(active_command_session_ids: &[CommandSessionId]) -> Response {
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
        "profile": handler.handle.profile.as_str(),
    })
}

fn destroy_workspace_session_value(result: DestroyWorkspaceResult) -> Value {
    json!({
        "workspace_session_id": result.workspace_session_id.0,
        "destroyed": true,
    })
}
