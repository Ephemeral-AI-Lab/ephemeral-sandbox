pub mod command;

use std::time::Instant;

use crate::internal::services::SandboxRuntimeOperations;
use crate::operation::{CliOperationFamilySpec, CliOperationSpec};
use crate::workspace_crate::{RuntimeMetricStatus, RuntimeOperationName};

pub(crate) fn cli_operation_families() -> &'static [&'static CliOperationFamilySpec] {
    command::cli_operation_families()
}

pub(crate) fn cli_operation_specs() -> &'static [&'static CliOperationSpec] {
    command::cli_operation_specs()
}

pub(crate) fn dispatch_operation(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    command::operation_entries()
        .iter()
        .find(|entry| entry.spec.name == request.op)
        .map_or_else(sandbox_protocol::Response::unknown_op, |entry| {
            let started = Instant::now();
            let response = (entry.dispatch)(operations, request);
            if let Some(operation) = RuntimeOperationName::from_static_name(entry.spec.name) {
                operations.metrics().record_runtime_latency(
                    operation,
                    response_status(&response),
                    started.elapsed(),
                );
            }
            response
        })
}

fn response_status(response: &sandbox_protocol::Response) -> RuntimeMetricStatus {
    let value = response.clone().into_json_value();
    if value.get("error").is_some() {
        RuntimeMetricStatus::Error
    } else {
        RuntimeMetricStatus::Ok
    }
}
