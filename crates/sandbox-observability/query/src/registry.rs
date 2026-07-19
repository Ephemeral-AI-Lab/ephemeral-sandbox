use sandbox_operation_catalog::observability::{
    CGROUP_SPEC, DAEMON_SPEC, EVENTS_SPEC, LAYERSTACK_SPEC, RESOURCES_SPEC, SNAPSHOT_SPEC,
    TOPOLOGY_SPEC, TRACE_SPEC,
};
use sandbox_operation_contract::{
    OperationRequest, OperationResponse, OperationScopeKind, OperationSpec,
};

use crate::ports::ObservabilityInput;
use crate::query;

type Handler = fn(&dyn ObservabilityInput, &OperationRequest) -> OperationResponse;

struct OperationEntry {
    scope_kind: OperationScopeKind,
    spec: &'static OperationSpec,
    handler: Handler,
}

const OPERATIONS: &[OperationEntry] = &[
    OperationEntry::new(&SNAPSHOT_SPEC, query::snapshot),
    OperationEntry::new(&TRACE_SPEC, query::trace),
    OperationEntry::new(&EVENTS_SPEC, query::events),
    OperationEntry::new(&CGROUP_SPEC, query::cgroup),
    OperationEntry::new(&RESOURCES_SPEC, query::resources),
    OperationEntry::new(&TOPOLOGY_SPEC, query::topology),
    OperationEntry::new(&DAEMON_SPEC, query::daemon),
    OperationEntry::new(&LAYERSTACK_SPEC, query::layerstack),
];

impl OperationEntry {
    const fn new(spec: &'static OperationSpec, handler: Handler) -> Self {
        Self {
            scope_kind: OperationScopeKind::Sandbox,
            spec,
            handler,
        }
    }
}

pub fn dispatch_operation(
    input: &dyn ObservabilityInput,
    request: &OperationRequest,
) -> OperationResponse {
    OPERATIONS
        .iter()
        .find(|entry| entry.scope_kind == request.scope.kind() && entry.spec.name == request.op)
        .map_or_else(OperationResponse::unknown_op, |entry| {
            (entry.handler)(input, request)
        })
}

pub fn observability_handler_keys() -> impl Iterator<Item = (OperationScopeKind, &'static str)> {
    OPERATIONS
        .iter()
        .map(|entry| (entry.scope_kind, entry.spec.name))
}
