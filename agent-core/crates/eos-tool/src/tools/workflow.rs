//! Workflow tools.

use crate::ToolName;
use crate::ToolResult;

pub(super) fn empty_workflow_id_error(tool: ToolName, field: &str) -> ToolResult {
    ToolResult::error(format!(
        "Invalid input for {}: {field} must be non-empty. \
         Please retry the tool call with valid arguments.",
        tool.as_str()
    ))
}

mod delegate_workflow {
    //! The `delegate_workflow` launch tool.

    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_types::JsonObject;
    use schemars::{schema_for, JsonSchema};
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use eos_types::{StartWorkflowRequest, WorkflowApi};

    use crate::registry::text_spec;
    use crate::registry::ToolConfigSet;
    use crate::tools::parse_input;
    use crate::tools::BackgroundHandle;
    use crate::ExecutionMetadata;
    use crate::ToolError;
    use crate::ToolExecutor;
    use crate::ToolName;
    use crate::ToolRegistry;
    use crate::{OutputShape, ToolResult};

    #[derive(Debug, Deserialize, Serialize, JsonSchema)]
    pub(super) struct DelegateWorkflowInput {
        goal: String,
    }

    pub(in crate::tools::workflow) struct DelegateWorkflow {
        workflow_service: Arc<dyn WorkflowApi>,
        workflow_sessions: BackgroundHandle,
    }

    impl DelegateWorkflow {
        pub(in crate::tools::workflow) fn new(
            workflow_service: Arc<dyn WorkflowApi>,
            workflow_sessions: BackgroundHandle,
        ) -> Self {
            Self {
                workflow_service,
                workflow_sessions,
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
            let parsed: DelegateWorkflowInput = match parse_input(ToolName::DelegateWorkflow, input)
            {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
            if parsed.goal.trim().is_empty() {
                return Ok(ToolResult::error("goal must be nonblank"));
            }
            let task_id = ctx.require_task_id()?;
            let agent_run_id = ctx.require_agent_run_id()?;
            let service = self.workflow_service.as_ref();
            let sessions = &self.workflow_sessions;

            let open_workflows = service
                .list_open_delegated_workflows_for_agent_run(agent_run_id)
                .await?;
            if let Some(existing) = open_workflows.first() {
                let payload = json!({
                    "workflow_id": existing.workflow_id.as_str(),
                    "status": "running",
                    "message": "A delegated workflow is already open for this agent run. \
                        Use check_workflow_status or cancel_workflow before starting another.",
                });
                return Ok(ToolResult::error(payload.to_string()));
            }

            let started = service
                .start_workflow(StartWorkflowRequest {
                    parent_task_id: task_id.clone(),
                    agent_run_id: agent_run_id.clone(),
                    tool_use_id: ctx.tool_use_id.clone(),
                    workflow_goal: parsed.goal.clone(),
                })
                .await?;
            sessions.register_workflow_session(&started).await?;
            let payload = json!({
                "workflow_id": started.workflow_id.as_str(),
                "status": "running",
                "message": format!(
                    "Started delegated workflow {}. Use check_workflow_status to inspect progress \
                     or cancel_workflow to stop it.",
                    started.workflow_id
                ),
            });
            let metadata: JsonObject = [
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
        workflow_service: Arc<dyn WorkflowApi>,
        workflow_sessions: BackgroundHandle,
    ) {
        let delegate = config.get(ToolName::DelegateWorkflow);
        crate::tools::register_tool(
            registry,
            ToolName::DelegateWorkflow,
            delegate,
            text_spec(
                ToolName::DelegateWorkflow,
                &delegate.description,
                schema_for!(DelegateWorkflowInput),
            ),
            OutputShape::Text,
            Arc::new(DelegateWorkflow::new(workflow_service, workflow_sessions)),
        );
    }
}
mod check_workflow_status {
    //! The `check_workflow_status` control tool.

    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_types::{JsonObject, WorkflowApi, WorkflowId};
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

    use super::empty_workflow_id_error;

    #[derive(Debug, Deserialize, Serialize, JsonSchema)]
    pub(super) struct CheckWorkflowStatusInput {
        workflow_id: WorkflowId,
    }

    pub(in crate::tools::workflow) struct CheckWorkflowStatus {
        workflow_service: Arc<dyn WorkflowApi>,
    }

    impl CheckWorkflowStatus {
        pub(in crate::tools::workflow) fn new(workflow_service: Arc<dyn WorkflowApi>) -> Self {
            Self { workflow_service }
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
            let output = self
                .workflow_service
                .as_ref()
                .check_workflow_status(&parsed.workflow_id)
                .await?;
            Ok(ToolResult::ok(output))
        }
    }

    pub(super) fn register(
        registry: &mut ToolRegistry,
        config: &ToolConfigSet,
        workflow_service: Arc<dyn WorkflowApi>,
    ) {
        let check = config.get(ToolName::CheckWorkflowStatus);
        crate::tools::register_tool(
            registry,
            ToolName::CheckWorkflowStatus,
            check,
            text_spec(
                ToolName::CheckWorkflowStatus,
                &check.description,
                schema_for!(CheckWorkflowStatusInput),
            ),
            OutputShape::Text,
            Arc::new(CheckWorkflowStatus::new(workflow_service)),
        );
    }
}
mod cancel_workflow {
    //! The `cancel_workflow` control tool.

    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_types::{JsonObject, WorkflowApi, WorkflowId};
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

    use super::empty_workflow_id_error;

    #[derive(Debug, Deserialize, Serialize, JsonSchema)]
    pub(super) struct CancelWorkflowInput {
        workflow_id: WorkflowId,
        #[serde(default)]
        reason: String,
    }

    pub(in crate::tools::workflow) struct CancelWorkflow {
        workflow_service: Arc<dyn WorkflowApi>,
    }

    impl CancelWorkflow {
        pub(in crate::tools::workflow) fn new(workflow_service: Arc<dyn WorkflowApi>) -> Self {
            Self { workflow_service }
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
            if parsed.workflow_id.as_str().is_empty() {
                return Ok(empty_workflow_id_error(
                    ToolName::CancelWorkflow,
                    "workflow_id",
                ));
            }
            let output = self
                .workflow_service
                .as_ref()
                .cancel_workflow(&parsed.workflow_id, &parsed.reason)
                .await?;
            Ok(ToolResult::ok(output))
        }
    }

    pub(super) fn register(
        registry: &mut ToolRegistry,
        config: &ToolConfigSet,
        workflow_service: Arc<dyn WorkflowApi>,
    ) {
        let cancel = config.get(ToolName::CancelWorkflow);
        crate::tools::register_tool(
            registry,
            ToolName::CancelWorkflow,
            cancel,
            text_spec(
                ToolName::CancelWorkflow,
                &cancel.description,
                schema_for!(CancelWorkflowInput),
            ),
            OutputShape::Text,
            Arc::new(CancelWorkflow::new(workflow_service)),
        );
    }
}

pub(crate) fn register(
    registry: &mut crate::ToolRegistry,
    config: &crate::registry::ToolConfigSet,
    workflow: std::sync::Arc<dyn eos_types::WorkflowApi>,
    background: crate::tools::BackgroundHandle,
) {
    delegate_workflow::register(registry, config, workflow.clone(), background);
    check_workflow_status::register(registry, config, workflow.clone());
    cancel_workflow::register(registry, config, workflow);
}

pub(crate) fn register_schema(
    registry: &mut crate::ToolRegistry,
    config: &crate::registry::ToolConfigSet,
) {
    use crate::registry::text_spec;
    use crate::{OutputShape, ToolName};
    use schemars::schema_for;

    let delegate = config.get(ToolName::DelegateWorkflow);
    crate::tools::register_schema_tool(
        registry,
        ToolName::DelegateWorkflow,
        delegate,
        text_spec(
            ToolName::DelegateWorkflow,
            &delegate.description,
            schema_for!(delegate_workflow::DelegateWorkflowInput),
        ),
        OutputShape::Text,
    );
    let check = config.get(ToolName::CheckWorkflowStatus);
    crate::tools::register_schema_tool(
        registry,
        ToolName::CheckWorkflowStatus,
        check,
        text_spec(
            ToolName::CheckWorkflowStatus,
            &check.description,
            schema_for!(check_workflow_status::CheckWorkflowStatusInput),
        ),
        OutputShape::Text,
    );
    let cancel = config.get(ToolName::CancelWorkflow);
    crate::tools::register_schema_tool(
        registry,
        ToolName::CancelWorkflow,
        cancel,
        text_spec(
            ToolName::CancelWorkflow,
            &cancel.description,
            schema_for!(cancel_workflow::CancelWorkflowInput),
        ),
        OutputShape::Text,
    );
}
