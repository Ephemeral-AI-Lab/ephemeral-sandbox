use std::sync::OnceLock;

use crate::cli_definition::{
    command_operations, layerstack_operations, workspace_session_operations,
    CliOperationFamilySpec, CliOperationSpec,
};
use crate::observability::{measure_optional, OperationTrace};
use crate::services::SandboxRuntimeOperations;

#[derive(Clone, Copy)]
pub(crate) struct OperationEntry {
    pub(crate) name: &'static str,
    pub(crate) cli: Option<&'static CliOperationSpec>,
    pub(crate) dispatch: OperationDispatch,
}

type OperationDispatch = fn(
    &SandboxRuntimeOperations,
    &sandbox_protocol::Request,
    Option<&OperationTrace>,
) -> sandbox_protocol::Response;

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
    &command_operations::COMMAND_FAMILY,
    &workspace_session_operations::WORKSPACE_SESSION_FAMILY,
    &layerstack_operations::LAYERSTACK_FAMILY,
];
static CLI_SPECS: OnceLock<&'static [&'static CliOperationSpec]> = OnceLock::new();

pub(crate) fn cli_operation_families() -> &'static [&'static CliOperationFamilySpec] {
    CLI_FAMILIES
}

pub(crate) fn cli_operation_specs() -> &'static [&'static CliOperationSpec] {
    CLI_SPECS.get_or_init(|| {
        Box::leak(
            operation_entry_groups()
                .into_iter()
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
    trace: Option<&OperationTrace>,
) -> sandbox_protocol::Response {
    measure_optional(trace, "dispatch_operation", || {
        operation_entry_groups()
            .into_iter()
            .flat_map(|entries| entries.iter())
            .find(|entry| entry.name == request.op)
            .map_or_else(sandbox_protocol::Response::unknown_op, |entry| {
                measure_optional(trace, operation_dispatch_span(entry.name), || {
                    (entry.dispatch)(operations, request, trace)
                })
            })
    })
}

pub(crate) fn known_operation_name(operation: &str) -> Option<&'static str> {
    operation_entry_groups()
        .into_iter()
        .flat_map(|entries| entries.iter())
        .find_map(|entry| (entry.name == operation).then_some(entry.name))
}

fn operation_entry_groups() -> [&'static [OperationEntry]; 3] {
    [
        command_operations::operation_entries(),
        workspace_session_operations::operation_entries(),
        layerstack_operations::operation_entries(),
    ]
}

fn operation_dispatch_span(operation: &str) -> &'static str {
    match operation {
        "exec_command" => "exec_command::dispatch",
        "write_command_stdin" => "write_command_stdin::dispatch",
        "read_command_lines" => "read_command_lines::dispatch",
        "create_workspace_session" => "create_workspace_session::dispatch",
        "destroy_workspace_session" => "destroy_workspace_session::dispatch",
        "squash" => "squash::dispatch",
        _ => "operation::dispatch",
    }
}
