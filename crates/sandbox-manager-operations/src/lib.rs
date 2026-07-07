//! Manager CLI operation surface (the `manager` execution space).
//!
//! This crate is **spec-only**: it owns the `CliOperationSpec` catalog for the
//! manager execution space and nothing else. Dispatch tables and fn-pointers
//! live in `sandbox-manager`, which imports these specs. Keeping the catalog in
//! a thin, dependency-light crate lets protocol clients link the manager
//! operation surface without pulling in the manager server tree.
#![forbid(unsafe_code)]

use sandbox_protocol::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationCatalog, CliOperationExecutionSpace,
    CliOperationFamilySpec, CliOperationSpec, CliSpec,
};

pub const MANAGEMENT_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "management",
    title: "Management",
    summary: "Create, destroy, list, and inspect sandbox records.",
    description: "Create, destroy, list, and inspect sandbox records. Daemons are managed as part of sandbox lifecycle behavior, not as standalone manager operations.",
};

pub const CREATE_SANDBOX_SPEC: CliOperationSpec = CliOperationSpec {
    name: "create_sandbox",
    family: "management",
    summary: "Create a host-side sandbox record and runtime sandbox.",
    description:
        "Create a host-side sandbox record, create the runtime sandbox, and start its daemon.",
    args: CREATE_SANDBOX_ARGS,
    cli: Some(CREATE_SANDBOX_CLI),
    related: &["list_sandboxes", "inspect_sandbox", "destroy_sandbox"],
};

const CREATE_SANDBOX_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "image",
        ArgKind::String,
        "Container image used to create the sandbox.",
        Some(ArgCliSpec {
            flag: Some("--image"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "workspace_root",
        ArgKind::Path,
        "Absolute host workspace directory bind-mounted into this sandbox.",
        Some(ArgCliSpec {
            flag: Some("--workspace-bind-root"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "count",
        ArgKind::Integer,
        "Number of sandboxes to create. Values greater than 1 use a shared read-only workspace base.",
        None,
        Some(ArgCliSpec {
            flag: Some("--count"),
            positional: None,
        }),
    ),
];

const CREATE_SANDBOX_CLI: CliSpec = CliSpec {
    path: &["manager", "create_sandbox"],
    usage: "sandbox-manager-cli create_sandbox --image IMAGE --workspace-bind-root PATH [--count N]",
    examples: &[
        "sandbox-manager-cli create_sandbox --image ubuntu:24.04 --workspace-bind-root /testbed",
        "sandbox-manager-cli create_sandbox --image ubuntu:24.04 --workspace-bind-root /testbed --count 5",
    ],
};

pub const DESTROY_SANDBOX_SPEC: CliOperationSpec = CliOperationSpec {
    name: "destroy_sandbox",
    family: "management",
    summary: "Destroy a host-side sandbox and remove it from the registry.",
    description: "Stop the sandbox daemon, destroy the runtime sandbox, and remove the host-side sandbox record.",
    args: DESTROY_SANDBOX_ARGS,
    cli: Some(DESTROY_SANDBOX_CLI),
    related: &["list_sandboxes", "inspect_sandbox"],
};

const DESTROY_SANDBOX_ARGS: &[ArgSpec] = &[ArgSpec::required(
    "sandbox_id",
    ArgKind::String,
    "Sandbox id.",
    Some(ArgCliSpec {
        flag: Some("--sandbox-id"),
        positional: None,
    }),
)];

const DESTROY_SANDBOX_CLI: CliSpec = CliSpec {
    path: &["manager", "destroy_sandbox"],
    usage: "sandbox-manager-cli destroy_sandbox --sandbox-id ID",
    examples: &["sandbox-manager-cli destroy_sandbox --sandbox-id sbox-1"],
};

pub const OBSERVABILITY_SNAPSHOT_SPEC: CliOperationSpec = CliOperationSpec {
    name: "snapshot",
    family: "management",
    summary: "Aggregate daemon observability snapshots for manager-known sandboxes.",
    description: "Aggregate daemon-local observability snapshots for ready manager-known sandboxes without reading daemon storage from the manager.",
    args: OBSERVABILITY_SNAPSHOT_ARGS,
    cli: None,
    related: &["list_sandboxes", "inspect_sandbox"],
};

const OBSERVABILITY_SNAPSHOT_ARGS: &[ArgSpec] = &[ArgSpec::optional(
    "sandbox_id",
    ArgKind::String,
    "Optional manager sandbox id. When omitted, all ready sandboxes with daemon endpoints are queried.",
    None,
    Some(ArgCliSpec {
        flag: Some("--sandbox-id"),
        positional: None,
    }),
)];

pub const LIST_SANDBOXES_SPEC: CliOperationSpec = CliOperationSpec {
    name: "list_sandboxes",
    family: "management",
    summary: "List sandbox records known to the manager.",
    description: "List sandbox records known to the manager, including lifecycle state and configured daemon endpoint metadata.",
    args: &[],
    cli: Some(LIST_SANDBOXES_CLI),
    related: &["inspect_sandbox", "create_sandbox"],
};

const LIST_SANDBOXES_CLI: CliSpec = CliSpec {
    path: &["manager", "list_sandboxes"],
    usage: "sandbox-manager-cli list_sandboxes",
    examples: &["sandbox-manager-cli list_sandboxes"],
};

pub const INSPECT_SANDBOX_SPEC: CliOperationSpec = CliOperationSpec {
    name: "inspect_sandbox",
    family: "management",
    summary: "Inspect one sandbox record.",
    description: "Inspect one sandbox record, including lifecycle state, workspace root, and configured daemon endpoint metadata.",
    args: INSPECT_SANDBOX_ARGS,
    cli: Some(INSPECT_SANDBOX_CLI),
    related: &["list_sandboxes"],
};

const INSPECT_SANDBOX_ARGS: &[ArgSpec] = &[ArgSpec::required(
    "sandbox_id",
    ArgKind::String,
    "Sandbox id.",
    Some(ArgCliSpec {
        flag: Some("--sandbox-id"),
        positional: None,
    }),
)];

const INSPECT_SANDBOX_CLI: CliSpec = CliSpec {
    path: &["manager", "inspect_sandbox"],
    usage: "sandbox-manager-cli inspect_sandbox --sandbox-id ID",
    examples: &["sandbox-manager-cli inspect_sandbox --sandbox-id sbox-1"],
};

pub const CHECKPOINT_SQUASH_SPEC: CliOperationSpec = CliOperationSpec {
    name: "checkpoint_squash",
    family: "management",
    summary: "Squash a sandbox's layer stack and live-remount its sessions.",
    description: "Squash every squashable block of the selected sandbox's published layers into equivalent flattened layers and migrate live workspace sessions onto the compact chains. Forwards one squash_layerstack request to the sandbox daemon.",
    args: CHECKPOINT_SQUASH_ARGS,
    cli: Some(CHECKPOINT_SQUASH_CLI),
    related: &["list_sandboxes", "inspect_sandbox", "export_changes"],
};

const CHECKPOINT_SQUASH_ARGS: &[ArgSpec] = &[ArgSpec::required(
    "sandbox_id",
    ArgKind::String,
    "Sandbox id.",
    Some(ArgCliSpec {
        flag: Some("--sandbox-id"),
        positional: None,
    }),
)];

const CHECKPOINT_SQUASH_CLI: CliSpec = CliSpec {
    path: &["manager", "checkpoint_squash"],
    usage: "sandbox-manager-cli checkpoint_squash --sandbox-id ID",
    examples: &["sandbox-manager-cli checkpoint_squash --sandbox-id sbox-1"],
};

pub const EXPORT_CHANGES_SPEC: CliOperationSpec = CliOperationSpec {
    name: "export_changes",
    family: "management",
    summary: "Export a sandbox's published changes to a host path.",
    description: "Fold every published layer above the base (newest-wins, \
                  whiteout/opaque aware) into a compressed delta stream, \
                  fetch it from the sandbox daemon, and apply it onto \
                  --dest or write it as an archive. Forwards \
                  export_layerstack and read_export_chunk requests to the \
                  sandbox daemon.",
    args: EXPORT_CHANGES_ARGS,
    cli: Some(CliSpec {
        path: &["manager", "export_changes"],
        usage: "sandbox-manager-cli export_changes --sandbox-id ID --dest PATH [--format dir|tar|tar-zst]",
        examples: &[
            "sandbox-manager-cli export_changes --sandbox-id sbox-1 --dest /home/me/myproject",
            "sandbox-manager-cli export_changes --sandbox-id sbox-1 --dest /tmp/delta.tar.zst --format tar-zst",
        ],
    }),
    related: &["inspect_sandbox", "checkpoint_squash"],
};

const EXPORT_CHANGES_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "sandbox_id",
        ArgKind::String,
        "Sandbox id.",
        Some(ArgCliSpec {
            flag: Some("--sandbox-id"),
            positional: None,
        }),
    ),
    ArgSpec::required(
        "dest",
        ArgKind::Path,
        "Absolute host destination: directory for dir format, archive file for tar formats.",
        Some(ArgCliSpec {
            flag: Some("--dest"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "format",
        ArgKind::String,
        "Output format: dir, tar, or tar-zst.",
        Some("dir"),
        Some(ArgCliSpec {
            flag: Some("--format"),
            positional: None,
        }),
    ),
];

const FAMILIES: &[&CliOperationFamilySpec] = &[&MANAGEMENT_FAMILY];

const SPECS: &[&CliOperationSpec] = &[
    &CREATE_SANDBOX_SPEC,
    &DESTROY_SANDBOX_SPEC,
    &LIST_SANDBOXES_SPEC,
    &INSPECT_SANDBOX_SPEC,
    &CHECKPOINT_SQUASH_SPEC,
    &EXPORT_CHANGES_SPEC,
];

#[must_use]
pub const fn cli_operation_families() -> &'static [&'static CliOperationFamilySpec] {
    FAMILIES
}

#[must_use]
pub const fn cli_operation_specs() -> &'static [&'static CliOperationSpec] {
    SPECS
}

#[must_use]
pub const fn manager_catalog() -> CliOperationCatalog {
    CliOperationCatalog::new(CliOperationExecutionSpace::Manager, FAMILIES, SPECS)
}
