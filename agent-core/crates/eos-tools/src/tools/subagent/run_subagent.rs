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
use crate::ports::{
    BackgroundSupervisorPort, SpawnedSubagent, StartedSubagent, SubagentLaunch,
    SubagentLaunchRejection,
};
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

fn launch_rejection(rejection: SubagentLaunchRejection) -> ToolResult {
    let message = match rejection {
        SubagentLaunchRejection::Recursive => {
            "run_subagent: subagents may not spawn further subagents. \
             This is a hard contract — handle the work directly or submit your findings via the terminal tool."
                .to_owned()
        }
        SubagentLaunchRejection::NotRegistered { agent_name } => {
            format!("run_subagent: agent '{agent_name}' is not registered.")
        }
        SubagentLaunchRejection::NotSubagent {
            agent_name,
            agent_type,
        } => format!(
            "run_subagent: agent '{agent_name}' is not a subagent \
             (agent_type='{agent_type}'); only subagent-typed agents may be dispatched here."
        ),
    };
    ToolResult::error(message)
}

fn explorer_launch_guidance() -> String {
    "# What's in context\n\
     - Parent's user message above\n\
     \n\
     # What to do\n\
     - Investigate the parent's question and return concrete findings.\n\
     \n\
     ## Deliver\n\
     - File paths, line numbers, specific symbols. No vague hand-waves.\n\
     - Missing context the parent will need to act on the findings.\n\
     - Obvious areas you skipped.\n\
     \n\
     ## Submit\n\
     Call `submit_exploration_result`."
        .to_owned()
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
            .spawn(
                ctx,
                SubagentLaunch {
                    agent_name: parsed.agent_name.clone(),
                    prompt: parsed.prompt.clone(),
                    guidance: explorer_launch_guidance(),
                },
            )
            .await?
        {
            SpawnedSubagent::Launched(started) => Ok(launch_result(&parsed.agent_name, &started)),
            SpawnedSubagent::Rejected(rejection) => Ok(launch_rejection(rejection)),
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
