//! Pure command-session helpers: `exec_command`, `exec_stdin`/`write_stdin`,
//! `cancel_command_session`, and `collect_command_completions`. The first three
//! return [`ExecCommandResult`]; `collect_command_completions` returns the raw
//! completion maps (the only verb without a typed result struct).

use eos_types::{JsonObject, SandboxId};
use serde_json::Value;

use crate::error::SandboxApiError;
use crate::models::{
    CommandSessionCancelRequest, CommandSessionWriteRequest, ExecCommandRequest, ExecCommandResult,
    ExecStdinRequest,
};
use crate::ops::DaemonOp;
use crate::timeouts::exec_dispatch_timeout;
use crate::tool_api::parse::{daemon_request_identity_fields, parse_exec_command_result};
use crate::transport::SandboxTransport;

/// Run or start a managed command session.
pub async fn exec_command(
    transport: &dyn SandboxTransport,
    sandbox_id: &SandboxId,
    request: &ExecCommandRequest,
) -> Result<ExecCommandResult, SandboxApiError> {
    let mut payload = daemon_request_identity_fields(&request.base);
    payload.insert("cmd".to_owned(), Value::String(request.cmd.clone()));
    if let Some(yield_time_ms) = request.yield_time_ms {
        payload.insert("yield_time_ms".to_owned(), Value::from(yield_time_ms));
    }
    if let Some(timeout) = request.timeout {
        payload.insert("timeout".to_owned(), Value::from(timeout));
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        payload.insert(
            "max_output_tokens".to_owned(),
            Value::from(max_output_tokens),
        );
    }
    let response = transport
        .call(
            sandbox_id,
            DaemonOp::ExecCommand,
            payload,
            exec_dispatch_timeout(request.timeout),
        )
        .await?;
    parse_exec_command_result(&response)
}

/// Write characters (stdin) to an open command session.
pub async fn exec_stdin(
    transport: &dyn SandboxTransport,
    sandbox_id: &SandboxId,
    request: &ExecStdinRequest,
) -> Result<ExecCommandResult, SandboxApiError> {
    let mut payload = daemon_request_identity_fields(&request.base);
    payload.insert(
        "command_session_id".to_owned(),
        Value::String(request.command_session_id.clone()),
    );
    payload.insert("chars".to_owned(), Value::String(request.chars.clone()));
    if let Some(yield_time_ms) = request.yield_time_ms {
        payload.insert("yield_time_ms".to_owned(), Value::from(yield_time_ms));
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        payload.insert(
            "max_output_tokens".to_owned(),
            Value::from(max_output_tokens),
        );
    }
    if request.terminate {
        payload.insert("terminate".to_owned(), Value::Bool(true));
    }
    let response = transport
        .call(
            sandbox_id,
            DaemonOp::ExecStdin,
            payload,
            exec_dispatch_timeout(None),
        )
        .await?;
    parse_exec_command_result(&response)
}

/// Model-facing alias for [`exec_stdin`].
pub async fn write_stdin(
    transport: &dyn SandboxTransport,
    sandbox_id: &SandboxId,
    request: &CommandSessionWriteRequest,
) -> Result<ExecCommandResult, SandboxApiError> {
    exec_stdin(transport, sandbox_id, request).await
}

/// Cancel an open command session.
pub async fn cancel_command_session(
    transport: &dyn SandboxTransport,
    sandbox_id: &SandboxId,
    request: &CommandSessionCancelRequest,
) -> Result<ExecCommandResult, SandboxApiError> {
    let mut payload = daemon_request_identity_fields(&request.base);
    payload.insert(
        "command_session_id".to_owned(),
        Value::String(request.command_session_id.clone()),
    );
    let response = transport
        .call(
            sandbox_id,
            DaemonOp::CommandCancel,
            payload,
            exec_dispatch_timeout(None),
        )
        .await?;
    parse_exec_command_result(&response)
}

/// Collect completed background command sessions for one agent. Returns the raw
/// completion maps (objects only; non-object entries are dropped).
pub async fn collect_command_completions(
    transport: &dyn SandboxTransport,
    sandbox_id: &SandboxId,
    agent_id: &str,
    command_session_ids: &[String],
) -> Result<Vec<JsonObject>, SandboxApiError> {
    let mut payload = JsonObject::new();
    payload.insert("agent_id".to_owned(), Value::String(agent_id.to_owned()));
    payload.insert(
        "command_session_ids".to_owned(),
        Value::Array(
            command_session_ids
                .iter()
                .map(|id| Value::String(id.clone()))
                .collect(),
        ),
    );
    let response = transport
        .call(
            sandbox_id,
            DaemonOp::CommandCollectCompleted,
            payload,
            exec_dispatch_timeout(None),
        )
        .await?;
    let completions = match response.get("completions") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_object().cloned())
            .collect(),
        _ => Vec::new(),
    };
    Ok(completions)
}
