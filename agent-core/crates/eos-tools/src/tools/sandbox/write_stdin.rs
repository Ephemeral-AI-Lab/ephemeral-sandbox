use async_trait::async_trait;
use eos_sandbox_api::ExecStdinRequest;
use eos_types::{CommandSessionId, JsonObject};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::ToolResult;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;

use super::super::CommandToolService;
use super::lib::{
    command_result_value, command_tool_result, command_tool_result_from_value, default_false,
    default_yield_ms, invalid_input, is_command_session_not_found, request_base,
    validate_command_timing,
};

fn default_chars() -> String {
    String::new()
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct WriteStdinInput {
    command_session_id: CommandSessionId,
    #[serde(default = "default_chars")]
    chars: String,
    #[serde(default = "default_yield_ms")]
    #[schemars(default = "default_yield_ms", range(max = 30000))]
    yield_time_ms: u32,
    #[serde(default)]
    #[schemars(range(min = 1))]
    max_output_tokens: Option<u32>,
    /// Tear the session down after writing. A `\x03` char only interrupts
    /// (SIGINT); set this to end the session.
    #[serde(default = "default_false")]
    terminate: bool,
}

pub(super) struct WriteStdin {
    service: CommandToolService,
}

impl WriteStdin {
    pub(super) fn new(service: CommandToolService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for WriteStdin {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: WriteStdinInput = match parse_input(ToolName::WriteStdin, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if let Some(err) = validate_command_timing(
            ToolName::WriteStdin,
            parsed.yield_time_ms,
            None,
            parsed.max_output_tokens,
        ) {
            return Ok(err);
        }
        if parsed.command_session_id.as_str().is_empty() {
            return Ok(invalid_input(
                ToolName::WriteStdin,
                "command_session_id must be non-empty",
            ));
        }
        let command_session_id = &parsed.command_session_id;
        let sandbox_id = ctx.require_sandbox_id()?;
        // Ctrl-C decoupling (sense-2 D7): `\x03` rides through as ordinary stdin
        // and the daemon raises SIGINT; teardown is the explicit `terminate`
        // flag, so the tool no longer escalates to a cancel RPC.
        let write_request = ExecStdinRequest {
            base: request_base(ctx, "write_stdin")?,
            command_session_id: command_session_id.clone(),
            chars: parsed.chars.clone(),
            yield_time_ms: Some(parsed.yield_time_ms),
            max_output_tokens: parsed.max_output_tokens,
            terminate: parsed.terminate,
        };
        let result =
            match eos_sandbox_api::exec_stdin(&*self.service.transport, sandbox_id, &write_request)
                .await
            {
                Ok(result) => result,
                Err(err) => return Ok(ToolResult::error(err.to_string())),
            };
        // If the daemon already lost the live session, surface the supervisor's
        // stored terminal; otherwise, once a terminal status is observed inline,
        // latch it as delivered so the heartbeat never re-notifies the same result.
        if let Some(port) = &self.service.command_session_supervisor {
            if is_command_session_not_found(&result) {
                if port
                    .command_session_already_reported(command_session_id)
                    .await
                {
                    return Ok(ToolResult::ok(format!(
                        "Command session {command_session_id} already completed; \
                         its result was already reported."
                    )));
                }
                if let Some(stored) = port.command_session_result(command_session_id).await {
                    port.mark_command_session_reported(command_session_id, stored.clone())
                        .await;
                    return Ok(command_tool_result_from_value(&stored));
                }
            } else if !result.is_running() {
                port.mark_command_session_reported(
                    command_session_id,
                    command_result_value(&result),
                )
                .await;
            }
        }
        Ok(command_tool_result(&result))
    }
}
