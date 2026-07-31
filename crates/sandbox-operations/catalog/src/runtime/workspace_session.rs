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

pub const ATTACH_MPLA_PREPARED_FIXTURE: RoutedOperation = RoutedOperation {
    spec: &ATTACH_MPLA_PREPARED_FIXTURE_SPEC,
    routing: RUNTIME_OWNED,
};

pub const ACTIVATE_WORKSPACE_SESSION: RoutedOperation = RoutedOperation {
    spec: &ACTIVATE_WORKSPACE_SESSION_SPEC,
    routing: RUNTIME_OWNED,
};

pub const FORK_WORKSPACE_SESSION: RoutedOperation = RoutedOperation {
    spec: &FORK_WORKSPACE_SESSION_SPEC,
    routing: RUNTIME_OWNED,
};

pub const ROLLBACK_WORKSPACE_SESSION: RoutedOperation = RoutedOperation {
    spec: &ROLLBACK_WORKSPACE_SESSION_SPEC,
    routing: RUNTIME_OWNED,
};

pub const PUBLISH_MPLA_WORKSPACE_SESSION: RoutedOperation = RoutedOperation {
    spec: &PUBLISH_MPLA_WORKSPACE_SESSION_SPEC,
    routing: RUNTIME_OWNED,
};

pub const SQUASH_MPLA_BRANCH: RoutedOperation = RoutedOperation {
    spec: &SQUASH_MPLA_BRANCH_SPEC,
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

pub const ATTACH_MPLA_PREPARED_FIXTURE_SPEC: OperationSpec = OperationSpec {
    name: "attach_mpla_prepared_fixture",
    family: "workspace_session",
    summary: "Attach the fixed immutable MPLA scorecard fixture to a fresh run.",
    description: "Validate the server-owned s4-chain-v1 fixture cache and install only fresh local refs, locators, and projection metadata for the selected run. This operation never copies fixture payload bytes or creates a writable upper; activation remains responsible for a fresh upper and lease.",
    args: ATTACH_MPLA_PREPARED_FIXTURE_ARGS,
    related: &["activate_workspace_session", "fork_workspace_session"],
};

const ATTACH_MPLA_PREPARED_FIXTURE_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "run_id",
        ArgKind::String,
        "Fresh MPLA run identity receiving only local fixture metadata.",
    ),
    ArgSpec::required(
        "fixture_profile",
        ArgKind::String,
        "Closed prepared fixture profile. Only s4-chain-v1 is accepted.",
    ),
];

const CREATE_MPLA_WORKSPACE_SESSION_ARGS: &[ArgSpec] = &[ArgSpec::required(
    "run_id",
    ArgKind::String,
    "MPLA qualification run identity bound permanently to the allocation lease.",
)];

pub const ACTIVATE_WORKSPACE_SESSION_SPEC: OperationSpec = OperationSpec {
    name: "activate_workspace_session",
    family: "workspace_session",
    summary: "Activate an MPLA branch as a usable workspace session.",
    description: "Resolve a branch within daemon-owned MPLA state, allocate a fresh private upper, mount its stored projection, and return a workspace session only after readiness succeeds. The daemon derives lifecycle roots, allocation paths, leases, namespace targets, and storage capability authority; callers select only the run and branch identities.",
    args: ACTIVATE_WORKSPACE_SESSION_ARGS,
    related: &[
        "fork_workspace_session",
        "rollback_workspace_session",
        "publish_mpla_workspace_session",
        "squash_mpla_branch",
        "destroy_workspace_session",
        "exec_command",
    ],
};

const ACTIVATE_WORKSPACE_SESSION_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "run_id",
        ArgKind::String,
        "MPLA run identity resolved within the sandbox's configured lifecycle roots.",
    ),
    ArgSpec::required("branch", ArgKind::String, "Stored MPLA branch to activate."),
];

pub const FORK_WORKSPACE_SESSION_SPEC: OperationSpec = OperationSpec {
    name: "fork_workspace_session",
    family: "workspace_session",
    summary: "Create an inactive metadata-only MPLA branch.",
    description: "Create a durable branch selector from an existing branch in the same daemon-owned MPLA run without allocating a workspace session, private upper, mount, or payload copy. Use activate_workspace_session separately when the child must become usable.",
    args: FORK_WORKSPACE_SESSION_ARGS,
    related: &["activate_workspace_session", "rollback_workspace_session"],
};

const FORK_WORKSPACE_SESSION_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "run_id",
        ArgKind::String,
        "MPLA run identity resolved within the sandbox's configured lifecycle roots.",
    ),
    ArgSpec::required(
        "source_branch",
        ArgKind::String,
        "Existing branch whose durable selection the child inherits.",
    ),
    ArgSpec::required("branch", ArgKind::String, "New inactive branch identity."),
];

pub const ROLLBACK_WORKSPACE_SESSION_SPEC: OperationSpec = OperationSpec {
    name: "rollback_workspace_session",
    family: "workspace_session",
    summary: "Roll back an MPLA branch and activate the selected state.",
    description: "Atomically replace a branch selector with the durable selection of a target branch in the same daemon-owned MPLA run, then return a newly activated workspace session only after readiness succeeds. The daemon derives lifecycle roots, allocations, leases, namespace targets, and storage capability authority.",
    args: ROLLBACK_WORKSPACE_SESSION_ARGS,
    related: &[
        "activate_workspace_session",
        "fork_workspace_session",
        "publish_mpla_workspace_session",
        "squash_mpla_branch",
        "destroy_workspace_session",
        "exec_command",
    ],
};

const ROLLBACK_WORKSPACE_SESSION_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "run_id",
        ArgKind::String,
        "MPLA run identity resolved within the sandbox's configured lifecycle roots.",
    ),
    ArgSpec::required(
        "branch",
        ArgKind::String,
        "Branch whose durable selector is replaced.",
    ),
    ArgSpec::required(
        "target_branch",
        ArgKind::String,
        "Existing branch that supplies the rollback selection.",
    ),
];

pub const PUBLISH_MPLA_WORKSPACE_SESSION_SPEC: OperationSpec = OperationSpec {
    name: "publish_mpla_workspace_session",
    family: "workspace_session",
    summary: "Publish an MPLA workspace session and close it.",
    description: "Fence new command admission, drain the selected session, strictly unmount and durably adopt its private upper, atomically replace the selected MPLA branch, and close the workspace session. The daemon reconstructs run, allocation, lease, mount, lifecycle-root, and storage-capability authority from authenticated sandbox state; callers select only the session and branch identities.",
    args: PUBLISH_MPLA_WORKSPACE_SESSION_ARGS,
    related: &[
        "activate_workspace_session",
        "rollback_workspace_session",
        "squash_mpla_branch",
        "destroy_workspace_session",
        "exec_command",
    ],
};

const PUBLISH_MPLA_WORKSPACE_SESSION_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "workspace_session_id",
        ArgKind::String,
        "Authenticated MPLA workspace session to publish and close.",
    ),
    ArgSpec::required(
        "branch",
        ArgKind::String,
        "MPLA branch whose durable selector receives the published upper.",
    ),
];

pub const SQUASH_MPLA_BRANCH_SPEC: OperationSpec = OperationSpec {
    name: "squash_mpla_branch",
    family: "workspace_session",
    summary: "Logically squash an MPLA branch.",
    description: "Replace a branch's prepared MPLA selector and ancestry metadata with an equivalent compact durable selection while preserving canonical root identity, filesystem semantics, and attribution. This metadata-only operation must not scan, reconstruct, copy, or physically flatten immutable payload. The daemon derives lifecycle roots and storage authority from authenticated sandbox state.",
    args: SQUASH_MPLA_BRANCH_ARGS,
    related: &[
        "activate_workspace_session",
        "fork_workspace_session",
        "rollback_workspace_session",
        "publish_mpla_workspace_session",
    ],
};

const SQUASH_MPLA_BRANCH_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "run_id",
        ArgKind::String,
        "MPLA run identity resolved within the sandbox's configured lifecycle roots.",
    ),
    ArgSpec::required(
        "branch",
        ArgKind::String,
        "MPLA branch whose logical ancestry is compacted.",
    ),
];

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
