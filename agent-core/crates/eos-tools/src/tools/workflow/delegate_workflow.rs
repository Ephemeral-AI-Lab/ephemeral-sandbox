//! The `delegate_workflow` launch tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::JsonObject;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::ports::{BackgroundSupervisorPort, WorkflowControlPort};
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct DelegateWorkflowInput {
    goal: String,
}

pub(in crate::tools::workflow) struct DelegateWorkflow {
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
}

impl DelegateWorkflow {
    pub(in crate::tools::workflow) fn new(
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
    ) -> Self {
        Self {
            workflow_control,
            background_supervisor,
        }
    }
}

#[async_trait]
impl ToolExecutor for DelegateWorkflow {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: DelegateWorkflowInput = match parse_input(ToolName::DelegateWorkflow, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.goal.trim().is_empty() {
            return Ok(ToolResult::error("goal must be nonblank"));
        }
        let task_id = ctx.require_task_id()?;
        let agent_run_id = ctx.require_agent_run_id()?;
        let control = self
            .workflow_control
            .as_deref()
            .ok_or(ToolError::MissingPort("workflow_control"))?;
        let supervisor = self
            .background_supervisor
            .as_deref()
            .ok_or(ToolError::MissingPort("background_supervisor"))?;

        let outstanding = control.find_outstanding(task_id, agent_run_id).await?;
        if let Some(existing) = outstanding.first() {
            let payload = json!({
                "workflow_task_id": existing.workflow_task_id.as_str(),
                "workflow_id": existing.workflow_id.as_str(),
                "status": "running",
                "message": "A delegated workflow is already outstanding for this task. \
                    Use check_workflow_status or cancel_workflow before starting another.",
            });
            return Ok(ToolResult::error(payload.to_string()));
        }

        let started = control.start(task_id, agent_run_id, &parsed.goal).await?;
        supervisor.register_workflow(agent_run_id, &started).await;
        let payload = json!({
            "workflow_task_id": started.workflow_task_id.as_str(),
            "workflow_id": started.workflow_id.as_str(),
            "status": "running",
            "message": format!(
                "Started delegated workflow {}. Use check_workflow_status to inspect progress \
                 or cancel_workflow to stop it.",
                started.workflow_task_id
            ),
        });
        let metadata: JsonObject = [
            ("submission_kind".to_owned(), json!("workflow_delegated")),
            (
                "workflow_task_id".to_owned(),
                json!(started.workflow_task_id.as_str()),
            ),
            (
                "workflow_id".to_owned(),
                json!(started.workflow_id.as_str()),
            ),
            ("task_id".to_owned(), json!(task_id.as_str())),
        ]
        .into_iter()
        .collect();
        Ok(ToolResult::ok(payload.to_string()).with_metadata(metadata))
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
) {
    let delegate = config.get(ToolName::DelegateWorkflow);
    super::super::register_tool(
        registry,
        ToolName::DelegateWorkflow,
        delegate,
        text_spec(
            ToolName::DelegateWorkflow,
            &delegate.description,
            schema_for!(DelegateWorkflowInput),
        ),
        OutputShape::Text,
        Arc::new(DelegateWorkflow::new(
            workflow_control,
            background_supervisor,
        )),
    );
}
