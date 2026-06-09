//! Subagent tools.

use crate::ToolName;
use crate::ToolResult;

pub(super) fn empty_subagent_agent_run_error(tool: ToolName) -> ToolResult {
    ToolResult::error(format!(
        "Invalid input for {}: agent_run_id must be non-empty. \
         Please retry the tool call with valid arguments.",
        tool.as_str()
    ))
}

mod run_subagent {
    //! The `run_subagent` launch tool.

    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_types::JsonObject;
    use eos_types::Message;
    use eos_types::{
        AgentName, AgentRunApi, AgentRunError, ParentAgentRunAnchor, SpawnAgentRequest,
        SpawnAgentTarget,
    };
    use schemars::{schema_for, JsonSchema};
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use crate::registry::text_spec_with_agent_enum;
    use crate::registry::ToolConfigSet;
    use crate::tools::parse_input;
    use crate::tools::CallerScope;
    use crate::ExecutionMetadata;
    use crate::SubagentLaunchRejection;
    use crate::ToolError;
    use crate::ToolExecutor;
    use crate::ToolName;
    use crate::ToolRegistry;
    use crate::{OutputShape, ToolResult};

    use crate::tools::BackgroundHandle;

    #[derive(Debug, Deserialize, Serialize, JsonSchema)]
    pub(super) struct RunSubagentInput {
        /// Name of a registered dispatchable subagent (caller-scoped enum).
        agent_name: String,
        prompt: String,
    }

    pub(in crate::tools::subagent) struct RunSubagent {
        agent_run_service: Arc<dyn AgentRunApi>,
        subagent_sessions: BackgroundHandle,
    }

    impl RunSubagent {
        pub(in crate::tools::subagent) fn new(
            agent_run_service: Arc<dyn AgentRunApi>,
            subagent_sessions: BackgroundHandle,
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

    fn subagent_launch_guidance() -> String {
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
     Call `submit_subagent_result`."
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
            let parent_task_id = ctx.require_task_id()?.clone();
            let parent_request_id = ctx.require_request_id()?.clone();
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
                .spawn_agent(SpawnAgentRequest {
                    agent_name: agent_name.clone(),
                    agent_run_id: None,
                    initial_messages: vec![
                        Message::from_user_text(parsed.prompt.clone()),
                        Message::from_user_text(subagent_launch_guidance()),
                    ],
                    target: SpawnAgentTarget::Subagent {
                        parent: ParentAgentRunAnchor {
                            request_id: parent_request_id,
                            parent_task_id,
                            agent_run_id: parent_agent_run_id,
                        },
                    },
                    sandbox_id: ctx.sandbox_id.clone(),
                    workspace_root: ctx.workspace_root.clone(),
                    is_isolated_workspace_mode: ctx.is_isolated_workspace_mode,
                    persist: true,
                })
                .await
            {
                Ok(agent_run_id) => agent_run_id,
                Err(err) => return render_launch_error(err),
            };
            self.subagent_sessions
                .register_subagent_run(&agent_run_id)
                .await?;
            Ok(launch_result(&agent_run_id, agent_name.as_str()))
        }
    }

    pub(super) fn register(
        registry: &mut ToolRegistry,
        config: &ToolConfigSet,
        caller: &CallerScope,
        agent_run_service: Arc<dyn AgentRunApi>,
        subagent_sessions: BackgroundHandle,
    ) {
        let run = config.get(ToolName::RunSubagent);
        crate::tools::register_tool(
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
}
mod cancel_subagent {
    //! The `cancel_subagent` control tool.

    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_types::{AgentRunId, JsonObject};
    use schemars::{schema_for, JsonSchema};
    use serde::{Deserialize, Serialize};

    use crate::registry::text_spec;
    use crate::registry::ToolConfigSet;
    use crate::tools::parse_input;
    use crate::ExecutionMetadata;
    use crate::ToolError;
    use crate::ToolExecutor;
    use crate::ToolName;
    use crate::ToolRegistry;
    use crate::{OutputShape, ToolResult};

    use super::empty_subagent_agent_run_error;
    use crate::tools::BackgroundHandle;

    #[derive(Debug, Deserialize, Serialize, JsonSchema)]
    pub(super) struct CancelSubagentInput {
        agent_run_id: AgentRunId,
        #[serde(default)]
        reason: String,
    }

    pub(in crate::tools::subagent) struct CancelSubagent {
        subagent_sessions: BackgroundHandle,
    }

    impl CancelSubagent {
        pub(in crate::tools::subagent) fn new(subagent_sessions: BackgroundHandle) -> Self {
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
                .cancel_subagent_run(&parsed.agent_run_id, &parsed.reason)
                .await?
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
        subagent_sessions: BackgroundHandle,
    ) {
        let cancel = config.get(ToolName::CancelSubagent);
        crate::tools::register_tool(
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
}

pub(crate) fn register(
    registry: &mut crate::ToolRegistry,
    config: &crate::registry::ToolConfigSet,
    caller: &crate::tools::CallerScope,
    launcher: std::sync::Arc<dyn eos_types::AgentRunApi>,
    background: crate::tools::BackgroundHandle,
) {
    run_subagent::register(registry, config, caller, launcher, background.clone());
    cancel_subagent::register(registry, config, background);
}

pub(crate) fn register_schema(
    registry: &mut crate::ToolRegistry,
    config: &crate::registry::ToolConfigSet,
    caller: &crate::tools::CallerScope,
) {
    use crate::registry::{text_spec, text_spec_with_agent_enum};
    use crate::{OutputShape, ToolName};
    use schemars::schema_for;

    let run = config.get(ToolName::RunSubagent);
    crate::tools::register_schema_tool(
        registry,
        ToolName::RunSubagent,
        run,
        text_spec_with_agent_enum(
            ToolName::RunSubagent,
            &run.description,
            schema_for!(run_subagent::RunSubagentInput),
            &caller.dispatchable_subagents,
        ),
        OutputShape::Text,
    );
    let cancel = config.get(ToolName::CancelSubagent);
    crate::tools::register_schema_tool(
        registry,
        ToolName::CancelSubagent,
        cancel,
        text_spec(
            ToolName::CancelSubagent,
            &cancel.description,
            schema_for!(cancel_subagent::CancelSubagentInput),
        ),
        OutputShape::Text,
    );
}
