use sandbox_protocol::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationFamilySpec, CliOperationSpec, CliSpec,
};

pub const WORKSPACE_SESSION_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "workspace_session",
    title: "Workspace Session",
    summary: "Create and destroy runtime workspace sessions.",
    description: "Create and destroy runtime workspace sessions.",
};

pub const CREATE_WORKSPACE_SESSION_SPEC: CliOperationSpec = CliOperationSpec {
    name: "create_workspace_session",
    family: "workspace_session",
    summary: "Create a runtime workspace session.",
    description: "Create a runtime workspace session with finalize policy no_op: the session lives until destroy_workspace_session. When network profile is omitted, the runtime creates a shared-network workspace.",
    args: CREATE_ARGS,
    cli: Some(CliSpec {
        path: &["runtime", "create_workspace_session"],
        usage: "sandbox-runtime-cli --sandbox-id ID create_workspace_session [--network-profile PROFILE]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID create_workspace_session",
            "sandbox-runtime-cli --sandbox-id ID create_workspace_session --network-profile shared",
            "sandbox-runtime-cli --sandbox-id ID create_workspace_session --network-profile isolated",
        ],
    }),
    related: &["destroy_workspace_session", "exec_command"],
};

const CREATE_ARGS: &[ArgSpec] = &[ArgSpec::optional(
    "network_profile",
    ArgKind::String,
    "Network profile: 'shared' joins the host network namespace (still isolated in mount/pid/user) or 'isolated' uses a dedicated network namespace. Defaults to 'shared' when omitted.",
    None,
    Some(ArgCliSpec {
        flag: Some("--network-profile"),
        positional: None,
    }),
)];

pub const DESTROY_WORKSPACE_SESSION_SPEC: CliOperationSpec = CliOperationSpec {
    name: "destroy_workspace_session",
    family: "workspace_session",
    summary: "Destroy a runtime workspace session.",
    description: "Destroy a runtime workspace session by workspace_session_id, always discarding unpublished changes regardless of the session's finalize policy. Refuses while the session's command ledger is non-empty, reporting active_command_session_ids. Sessions whose finalization failed remain destroyable through this operation.",
    args: DESTROY_ARGS,
    cli: Some(CliSpec {
        path: &["runtime", "destroy_workspace_session"],
        usage: "sandbox-runtime-cli --sandbox-id ID destroy_workspace_session --workspace-session-id ID [--grace-s SECONDS]",
        examples: &[
            "sandbox-runtime-cli --sandbox-id ID destroy_workspace_session --workspace-session-id ws-1",
            "sandbox-runtime-cli --sandbox-id ID destroy_workspace_session --workspace-session-id ws-1 --grace-s 2.5",
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
