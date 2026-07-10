pub(crate) mod command_operations;
pub(crate) mod file_operations;
pub(crate) mod workspace_session_operations;

use super::dispatch::OperationEntry;

const PUBLIC_OPERATION_ENTRY_GROUPS: &[&[OperationEntry]] = &[
    command_operations::public_operation_entries(),
    file_operations::public_operation_entries(),
];

const INTERNAL_OPERATION_ENTRY_GROUPS: &[&[OperationEntry]] = &[
    workspace_session_operations::internal_operation_entries(),
    crate::layerstack::squash_operation_entries(),
    crate::layerstack::export_operation_entries(),
];

const HTTP_ONLY_OPERATION_ENTRY_GROUPS: &[&[OperationEntry]] =
    &[file_operations::http_only_operation_entries()];

pub(crate) fn public_operation_entries() -> impl Iterator<Item = &'static OperationEntry> {
    PUBLIC_OPERATION_ENTRY_GROUPS.iter().copied().flatten()
}

pub(crate) fn internal_operation_entries() -> impl Iterator<Item = &'static OperationEntry> {
    INTERNAL_OPERATION_ENTRY_GROUPS.iter().copied().flatten()
}

pub(crate) fn http_only_operation_entries() -> impl Iterator<Item = &'static OperationEntry> {
    HTTP_ONLY_OPERATION_ENTRY_GROUPS.iter().copied().flatten()
}
