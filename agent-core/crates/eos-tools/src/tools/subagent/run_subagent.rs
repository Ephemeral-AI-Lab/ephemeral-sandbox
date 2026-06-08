//! The `run_subagent` launch tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_agent_ports::{
    AgentName, AgentRunApi, AgentRunError, AgentRunRecordKind, SpawnAgentRequest,
};
use eos_llm_client::Message;
use eos_types::JsonObject;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::CallerScope;
use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec_with_agent_enum;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;
use crate::SubagentLaunchRejection;

use super::super::SubagentToolService;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct RunSubagentInput {
    /// Name of a registered dispatchable subagent (caller-scoped enum).
    agent_name: String,
    prompt: String,
}

pub(in crate::tools::subagent) struct RunSubagent {
    agent_run_service: Option<Arc<dyn AgentRunApi>>,
    subagent_sessions: Option<SubagentToolService>,
}

impl RunSubagent {
    pub(in crate::tools::subagent) fn new(
        agent_run_service: Option<Arc<dyn AgentRunApi>>,
        subagent_sessions: Option<SubagentToolService>,
    ) -> Self {
        Self {
            agent_run_service,
            subagent_sessions,
        }
    }
}

fn launch_result(agent_run_id: &eos_types::AgentRunId, agent_name: &str) -> ToolResult {
    let agent_run_id_str = agent_run_id.as_str();
    let mut metadata = JsonObject::new();
    metadata.insert("agent_run_id".to_owned(), json!(agent_run_id_str));
    metadata.insert("status".to_owned(), json!("running"));
    metadata.insert("agent_name".to_owned(), json!(agent_name));
    ToolResult::ok(format!(
        "[SUBAGENT LAUNCHED] agent_run_id=\"{agent_run_id_str}\" status=running \
         agent_name=\"{agent_name}\"\nUse cancel_subagent(agent_run_id=\"{agent_run_id_str}\") \
         to stop it. \
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
        let parent_agent_run_id = ctx.require_agent_run_id()?.clone();
        let requested_agent_name = parsed.agent_name.clone();
        let agent_name = match AgentName::new(&parsed.agent_name) {
            Ok(agent_name) => agent_name,
            Err(_) => {
                return Ok(launch_rejection(SubagentLaunchRejection::NotRegistered {
                    agent_name: requested_agent_name,
                }))
            }
        };
        let agent_run_id = match self
            .agent_run_service
            .as_deref()
            .ok_or(ToolError::MissingPort("agent_run_service"))?
            .spawn_agent(SpawnAgentRequest {
                agent_name: agent_name.clone(),
                agent_run_id: None,
                initial_messages: vec![
                    Message::from_user_text(parsed.prompt.clone()),
                    Message::from_user_text(explorer_launch_guidance()),
                ],
                parent_agent_run_id: Some(parent_agent_run_id.clone()),
                request_id: ctx.request_id.clone(),
                task_id: None,
                attempt_id: None,
                workflow_id: None,
                sandbox_id: ctx.sandbox_id.clone(),
                workspace_root: ctx.workspace_root.clone(),
                is_isolated_workspace_mode: ctx.is_isolated_workspace_mode,
                persist: true,
                record_kind: AgentRunRecordKind::Subagent {
                    parent_agent_run_id,
                },
            })
            .await
        {
            Ok(agent_run_id) => agent_run_id,
            Err(err) => return render_launch_error(err),
        };
        self.subagent_sessions
            .as_ref()
            .ok_or(ToolError::MissingPort("subagent_sessions"))?
            .register_background_session(&agent_run_id)
            .await?;
        Ok(launch_result(&agent_run_id, agent_name.as_str()))
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    caller: &CallerScope,
    agent_run_service: Option<Arc<dyn AgentRunApi>>,
    subagent_sessions: Option<SubagentToolService>,
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
        Arc::new(RunSubagent::new(agent_run_service, subagent_sessions)),
    );
}

fn render_launch_error(err: AgentRunError) -> Result<ToolResult, ToolError> {
    match err {
        AgentRunError::RecursiveSubagent => {
            Ok(launch_rejection(SubagentLaunchRejection::Recursive))
        }
        AgentRunError::AgentNotRegistered(agent_name) => {
            Ok(launch_rejection(SubagentLaunchRejection::NotRegistered {
                agent_name,
            }))
        }
        AgentRunError::WrongAgentType {
            agent_name, actual, ..
        } => Ok(launch_rejection(SubagentLaunchRejection::NotSubagent {
            agent_name,
            agent_type: actual.to_owned(),
        })),
        err => Err(ToolError::Internal(format!("run_subagent: {err}"))),
    }
}
