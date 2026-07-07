use std::sync::OnceLock;

use crate::cli_definition::{
    command_operations, file_operations, workspace_session_operations, CliOperationFamilySpec,
    CliOperationSpec,
};
use crate::services::SandboxRuntimeOperations;

#[derive(Clone, Copy)]
pub(crate) struct OperationEntry {
    pub(crate) name: &'static str,
    pub(crate) cli: Option<&'static CliOperationSpec>,
    pub(crate) dispatch: OperationDispatch,
}

type OperationDispatch =
    fn(&SandboxRuntimeOperations, &sandbox_protocol::Request) -> sandbox_protocol::Response;

impl OperationEntry {
    #[must_use]
    pub(crate) const fn cli(spec: &'static CliOperationSpec, dispatch: OperationDispatch) -> Self {
        Self {
            name: spec.name,
            cli: Some(spec),
            dispatch,
        }
    }

    #[must_use]
    const fn cli_spec(self) -> Option<&'static CliOperationSpec> {
        self.cli
    }
}

const CLI_FAMILIES: &[&CliOperationFamilySpec] = &[
    &sandbox_runtime_operations::COMMAND_FAMILY,
    &sandbox_runtime_operations::FILE_FAMILY,
    &sandbox_runtime_operations::WORKSPACE_SESSION_FAMILY,
];
static CLI_SPECS: OnceLock<&'static [&'static CliOperationSpec]> = OnceLock::new();

pub(crate) fn cli_operation_families() -> &'static [&'static CliOperationFamilySpec] {
    CLI_FAMILIES
}

pub(crate) fn cli_operation_specs() -> &'static [&'static CliOperationSpec] {
    CLI_SPECS.get_or_init(|| {
        Box::leak(
            operation_entry_groups()
                .iter()
                .flat_map(|entries| entries.iter())
                .filter_map(|entry| entry.cli_spec())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    })
}

pub(crate) fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    operation_entry_groups()
        .iter()
        .flat_map(|entries| entries.iter())
        .find(|entry| entry.name == request.op)
        .map_or_else(sandbox_protocol::Response::unknown_op, |entry| {
            (entry.dispatch)(operations, request)
        })
}

pub(crate) fn known_operation_name(operation: &str) -> Option<&'static str> {
    operation_entry_groups()
        .iter()
        .flat_map(|entries| entries.iter())
        .find_map(|entry| (entry.name == operation).then_some(entry.name))
}

const OPERATION_ENTRY_GROUPS: &[&[OperationEntry]] = &[
    command_operations::operation_entries(),
    file_operations::operation_entries(),
    workspace_session_operations::operation_entries(),
    crate::layerstack::squash_operation_entries(),
    crate::layerstack::export_operation_entries(),
];

fn operation_entry_groups() -> &'static [&'static [OperationEntry]] {
    OPERATION_ENTRY_GROUPS
}
