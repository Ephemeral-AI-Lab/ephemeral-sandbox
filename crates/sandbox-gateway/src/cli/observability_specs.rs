//! CLI operation specs for the read-only `observability` execution space.
//!
//! Every operation resolves to the single daemon op `get_observability`; the
//! operation name is the `view` value and the flags map to that op's params (see
//! `request_builder`). One `CliOperationSpec` per view gives each its own
//! subcommand, help page, and args.

use sandbox_protocol::{
    ArgCliSpec, ArgKind, ArgSpec, CliOperationCatalog, CliOperationExecutionSpace,
    CliOperationFamilySpec, CliOperationSpec, CliSpec,
};

pub const OBSERVABILITY_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "observability",
    title: "Observability",
    summary: "Inspect traces, events, and resource stats for a sandbox.",
    description: "Read a sandbox's observability stream — span waterfalls, domain \
events, cgroup/disk resource series, and live state, over the daemon \
get_observability op.",
};

const SANDBOX_ID_ARG: ArgSpec = ArgSpec::required(
    "sandbox_id",
    ArgKind::String,
    "Target sandbox id (selects the daemon to query).",
    Some(ArgCliSpec {
        flag: Some("--sandbox-id"),
        positional: None,
    }),
);

const LAYERSTACK_SPEC: CliOperationSpec = CliOperationSpec {
    name: "layerstack",
    family: "observability",
    summary: "Per-layer leasing/booking inventory, and stack series.",
    description: "Show the active manifest as a per-layer inventory: disk bytes, \
how many workspaces lease each layer, and which leased layers book each base. \
Served live from the runtime; does not read the log.",
    args: &[
        SANDBOX_ID_ARG,
        ArgSpec::optional(
            "workspace",
            ArgKind::String,
            "Restrict to one workspace/session: its mounted layers and which other sessions share each.",
            None,
            Some(ArgCliSpec {
                flag: Some("--workspace"),
                positional: None,
            }),
        ),
        ArgSpec::optional(
            "window_ms",
            ArgKind::Integer,
            "Lookback window in milliseconds for the stack trend (max 600000).",
            Some("60000"),
            Some(ArgCliSpec {
                flag: Some("--window-ms"),
                positional: None,
            }),
        ),
    ],
    cli: Some(CliSpec {
        path: &["observability", "layerstack"],
        usage: "sandbox-cli observability layerstack --sandbox-id ID [--workspace WS] [--window-ms MS]",
        examples: &[
            "sandbox-cli observability layerstack --sandbox-id eos-abc",
            "sandbox-cli observability layerstack --sandbox-id eos-abc --workspace ws-7",
        ],
    }),
    related: &["snapshot", "cgroup"],
};

const SNAPSHOT_SPEC: CliOperationSpec = CliOperationSpec {
    name: "snapshot",
    family: "observability",
    summary: "Show live sandbox state.",
    description: "Show current state from the runtime registry: sandbox lifecycle \
state, workspaces (with layer counts), in-flight executions, the latest resource \
sample per scope, and the layer-stack summary line. Served live; does not read \
the log.",
    args: &[SANDBOX_ID_ARG],
    cli: Some(CliSpec {
        path: &["observability", "snapshot"],
        usage: "sandbox-cli observability snapshot --sandbox-id ID",
        examples: &["sandbox-cli observability snapshot --sandbox-id eos-abc"],
    }),
    related: &["layerstack", "cgroup"],
};

const CGROUP_SPEC: CliOperationSpec = CliOperationSpec {
    name: "cgroup",
    family: "observability",
    summary: "Resource series for a scope (cpu/mem/io + disk).",
    description: "Resource time series for one scope: cgroup cpu/mem/io counters \
plus the disk sample (upperdir bytes/files), with deltas computed at read.",
    args: &[
        SANDBOX_ID_ARG,
        ArgSpec::optional(
            "scope",
            ArgKind::String,
            "Resource scope: 'sandbox' or a workspace id.",
            Some("sandbox"),
            Some(ArgCliSpec {
                flag: Some("--scope"),
                positional: None,
            }),
        ),
        ArgSpec::optional(
            "window_ms",
            ArgKind::Integer,
            "Lookback window in milliseconds (max 600000).",
            Some("60000"),
            Some(ArgCliSpec {
                flag: Some("--window-ms"),
                positional: None,
            }),
        ),
    ],
    cli: Some(CliSpec {
        path: &["observability", "cgroup"],
        usage: "sandbox-cli observability cgroup --sandbox-id ID [--scope SCOPE] [--window-ms MS]",
        examples: &[
            "sandbox-cli observability cgroup --sandbox-id eos-abc",
            "sandbox-cli observability cgroup --sandbox-id eos-abc --scope ws-1 --window-ms 60000",
        ],
    }),
    related: &["snapshot"],
};

const FAMILIES: &[&CliOperationFamilySpec] = &[&OBSERVABILITY_FAMILY];
const SPECS: &[&CliOperationSpec] = &[&LAYERSTACK_SPEC, &SNAPSHOT_SPEC, &CGROUP_SPEC];

#[must_use]
pub fn observability_catalog() -> CliOperationCatalog {
    CliOperationCatalog::new(CliOperationExecutionSpace::Observability, FAMILIES, SPECS)
}
