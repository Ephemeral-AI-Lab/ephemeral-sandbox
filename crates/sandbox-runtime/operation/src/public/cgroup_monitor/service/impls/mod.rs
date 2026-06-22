mod inspect_cgroup_monitor;
mod read_cgroup_monitor_samples;

use serde_json::{json, Value};

use crate::cgroup_monitor::{
    CgroupMonitorServiceError, InspectCgroupMonitorOutput, ReadCgroupMonitorSamplesOutput,
};
use crate::command::CommandSessionId;
use crate::operation::{CliOperationSpec, OperationEntry};
use crate::workspace_crate::WorkspaceSessionId;
use sandbox_protocol::Response;

pub(crate) const OPERATIONS: &[OperationEntry] = &[
    OperationEntry::new(
        &inspect_cgroup_monitor::SPEC,
        inspect_cgroup_monitor::dispatch,
    ),
    OperationEntry::new(
        &read_cgroup_monitor_samples::SPEC,
        read_cgroup_monitor_samples::dispatch,
    ),
];

pub(crate) const SPECS: &[&CliOperationSpec] = &[
    &inspect_cgroup_monitor::SPEC,
    &read_cgroup_monitor_samples::SPEC,
];

pub(super) fn inspect_response(
    result: Result<InspectCgroupMonitorOutput, CgroupMonitorServiceError>,
) -> Response {
    match result {
        Ok(output) => Response::ok(inspect_value(output)),
        Err(error) => cgroup_monitor_error_response(error),
    }
}

pub(super) fn samples_response(
    result: Result<ReadCgroupMonitorSamplesOutput, CgroupMonitorServiceError>,
) -> Response {
    match result {
        Ok(output) => Response::ok(samples_value(output)),
        Err(error) => cgroup_monitor_error_response(error),
    }
}

fn cgroup_monitor_error_response(error: CgroupMonitorServiceError) -> Response {
    Response::fault_with_details(
        "operation_failed",
        error.to_string(),
        cgroup_monitor_error_details(&error),
    )
}

fn cgroup_monitor_error_details(error: &CgroupMonitorServiceError) -> Value {
    match error {
        CgroupMonitorServiceError::SessionTargetNotFound {
            workspace_session_id,
        } => json!({
            "workspace_session_id": workspace_session_id.0,
        }),
        CgroupMonitorServiceError::CommandTargetNotFound {
            workspace_session_id,
            command_session_id,
        } => json!({
            "workspace_session_id": workspace_session_id.0,
            "command_session_id": command_session_id.0,
        }),
        _ => json!({}),
    }
}

fn inspect_value(output: InspectCgroupMonitorOutput) -> Value {
    json!({
        "workspace_session_id": output.workspace_session_id.0,
        "command_session_id": output.command_session_id.map(|id| id.0),
        "target": output.target,
        "monitor": output.monitor,
        "latest": output.latest,
        "cleanup": output.cleanup,
    })
}

fn samples_value(output: ReadCgroupMonitorSamplesOutput) -> Value {
    json!({
        "workspace_session_id": output.workspace_session_id.0,
        "command_session_id": output.command_session_id.map(|id| id.0),
        "target": output.target,
        "samples": output.samples,
    })
}

fn validate_ids(
    workspace_session_id: &WorkspaceSessionId,
    command_session_id: Option<&CommandSessionId>,
) -> Result<(), CgroupMonitorServiceError> {
    if workspace_session_id.0.trim().is_empty() {
        return Err(CgroupMonitorServiceError::InvalidInput {
            message: "workspace_session_id must be non-empty".to_owned(),
        });
    }
    if command_session_id.is_some_and(|id| id.0.trim().is_empty()) {
        return Err(CgroupMonitorServiceError::InvalidInput {
            message: "command_session_id must be non-empty".to_owned(),
        });
    }
    Ok(())
}
