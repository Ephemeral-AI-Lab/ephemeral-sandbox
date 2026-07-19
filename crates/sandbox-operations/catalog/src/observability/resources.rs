use sandbox_operation_contract::{ArgKind, ArgSpec, OperationExecutionOwner, OperationSpec};

use crate::routed::{RoutedOperation, Routing};

pub const RESOURCES: RoutedOperation = RoutedOperation {
    spec: &RESOURCES_SPEC,
    routing: Routing::SystemOrSandbox {
        system: OperationExecutionOwner::Manager,
        sandbox: OperationExecutionOwner::Observability,
    },
};

const RESOURCES_SANDBOX_ID_ARG: ArgSpec = ArgSpec::optional(
    "sandbox_id",
    ArgKind::String,
    "Optional target sandbox id. When omitted, return one current record for every ready sandbox.",
    None,
);

pub static RESOURCES_SPEC: OperationSpec = OperationSpec {
    name: "resources",
    family: "resources",
    summary: "Read bounded sandbox or fleet resource metrics.",
    description: "A sandbox request reads daemon-sampled cgroup metrics from its bounded on-disk store. A system request remains manager-owned and returns one current record keyed by every ready sandbox id.",
    args: &[
        RESOURCES_SANDBOX_ID_ARG,
        ArgSpec::optional(
            "window_ms",
            ArgKind::Integer,
            "Sandbox-history lookback in milliseconds (max 600000); ignored by the fleet-current form.",
            Some("60000"),
        ),
    ],
    related: &["topology", "cgroup"],
};
