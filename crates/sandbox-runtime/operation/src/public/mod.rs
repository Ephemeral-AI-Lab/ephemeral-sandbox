pub mod cgroup_monitor;
pub mod command;

use std::sync::OnceLock;

use crate::internal::services::SandboxRuntimeOperations;
use crate::operation::{CliOperationSpec, OperationFamilySpec};

pub(crate) fn operation_families() -> &'static [&'static OperationFamilySpec] {
    static FAMILIES: OnceLock<Box<[&'static OperationFamilySpec]>> = OnceLock::new();
    FAMILIES
        .get_or_init(|| {
            command::operation_families()
                .iter()
                .chain(cgroup_monitor::operation_families().iter())
                .copied()
                .collect::<Vec<_>>()
                .into_boxed_slice()
        })
        .as_ref()
}

pub(crate) fn operation_specs() -> &'static [&'static CliOperationSpec] {
    static SPECS: OnceLock<Box<[&'static CliOperationSpec]>> = OnceLock::new();
    SPECS
        .get_or_init(|| {
            command::operation_specs()
                .iter()
                .chain(cgroup_monitor::operation_specs().iter())
                .copied()
                .collect::<Vec<_>>()
                .into_boxed_slice()
        })
        .as_ref()
}

pub(crate) fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    command::operation_entries()
        .iter()
        .chain(cgroup_monitor::operation_entries().iter())
        .find(|entry| entry.spec.name == request.op)
        .map_or_else(sandbox_protocol::Response::unknown_op, |entry| {
            (entry.dispatch)(operations, request)
        })
}
