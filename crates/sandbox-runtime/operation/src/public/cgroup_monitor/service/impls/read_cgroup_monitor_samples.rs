use super::{samples_response, validate_ids};
use crate::cgroup_monitor::{
    CgroupMonitorOperationService, CgroupMonitorServiceError, ReadCgroupMonitorSamplesInput,
    ReadCgroupMonitorSamplesOutput,
};
use crate::command::CommandSessionId;
use crate::operation::{ArgCliSpec, ArgKind, ArgSpec, CliOperationSpec, CliSpec};
use crate::workspace_crate::WorkspaceSessionId;
use crate::SandboxRuntimeOperations;
use sandbox_protocol::{Request, Response};

pub(crate) const SPEC: CliOperationSpec = CliOperationSpec {
    name: "read_cgroup_monitor_samples",
    family: "cgroup_monitor",
    summary: "Read retained cgroup monitor samples.",
    description:
        "Read the retained cgroup monitor sample window for a workspace session or command session.",
    args: READ_CGROUP_MONITOR_SAMPLES_ARGS,
    cli: Some(READ_CGROUP_MONITOR_SAMPLES_CLI),
    related: &["inspect_cgroup_monitor"],
};

const READ_CGROUP_MONITOR_SAMPLES_ARGS: &[ArgSpec] = &[
    ArgSpec::required(
        "workspace_session_id",
        ArgKind::String,
        "Workspace session id to read.",
        Some(ArgCliSpec {
            flag: Some("--workspace-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "command_session_id",
        ArgKind::String,
        "Command session id to read under the workspace session.",
        None,
        Some(ArgCliSpec {
            flag: Some("--command-session-id"),
            positional: None,
        }),
    ),
    ArgSpec::optional(
        "limit",
        ArgKind::Integer,
        "Maximum retained samples to return.",
        None,
        Some(ArgCliSpec {
            flag: Some("--limit"),
            positional: None,
        }),
    ),
];

const READ_CGROUP_MONITOR_SAMPLES_CLI: CliSpec = CliSpec {
    path: &["runtime", "read_cgroup_monitor_samples"],
    usage: "sandbox-cli runtime read_cgroup_monitor_samples --workspace-session-id ID [--command-session-id CMD] [--limit N]",
    examples: &[
        "sandbox-cli runtime read_cgroup_monitor_samples --workspace-session-id ws-1 --limit 100",
        "sandbox-cli runtime read_cgroup_monitor_samples --workspace-session-id ws-1 --command-session-id cmd-1 --limit 50",
    ],
};

pub(crate) fn dispatch(operations: &SandboxRuntimeOperations, request: &Request) -> Response {
    let input = match parse_input(request) {
        Ok(input) => input,
        Err(response) => return response,
    };
    samples_response(operations.cgroup_monitor.read_cgroup_monitor_samples(input))
}

fn parse_input(request: &Request) -> Result<ReadCgroupMonitorSamplesInput, Response> {
    Ok(ReadCgroupMonitorSamplesInput {
        workspace_session_id: WorkspaceSessionId(request.required_string("workspace_session_id")?),
        command_session_id: request
            .optional_string("command_session_id")?
            .map(CommandSessionId),
        limit: request.optional_usize("limit")?,
    })
}

impl CgroupMonitorOperationService {
    pub fn read_cgroup_monitor_samples(
        &self,
        input: ReadCgroupMonitorSamplesInput,
    ) -> Result<ReadCgroupMonitorSamplesOutput, CgroupMonitorServiceError> {
        validate_ids(
            &input.workspace_session_id,
            input.command_session_id.as_ref(),
        )?;
        if input.limit == Some(0) {
            return Err(CgroupMonitorServiceError::InvalidInput {
                message: "limit must be greater than zero".to_owned(),
            });
        }
        self.ensure_target_scope_available(
            &input.workspace_session_id,
            input.command_session_id.as_ref(),
        )?;
        let limit = input
            .limit
            .unwrap_or_else(|| self.registry().config().retained_samples_per_target);
        let window = self
            .registry()
            .read_samples(
                &input.workspace_session_id,
                input.command_session_id.as_ref().map(|id| id.0.as_str()),
                limit,
            )
            .ok_or_else(|| target_not_found(&input))?;
        Ok(ReadCgroupMonitorSamplesOutput {
            workspace_session_id: input.workspace_session_id,
            command_session_id: input.command_session_id,
            target: window.target,
            samples: window.samples,
        })
    }
}

fn target_not_found(input: &ReadCgroupMonitorSamplesInput) -> CgroupMonitorServiceError {
    match input.command_session_id.clone() {
        Some(command_session_id) => CgroupMonitorServiceError::CommandTargetNotFound {
            workspace_session_id: input.workspace_session_id.clone(),
            command_session_id,
        },
        None => CgroupMonitorServiceError::SessionTargetNotFound {
            workspace_session_id: input.workspace_session_id.clone(),
        },
    }
}
