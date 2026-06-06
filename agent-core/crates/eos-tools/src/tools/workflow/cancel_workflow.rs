//! The `cancel_workflow` control tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{JsonObject, WorkflowSessionId};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};

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

use super::lib::empty_workflow_id_error;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CancelWorkflowInput {
    workflow_task_id: WorkflowSessionId,
    #[serde(default)]
    reason: String,
}

pub(in crate::tools::workflow) struct CancelWorkflow {
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
}

impl CancelWorkflow {
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
impl ToolExecutor for CancelWorkflow {
    async fn execute(
        &self,
        input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: CancelWorkflowInput = match parse_input(ToolName::CancelWorkflow, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.workflow_task_id.as_str().is_empty() {
            return Ok(empty_workflow_id_error(
                ToolName::CancelWorkflow,
                "workflow_task_id",
            ));
        }
        let output = self
            .workflow_control
            .as_deref()
            .ok_or(ToolError::MissingPort("workflow_control"))?
            .cancel(&parsed.workflow_task_id, &parsed.reason)
            .await?;
        if let Some(supervisor) = &self.background_supervisor {
            supervisor
                .cancel_workflow_record(&parsed.workflow_task_id, &parsed.reason)
                .await;
        }
        Ok(ToolResult::ok(output))
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
) {
    let cancel = config.get(ToolName::CancelWorkflow);
    super::super::register_tool(
        registry,
        ToolName::CancelWorkflow,
        cancel,
        text_spec(
            ToolName::CancelWorkflow,
            &cancel.description,
            schema_for!(CancelWorkflowInput),
        ),
        OutputShape::Text,
        Arc::new(CancelWorkflow::new(workflow_control, background_supervisor)),
    );
}
