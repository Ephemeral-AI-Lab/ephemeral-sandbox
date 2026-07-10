use super::registry;
use crate::services::SandboxRuntimeOperations;
use sandbox_operation_contract::{OperationScopeKind, OperationSpec};

#[derive(Clone, Copy)]
pub(crate) struct OperationEntry {
    pub(crate) scope_kind: OperationScopeKind,
    pub(crate) name: &'static str,
    pub(crate) spec: Option<&'static OperationSpec>,
    pub(crate) dispatch: OperationDispatch,
}

type OperationDispatch = fn(
    &SandboxRuntimeOperations,
    &sandbox_operation_contract::OperationRequest,
) -> sandbox_operation_contract::OperationResponse;

impl OperationEntry {
    #[must_use]
    pub(crate) const fn public(spec: &'static OperationSpec, dispatch: OperationDispatch) -> Self {
        Self {
            scope_kind: OperationScopeKind::Sandbox,
            name: spec.name,
            spec: Some(spec),
            dispatch,
        }
    }
}

pub(crate) fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_operation_contract::OperationRequest,
) -> sandbox_operation_contract::OperationResponse {
    operation_entries()
        .find(|entry| entry.scope_kind == request.scope.kind() && entry.name == request.op)
        .map_or_else(
            sandbox_operation_contract::OperationResponse::unknown_op,
            |entry| {
                debug_assert!(entry.spec.is_none_or(|spec| spec.name == entry.name));
                (entry.dispatch)(operations, request)
            },
        )
}

pub(crate) fn runtime_public_handler_keys(
) -> impl Iterator<Item = (OperationScopeKind, &'static str)> {
    registry::public_operation_entries().map(|entry| (entry.scope_kind, entry.name))
}

pub(crate) fn runtime_internal_handler_keys(
) -> impl Iterator<Item = (OperationScopeKind, &'static str)> {
    registry::internal_operation_entries().map(|entry| (entry.scope_kind, entry.name))
}

pub(crate) fn runtime_http_only_handler_keys(
) -> impl Iterator<Item = (OperationScopeKind, &'static str)> {
    registry::http_only_operation_entries().map(|entry| (entry.scope_kind, entry.name))
}

fn operation_entries() -> impl Iterator<Item = &'static OperationEntry> {
    registry::public_operation_entries()
        .chain(registry::internal_operation_entries())
        .chain(registry::http_only_operation_entries())
}
