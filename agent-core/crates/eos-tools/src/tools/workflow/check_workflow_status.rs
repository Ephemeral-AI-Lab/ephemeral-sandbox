//! The `check_workflow_status` control tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{JsonObject, WorkflowId, WorkflowSessionId};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::ports::WorkflowControlPort;
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;

use super::lib::empty_workflow_id_error;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CheckWorkflowStatusInput {
    workflow_id: WorkflowId,
    #[serde(default)]
    workflow_task_id: Option<WorkflowSessionId>,
}

pub(in crate::tools::workflow) struct CheckWorkflowStatus {
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
}

impl CheckWorkflowStatus {
    pub(in crate::tools::workflow) fn new(
        workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    ) -> Self {
        Self { workflow_control }
    }
}

#[async_trait]
impl ToolExecutor for CheckWorkflowStatus {
    async fn execute(
        &self,
        input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: CheckWorkflowStatusInput =
            match parse_input(ToolName::CheckWorkflowStatus, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        if parsed.workflow_id.as_str().is_empty() {
            return Ok(empty_workflow_id_error(
                ToolName::CheckWorkflowStatus,
                "workflow_id",
            ));
        }
        if parsed
            .workflow_task_id
            .as_ref()
            .is_some_and(|id| id.as_str().is_empty())
        {
            return Ok(empty_workflow_id_error(
                ToolName::CheckWorkflowStatus,
                "workflow_task_id",
            ));
        }
        let output = self
            .workflow_control
            .as_deref()
            .ok_or(ToolError::MissingPort("workflow_control"))?
            .status(&parsed.workflow_id, parsed.workflow_task_id.as_ref())
            .await?;
        Ok(ToolResult::ok(output))
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
) {
    let check = config.get(ToolName::CheckWorkflowStatus);
    super::super::register_tool(
        registry,
        ToolName::CheckWorkflowStatus,
        check,
        text_spec(
            ToolName::CheckWorkflowStatus,
            &check.description,
            schema_for!(CheckWorkflowStatusInput),
        ),
        OutputShape::Text,
        Arc::new(CheckWorkflowStatus::new(workflow_control)),
    );
}
