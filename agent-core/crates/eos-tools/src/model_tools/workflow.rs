//! Workflow delegation tools: `delegate_workflow`, `check_workflow_status`,
//! `cancel_workflow`. All call the [`WorkflowControlPort`]; the live
//! workflow/outcome state lives downstream, so `status`/`cancel` return
//! already-rendered model-facing text.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{JsonObject, WorkflowId, WorkflowSessionId};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ToolError;
use crate::execution::parse_input;
use crate::executor::ToolExecutor;
use crate::metadata::ExecutionMetadata;
use crate::name::ToolName;
use crate::registry::ToolRegistry;
use crate::result::{OutputShape, ToolResult};
use crate::spec::text_spec;

const DELEGATE_DESCRIPTION: &str = "Start non-terminal delegated workflow work. Returns a workflow handle; continue running and use check_workflow_status or cancel_workflow later.";
const CHECK_DESCRIPTION: &str =
    "Inspect delegated workflow progress and print terminal outcomes when available.";
const CANCEL_DESCRIPTION: &str = "Cancel an outstanding delegated workflow by workflow_task_id.";

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct DelegateWorkflowInput {
    goal: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CheckWorkflowStatusInput {
    workflow_id: WorkflowId,
    #[serde(default)]
    workflow_task_id: Option<WorkflowSessionId>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CancelWorkflowInput {
    workflow_task_id: WorkflowSessionId,
    #[serde(default)]
    reason: String,
}

struct DelegateWorkflow;

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
        let agent_id = ctx.agent_id();
        let control = ctx.require_workflow_control()?;

        let outstanding = control.find_outstanding(task_id, &agent_id).await?;
        if let Some(existing) = outstanding.first() {
            let payload = json!({
                "workflow_task_id": existing.workflow_task_id.as_str(),
                "workflow_id": existing.workflow_id.as_str(),
                "status": "running",
                "message": "A delegated workflow is already outstanding for this task. \
                    Use check_workflow_status or cancel_workflow before starting another.",
            });
            return Ok(ToolResult::ok(payload.to_string()));
        }

        let started = control.start(task_id, &agent_id, &parsed.goal).await?;
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

struct CheckWorkflowStatus;

fn empty_workflow_id_error(tool: ToolName, field: &str) -> ToolResult {
    ToolResult::error(format!(
        "Invalid input for {}: {field} must be non-empty. \
         Please retry the tool call with valid arguments.",
        tool.as_str()
    ))
}

#[async_trait]
impl ToolExecutor for CheckWorkflowStatus {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
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
        let output = ctx
            .require_workflow_control()?
            .status(&parsed.workflow_id, parsed.workflow_task_id.as_ref())
            .await?;
        Ok(ToolResult::ok(output))
    }
}

struct CancelWorkflow;

#[async_trait]
impl ToolExecutor for CancelWorkflow {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
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
        let output = ctx
            .require_workflow_control()?
            .cancel(&parsed.workflow_task_id, &parsed.reason)
            .await?;
        Ok(ToolResult::ok(output))
    }
}

pub(crate) fn register(registry: &mut ToolRegistry) {
    super::register_tool(
        registry,
        ToolName::DelegateWorkflow,
        text_spec(
            ToolName::DelegateWorkflow,
            DELEGATE_DESCRIPTION,
            schema_for!(DelegateWorkflowInput),
        ),
        OutputShape::Text,
        Arc::new(DelegateWorkflow),
    );
    super::register_tool(
        registry,
        ToolName::CheckWorkflowStatus,
        text_spec(
            ToolName::CheckWorkflowStatus,
            CHECK_DESCRIPTION,
            schema_for!(CheckWorkflowStatusInput),
        ),
        OutputShape::Text,
        Arc::new(CheckWorkflowStatus),
    );
    super::register_tool(
        registry,
        ToolName::CancelWorkflow,
        text_spec(
            ToolName::CancelWorkflow,
            CANCEL_DESCRIPTION,
            schema_for!(CancelWorkflowInput),
        ),
        OutputShape::Text,
        Arc::new(CancelWorkflow),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use crate::testsupport::metadata;

    use super::*;

    fn obj(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[tokio::test]
    async fn workflow_controls_reject_empty_ids() {
        let ctx = metadata();

        for input in [
            obj(&[("workflow_id", json!(""))]),
            obj(&[
                ("workflow_id", json!("workflow-1")),
                ("workflow_task_id", json!("")),
            ]),
        ] {
            let res = CheckWorkflowStatus.execute(&input, &ctx).await.expect("ok");
            assert!(res.is_error);
            assert!(res.output.contains("workflow"), "{}", res.output);
        }

        let cancel = CancelWorkflow
            .execute(&obj(&[("workflow_task_id", json!(""))]), &ctx)
            .await
            .expect("ok");
        assert!(cancel.is_error);
        assert!(cancel.output.contains("workflow_task_id"));
    }
}
