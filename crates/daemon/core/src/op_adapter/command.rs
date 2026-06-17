//! Command dispatcher handlers, driving the caller-keyed
//! command runtime in `operation::command`.

use std::path::PathBuf;

use command::{
    CancelCommand, CollectCompleted, CommandConfig, CommandError, ReadCommandProgress,
    StartCommand, WriteStdin,
};
use operation::command::contract::{
    CancelCommandInput, CollectCompletedInput, CommandCountOutput, CommandResponse, CommandStatus,
    ExecCommandInput, ReadProgressInput, WriteStdinInput,
};
use operation::command::{
    CommandExecError, CommandExecOutcome, CommandOps, CommandProgressTraceFacts,
    CommandStdinTraceFacts, CommandTraceEvent,
};
use operation::control::contract::CallerCountInput;
use serde_json::{json, Value};
use thiserror::Error;

use crate::error::DaemonError;
use crate::response::u64_to_f64_saturating;
use crate::runtime::workspace_runtime::WorkspaceRouteTraceFacts;
use crate::{DispatchContext, WorkspaceRuntime};

use super::to_wire_value;

/// Typed command start request after daemon JSON parsing.
struct ExecCommandRequest {
    invocation_id: String,
    caller_id: String,
    cmd: String,
    trace_id: Option<String>,
    request_id: Option<String>,
    layer_stack_root: Option<PathBuf>,
    timeout_seconds: Option<f64>,
    yield_time_ms: u64,
    cwd: Option<PathBuf>,
    remountable: bool,
}

/// Errors from routing or starting a workspace-bound command.
#[derive(Debug, Error)]
enum CommandOpError {
    #[error("layer_stack_root is required")]
    MissingLayerStackRoot,
    #[error(transparent)]
    Workspace(#[from] workspace::WorkspaceError),
    #[error(transparent)]
    Command(#[from] CommandExecError),
}

impl CommandOpError {
    fn from_workspace(error: workspace::WorkspaceError) -> Self {
        if is_missing_layer_stack_root(&error) {
            Self::MissingLayerStackRoot
        } else {
            Self::Workspace(error)
        }
    }
}

/// `sandbox.command.exec` - command start contract.
pub(crate) fn op_exec_command(
    input: ExecCommandInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let services = context.require_services()?;
    let command_ops = &services.command;
    let command_config = command_ops.config();
    let timeout_seconds = Some(exec_timeout_seconds(&input, command_config));
    let yield_time_ms = input
        .yield_time_ms
        .unwrap_or(command_config.default_yield_time_ms);
    let outcome = match exec_command(
        &context,
        command_ops,
        &services.workspace,
        ExecCommandRequest {
            invocation_id: input.invocation_id.to_string(),
            caller_id: input.caller.to_string(),
            cmd: input.cmd,
            trace_id: context.trace_id().map(str::to_owned),
            request_id: context.request_id().map(str::to_owned),
            layer_stack_root: input.layer_stack_root,
            timeout_seconds,
            yield_time_ms,
            cwd: input.cwd,
            remountable: input.remountable,
        },
    ) {
        Ok(outcome) => outcome,
        Err(CommandOpError::Command(error)) => {
            record_command_trace_events(&context, error.trace_events());
            return Err(command_error(error.into_error()));
        }
        Err(error) => return Err(command_op_error(error)),
    };
    record_command_trace_events(&context, &outcome.trace_events);
    let response = outcome.response;
    let running = response.status == CommandStatus::Running;
    let wire = response.to_wire_value();
    if running {
        Ok(wire)
    } else {
        Ok(strip_command_id(wire))
    }
}

fn exec_timeout_seconds(input: &ExecCommandInput, config: &CommandConfig) -> f64 {
    u64_to_f64_saturating(input.timeout.unwrap_or(config.default_timeout_s))
}

fn exec_command(
    context: &DispatchContext<'_>,
    command_ops: &CommandOps,
    workspace: &WorkspaceRuntime,
    request: ExecCommandRequest,
) -> Result<CommandExecOutcome, CommandOpError> {
    let ExecCommandRequest {
        invocation_id,
        caller_id,
        cmd,
        trace_id,
        request_id,
        layer_stack_root,
        timeout_seconds,
        yield_time_ms,
        cwd,
        remountable,
    } = request;

    let route = workspace
        .route_command_context(&caller_id, &invocation_id, layer_stack_root)
        .map_err(CommandOpError::from_workspace)?;
    record_route_selected(context, route.trace_facts());
    let command_caller_id = route.caller_id().to_owned();
    let command_remountable = route.remountable(remountable);
    route
        .with_exec_target(command_ops.scratch_root(), |target| {
            command_ops.exec_command_with_trace(
                StartCommand {
                    invocation_id,
                    caller_id: command_caller_id,
                    cmd,
                    trace_id,
                    request_id,
                    timeout_seconds,
                    yield_time_ms,
                    cwd,
                    remountable: command_remountable,
                },
                target,
            )
        })
        .map_err(CommandOpError::Command)
}

fn record_route_selected(context: &DispatchContext<'_>, facts: &WorkspaceRouteTraceFacts) {
    let details = if let Some(layer_stack_root) = &facts.layer_stack_root {
        json!({
            "kind": facts.kind,
            "reason": facts.reason,
            "layer_stack_root": layer_stack_root,
        })
    } else {
        json!({
            "kind": facts.kind,
            "reason": facts.reason,
        })
    };
    context.record_trace_event("workspace.route", "route_selected", details);
}

pub(crate) fn op_command_collect_completed(
    input: CollectCompletedInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    Ok(context
        .require_services()?
        .command
        .collect_completed(&collect_completed_request(input))
        .to_wire_value())
}

pub(crate) fn op_command_count(
    input: CallerCountInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let caller_id = input.caller.to_string();
    let count = context
        .require_services()?
        .command
        .count_by_caller((!caller_id.is_empty()).then_some(&caller_id));
    Ok(to_wire_value(CommandCountOutput {
        success: true,
        caller_id,
        count,
    }))
}

pub(crate) fn command_write_stdin(
    input: WriteStdinInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let command_ops = &context.require_services()?.command;
    let request = WriteStdin {
        command_id: input.command_id.to_string(),
        chars: input.chars,
        yield_time_ms: input
            .yield_time_ms
            .unwrap_or(command_ops.config().default_yield_time_ms),
    };
    match command_ops.write_stdin_with_trace(request) {
        Ok(outcome) => {
            if let Some(trace) = &outcome.trace {
                record_stdin_written(&context, trace);
            }
            command_response_to_wire(Ok(outcome.response))
        }
        Err(error) => command_response_to_wire(Err(error)),
    }
}

pub(crate) fn command_read_progress(
    input: ReadProgressInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let command_ops = &context.require_services()?.command;
    let request = ReadCommandProgress {
        command_id: input.command_id.to_string(),
        last_n_lines: input.last_n_lines,
    };
    match command_ops.read_command_progress_with_trace(request) {
        Ok(outcome) => {
            record_progress_read(&context, &outcome.trace);
            command_response_to_wire(Ok(outcome.response))
        }
        Err(error) => command_response_to_wire(Err(error)),
    }
}

pub(crate) fn command_cancel(
    input: CancelCommandInput,
    context: DispatchContext<'_>,
) -> Result<Value, DaemonError> {
    let command_ops = &context.require_services()?.command;
    let request = CancelCommand {
        command_id: input.command_id.to_string(),
    };
    command_response_to_wire(command_ops.cancel(request))
}

fn command_response_to_wire(
    response: Result<CommandResponse, CommandError>,
) -> Result<Value, DaemonError> {
    match response {
        Ok(response) => Ok(response.to_wire_value()),
        // The not-found synthetic is not an error response; it stays a
        // CommandResponse-shaped output.
        Err(CommandError::NotFound(_)) => {
            Ok(CommandResponse::error("command_not_found").to_wire_value())
        }
        Err(error) => Err(command_error(error)),
    }
}

fn strip_command_id(mut response: Value) -> Value {
    if let Some(object) = response.as_object_mut() {
        object.remove("command_id");
    }
    response
}

fn command_error(error: CommandError) -> DaemonError {
    match error {
        CommandError::Io(message) => DaemonError::OverlayPipeline(message),
        other @ CommandError::ArtifactWrite { .. } => {
            DaemonError::OverlayPipeline(other.to_string())
        }
        other => DaemonError::InvalidRequest(other.to_string()),
    }
}

fn command_op_error(error: CommandOpError) -> DaemonError {
    match error {
        CommandOpError::MissingLayerStackRoot => {
            DaemonError::InvalidRequest("layer_stack_root is required".to_owned())
        }
        CommandOpError::Workspace(error) => DaemonError::InvalidRequest(error.to_string()),
        CommandOpError::Command(error) => command_error(error.into_error()),
    }
}

fn is_missing_layer_stack_root(error: &workspace::WorkspaceError) -> bool {
    matches!(
        error,
        workspace::WorkspaceError::InvalidRequest { field, message }
            if *field == "layer_stack_root" && message == "layer_stack_root is required"
    )
}

fn collect_completed_request(input: CollectCompletedInput) -> CollectCompleted {
    CollectCompleted {
        command_ids: input.command_ids.map(|ids| {
            ids.into_iter()
                .map(|command_id| command_id.to_string())
                .collect()
        }),
        caller_id: input.caller.map(|caller| caller.to_string()),
    }
}

fn record_command_trace_events(context: &DispatchContext<'_>, events: &[CommandTraceEvent]) {
    for event in events {
        if event.name == "resource_stats" {
            context.record_trace_event(
                "resource",
                event.name,
                command_resource_stats_details(context, &event.details),
            );
        } else if let Some(name) = event.name.strip_prefix("overlay_") {
            context.record_trace_event("overlay", name, event.details.clone());
        } else {
            context.record_trace_event("command", event.name, event.details.clone());
        }
    }
}

fn command_resource_stats_details(context: &DispatchContext<'_>, details: &Value) -> Value {
    let mut details = details.clone();
    if let Some(meta) = details.get_mut("meta").and_then(Value::as_object_mut) {
        meta.insert(
            "inflight_requests".to_owned(),
            json!(context.invocation_registry().map_or(
                0,
                crate::invocation_registry::InFlightRegistry::inflight_count
            )),
        );
    }
    details
}

fn record_stdin_written(context: &DispatchContext<'_>, trace: &CommandStdinTraceFacts) {
    context.record_trace_event(
        "command",
        "stdin_written",
        json!({
            "command_id": trace.command_id,
            "bytes": trace.bytes,
            "wait_ms": trace.wait_ms,
            "waited_for_output": trace.waited_for_output,
            "status": trace.status.as_str(),
        }),
    );
}

fn record_progress_read(context: &DispatchContext<'_>, trace: &CommandProgressTraceFacts) {
    context.record_trace_event(
        "command",
        "progress_read",
        json!({
            "command_id": trace.command_id,
            "last_n_lines": trace.last_n_lines,
            "status": trace.status.as_str(),
            "source": trace.source,
            "stdout_bytes": trace.stdout_bytes,
        }),
    );
}

#[cfg(test)]
#[path = "../../tests/unit/command/mod.rs"]
mod tests;
