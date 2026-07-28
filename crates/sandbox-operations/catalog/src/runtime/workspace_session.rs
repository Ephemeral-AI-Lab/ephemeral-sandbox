use sandbox_operation_contract::{
    ArgKind, ArgSpec, OperationExecutionOwner, OperationFamilySpec, OperationSpec,
};

use crate::routed::{RoutedOperation, Routing};

const RUNTIME_OWNED: Routing = Routing::Sandbox(OperationExecutionOwner::Runtime);

pub const CREATE_WORKSPACE_SESSION: RoutedOperation = RoutedOperation {
    spec: &CREATE_WORKSPACE_SESSION_SPEC,
    routing: RUNTIME_OWNED,
};

pub const CREATE_MPLA_WORKSPACE_SESSION: RoutedOperation = RoutedOperation {
    spec: &CREATE_MPLA_WORKSPACE_SESSION_SPEC,
    routing: RUNTIME_OWNED,
};

pub const PUBLISH_WORKSPACE_SESSION: RoutedOperation = RoutedOperation {
    spec: &PUBLISH_WORKSPACE_SESSION_SPEC,
    routing: RUNTIME_OWNED,
};

pub const DESTROY_WORKSPACE_SESSION: RoutedOperation = RoutedOperation {
    spec: &DESTROY_WORKSPACE_SESSION_SPEC,
    routing: RUNTIME_OWNED,
};

pub const MPLA_STORAGE_ADMIN: RoutedOperation = RoutedOperation {
    spec: &MPLA_STORAGE_ADMIN_SPEC,
    routing: RUNTIME_OWNED,
};

pub const WORKSPACE_SESSION_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "workspace_session",
    title: "Workspace session",
    summary: "Workspace-session lifecycle capability.",
    description: "Runtime capability that owns workspace-session lifecycle and finalization.",
};

pub const CREATE_WORKSPACE_SESSION_SPEC: OperationSpec = OperationSpec {
    name: "create_workspace_session",
    family: "workspace_session",
    summary: "Create an explicit workspace session.",
    description: "Create an explicit workspace session with finalize policy no_op. Commands and file operations can target the returned workspace_session_id. Private changes remain available while the session is live and are discarded when the session is destroyed.",
    args: CREATE_WORKSPACE_SESSION_ARGS,
    related: &["destroy_workspace_session", "exec_command"],
};

pub const CREATE_MPLA_WORKSPACE_SESSION_SPEC: OperationSpec = OperationSpec {
    name: "create_mpla_workspace_session",
    family: "workspace_session",
    summary: "Create an unmounted MPLA-backed workspace session.",
    description: "Create a dedicated MPLA allocation and an explicit shared-network workspace holder with no mounted overlay. The returned storage_admin_scope is server-derived and must be used with mpla_storage_admin to mount, quiesce, strictly unmount, and clean up the session. Ordinary commands and file operations are rejected until a successful mount receipt.",
    args: CREATE_MPLA_WORKSPACE_SESSION_ARGS,
    related: &[
        "mpla_storage_admin",
        "destroy_workspace_session",
        "exec_command",
    ],
};

const CREATE_MPLA_WORKSPACE_SESSION_ARGS: &[ArgSpec] = &[ArgSpec::required(
    "run_id",
    ArgKind::String,
    "MPLA qualification run identity bound permanently to the allocation lease.",
)];

const CREATE_WORKSPACE_SESSION_ARGS: &[ArgSpec] = &[ArgSpec::optional(
    "network_profile",
    ArgKind::String,
    "Network profile for the session: shared or isolated. Defaults to shared.",
    Some("shared"),
)];

pub const PUBLISH_WORKSPACE_SESSION_SPEC: OperationSpec = OperationSpec {
    name: "publish_workspace_session",
    family: "workspace_session",
    summary: "Publish an explicit workspace session and close it.",
    description: "Capture the unpublished changes of an explicit workspace session, merge them safely into the current LayerStack when possible, and close the session. Rejected or failed pre-commit publishes retain the session.",
    args: PUBLISH_WORKSPACE_SESSION_ARGS,
    related: &[
        "create_workspace_session",
        "destroy_workspace_session",
        "exec_command",
    ],
};

const PUBLISH_WORKSPACE_SESSION_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "workspace_session_id",
        ArgKind::String,
        "Explicit workspace session to publish and close.",
    ),
    ArgSpec::optional(
        "grace_s",
        ArgKind::Float,
        "Optional non-negative close grace period in seconds.",
        None,
    ),
];

pub const DESTROY_WORKSPACE_SESSION_SPEC: OperationSpec = OperationSpec {
    name: "destroy_workspace_session",
    family: "workspace_session",
    summary: "Destroy an explicit workspace session.",
    description: "Destroy an explicit workspace session and discard its unpublished changes. The operation is rejected while the session has active commands.",
    args: DESTROY_WORKSPACE_SESSION_ARGS,
    related: &["create_workspace_session", "exec_command"],
};

const DESTROY_WORKSPACE_SESSION_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "workspace_session_id",
        ArgKind::String,
        "Workspace session id to destroy.",
    ),
    ArgSpec::optional(
        "grace_s",
        ArgKind::Float,
        "Optional non-negative destroy grace period in seconds.",
        None,
    ),
];

pub const MPLA_STORAGE_ADMIN_SPEC: OperationSpec = OperationSpec {
    name: "mpla_storage_admin",
    family: "workspace_session",
    summary: "Run one authenticated MPLA storage lifecycle action.",
    description: "Submit one m2r-iface-v1 storage lifecycle request for a live explicit workspace session. The daemon authenticates and binds the request to the sandbox, workspace session, allocation, lease, mount namespace, and exact MPLA roots before invoking the fixed mpla-storage-admin-v1 executable. Caller-selected executables, capabilities, syscalls, or path widening are never accepted.",
    args: MPLA_STORAGE_ADMIN_ARGS,
    related: &[
        "create_mpla_workspace_session",
        "destroy_workspace_session",
        "exec_command",
    ],
};

const MPLA_STORAGE_ADMIN_ARGS: &[ArgSpec] = &[ArgSpec::required(
    "request_json",
    ArgKind::String,
    "Exact m2r-iface-v1 StorageAdminRequest JSON. Authority is reconstructed from server-owned state.",
)];
