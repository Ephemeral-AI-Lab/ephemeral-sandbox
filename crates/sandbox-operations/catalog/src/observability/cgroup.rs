use sandbox_operation_contract::{ArgKind, ArgSpec, OperationExecutionOwner, OperationSpec};

use super::SANDBOX_ID_ARG;
use crate::routed::{RoutedOperation, Routing};

pub const CGROUP: RoutedOperation = RoutedOperation {
    spec: &CGROUP_SPEC,
    routing: Routing::Sandbox(OperationExecutionOwner::Manager),
};

pub static CGROUP_SPEC: OperationSpec = OperationSpec {
    name: "cgroup",
    family: "cgroup",
    summary: "Sandbox resource series and workspace process topology.",
    description: "For explicit sandbox requests, merge manager-owned host resource series with \
daemon-owned proc namespace topology. Normal resource polling remains daemon-independent, and \
the operation never mutates cgroups.",
    args: &[
        SANDBOX_ID_ARG,
        ArgSpec::optional(
            "scope",
            ArgKind::String,
            "Resource scope: 'sandbox' or a workspace id.",
            Some("sandbox"),
        ),
        ArgSpec::optional(
            "window_ms",
            ArgKind::Integer,
            "Lookback window in milliseconds (max 600000).",
            Some("60000"),
        ),
    ],
    related: &["snapshot"],
};
