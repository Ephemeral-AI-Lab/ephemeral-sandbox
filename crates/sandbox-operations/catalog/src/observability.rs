//! Observability operation declarations.
mod cgroup;
mod daemon;
mod events;
mod layerstack;
mod resources;
mod snapshot;
mod topology;
mod trace;

pub use cgroup::CGROUP_SPEC;
pub use daemon::DAEMON_SPEC;
pub use events::EVENTS_SPEC;
pub use layerstack::LAYERSTACK_SPEC;
pub use resources::RESOURCES_SPEC;
pub use snapshot::SNAPSHOT_SPEC;
pub use topology::TOPOLOGY_SPEC;
pub use trace::TRACE_SPEC;

use sandbox_operation_contract::{
    ArgKind, ArgSpec, OperationCatalog, OperationDomain, OperationFamilySpec, OperationRouteSpec,
    OperationSpec,
};

use crate::routed::{self, RoutedOperation};

const SNAPSHOT_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "snapshot",
    title: "Snapshot",
    summary: "Inspect current sandbox state.",
    description: "Read live sandbox state and aggregate ready manager-known sandboxes.",
};

const TRACE_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "trace",
    title: "Trace",
    summary: "Inspect a trace waterfall.",
    description: "Render one sandbox observability trace as a span waterfall.",
};

const EVENTS_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "events",
    title: "Events",
    summary: "Inspect domain-fact events.",
    description: "List sandbox observability events across traces.",
};

const CGROUP_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "cgroup",
    title: "Cgroup",
    summary: "Inspect resource series.",
    description: "Read sandbox CPU, memory, I/O, and workspace disk resource series.",
};

const RESOURCES_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "resources",
    title: "Resources",
    summary: "Inspect bounded resource metrics.",
    description: "Read daemon-owned sandbox history or one manager-owned fleet current-usage map.",
};

const DAEMON_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "daemon",
    title: "Daemon",
    summary: "Inspect daemon self-metrics.",
    description: "Read one bounded daemon process and ownership sample without collecting workspace process topology.",
};

const TOPOLOGY_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "topology",
    title: "Topology",
    summary: "Inspect workspace process topology.",
    description: "Perform one explicit, bounded namespace topology collection in a sandbox daemon.",
};

const LAYERSTACK_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "layerstack",
    title: "Layerstack",
    summary: "Inspect layerstack inventory.",
    description: "Read live layerstack leasing, booking, and stack-series state.",
};

const RESOURCE_ISOLATION_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "resource_isolation",
    title: "Resource isolation",
    summary: "Qualify observability memory and storage isolation.",
    description: "Verify bounded resource use and read-only observability behavior.",
};

const RESOURCE_EFFICIENCY_FAMILY: OperationFamilySpec = OperationFamilySpec {
    id: "resource_efficiency",
    title: "Resource efficiency",
    summary: "Qualify sandbox resource efficiency.",
    description: "Verify holder lifecycle, bounded runtime ownership, manager-only resource routing, workload containment, and diagnostic evidence.",
};

pub(crate) const SANDBOX_ID_ARG: ArgSpec = ArgSpec::required(
    "sandbox_id",
    ArgKind::String,
    "Target sandbox id (selects the daemon to query).",
);

const FAMILIES: &[&OperationFamilySpec] = &[
    &SNAPSHOT_FAMILY,
    &TRACE_FAMILY,
    &EVENTS_FAMILY,
    &RESOURCES_FAMILY,
    &DAEMON_FAMILY,
    &TOPOLOGY_FAMILY,
    &CGROUP_FAMILY,
    &LAYERSTACK_FAMILY,
    &RESOURCE_ISOLATION_FAMILY,
    &RESOURCE_EFFICIENCY_FAMILY,
];

const OPERATIONS: &[&RoutedOperation] = &[
    &snapshot::SNAPSHOT,
    &trace::TRACE,
    &events::EVENTS,
    &resources::RESOURCES,
    &daemon::DAEMON,
    &topology::TOPOLOGY,
    &cgroup::CGROUP,
    &layerstack::LAYERSTACK,
];

const SPECS: [&OperationSpec; OPERATIONS.len()] = routed::specs(OPERATIONS);
const ROUTES: [OperationRouteSpec; routed::route_count(OPERATIONS)] =
    routed::expand_routes(OPERATIONS);

pub(crate) const fn routes() -> &'static [OperationRouteSpec] {
    &ROUTES
}

#[must_use]
pub const fn observability_catalog() -> OperationCatalog {
    OperationCatalog::new(OperationDomain::Observability, FAMILIES, &SPECS, &ROUTES)
}
