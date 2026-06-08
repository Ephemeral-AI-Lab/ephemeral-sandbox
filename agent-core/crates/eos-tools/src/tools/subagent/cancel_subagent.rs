//! The `cancel_subagent` control tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{AgentRunId, JsonObject};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::ports::SubagentSessionPort;
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;

use super::lib::empty_subagent_agent_run_error;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CancelSubagentInput {
    agent_run_id: AgentRunId,
    #[serde(default)]
    reason: String,
}

pub(in crate::tools::subagent) struct CancelSubagent {
    subagent_sessions: Option<Arc<dyn SubagentSessionPort>>,
}

impl CancelSubagent {
    pub(in crate::tools::subagent) fn new(
        subagent_sessions: Option<Arc<dyn SubagentSessionPort>>,
    ) -> Self {
        Self { subagent_sessions }
    }
}

#[async_trait]
impl ToolExecutor for CancelSubagent {
    async fn execute(
        &self,
        input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: CancelSubagentInput = match parse_input(ToolName::CancelSubagent, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.agent_run_id.as_str().is_empty() {
            return Ok(empty_subagent_agent_run_error(ToolName::CancelSubagent));
        }
        if self
            .subagent_sessions
            .as_deref()
            .ok_or(ToolError::MissingPort("subagent_sessions"))?
            .cancel_background_agent_run(&parsed.agent_run_id, &parsed.reason)
            .await
        {
            Ok(render_cancelled(&parsed.agent_run_id, &parsed.reason))
        } else {
            Ok(ToolResult::error(format!(
                "Could not cancel subagent agent run {}. It may have already completed \
                 or does not exist.",
                parsed.agent_run_id.as_str()
            )))
        }
    }
}

fn render_cancelled(agent_run_id: &AgentRunId, reason: &str) -> ToolResult {
    let reason_suffix = if reason.is_empty() {
        String::new()
    } else {
        format!(" Reason: {reason}")
    };
    ToolResult::ok(format!(
        "Subagent agent run {} cancellation requested.{reason_suffix}",
        agent_run_id.as_str()
    ))
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    subagent_sessions: Option<Arc<dyn SubagentSessionPort>>,
) {
    let cancel = config.get(ToolName::CancelSubagent);
    super::super::register_tool(
        registry,
        ToolName::CancelSubagent,
        cancel,
        text_spec(
            ToolName::CancelSubagent,
            &cancel.description,
            schema_for!(CancelSubagentInput),
        ),
        OutputShape::Text,
        Arc::new(CancelSubagent::new(subagent_sessions)),
    );
}
