//! The `run_subagent` launch tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::JsonObject;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::CallerScope;
use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::ports::{BackgroundSupervisorPort, SpawnedSubagent, StartedSubagent};
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec_with_agent_enum;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct RunSubagentInput {
    /// Name of a registered dispatchable subagent (caller-scoped enum).
    agent_name: String,
    prompt: String,
}

pub(in crate::tools::subagent) struct RunSubagent {
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
}

impl RunSubagent {
    pub(in crate::tools::subagent) fn new(
        background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
    ) -> Self {
        Self {
            background_supervisor,
        }
    }
}

fn launch_result(agent_name: &str, started: &StartedSubagent) -> ToolResult {
    let session_id = started.subagent_session_id.as_str();
    let mut metadata = JsonObject::new();
    metadata.insert("subagent_session_id".to_owned(), json!(session_id));
    metadata.insert("status".to_owned(), json!("running"));
    metadata.insert("agent_name".to_owned(), json!(agent_name));
    ToolResult::ok(format!(
        "[SUBAGENT LAUNCHED] subagent_session_id=\"{session_id}\" status=running \
         agent_name=\"{agent_name}\"\nUse check_subagent_progress(\
         subagent_session_id=\"{session_id}\", last_n_messages=5) to inspect progress, \
         or cancel_subagent(subagent_session_id=\"{session_id}\") to stop it. \
         Keep using the current response on other ready work first."
    ))
    .with_metadata(metadata)
}

#[async_trait]
impl ToolExecutor for RunSubagent {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: RunSubagentInput = match parse_input(ToolName::RunSubagent, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.agent_name.trim().is_empty() {
            return Ok(ToolResult::error(
                "run_subagent: `agent_name` must be a non-empty string.",
            ));
        }
        if parsed.prompt.trim().is_empty() {
            return Ok(ToolResult::error(
                "run_subagent: `prompt` must be a non-empty string.",
            ));
        }
        match self
            .background_supervisor
            .as_deref()
            .ok_or(ToolError::MissingPort("background_supervisor"))?
            .spawn(ctx, &parsed.agent_name, &parsed.prompt)
            .await?
        {
            SpawnedSubagent::Launched(started) => Ok(launch_result(&parsed.agent_name, &started)),
            SpawnedSubagent::Rejected(message) => Ok(ToolResult::error(message)),
        }
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    caller: &CallerScope,
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
) {
    let run = config.get(ToolName::RunSubagent);
    super::super::register_tool(
        registry,
        ToolName::RunSubagent,
        run,
        text_spec_with_agent_enum(
            ToolName::RunSubagent,
            &run.description,
            schema_for!(RunSubagentInput),
            &caller.dispatchable_subagents,
        ),
        OutputShape::Text,
        Arc::new(RunSubagent::new(background_supervisor)),
    );
}
