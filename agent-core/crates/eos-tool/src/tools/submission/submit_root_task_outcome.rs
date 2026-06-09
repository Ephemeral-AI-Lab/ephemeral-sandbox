//! The `submit_root_task_outcome` terminal tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{JsonObject, RequestStatus, TaskOutcome, TaskRole, TaskStatus};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::registry::{text_spec, ToolConfigSet};
use crate::tools::{parse_input, RootSubmissionHandle};
use crate::{
    ExecutionMetadata, OutputShape, ToolError, ToolExecutor, ToolName, ToolRegistry, ToolResult,
};

use super::support::{is_blank, meta_obj};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubmitRootTaskOutcomeInput {
    status: eos_types::SubmissionStatus,
    outcome: String,
}

struct SubmitRootTaskOutcome {
    service: RootSubmissionHandle,
}

impl SubmitRootTaskOutcome {
    fn new(service: RootSubmissionHandle) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for SubmitRootTaskOutcome {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: SubmitRootTaskOutcomeInput =
            match parse_input(ToolName::SubmitRootTaskOutcome, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        if is_blank(&parsed.outcome) {
            return Ok(ToolResult::error("outcome must be nonblank"));
        }

        let request_id = ctx.require_request_id()?;
        let task_id = ctx.require_task_id()?;
        let task_store = self.service.submission.task_store()?;
        let request_store = self.service.submission.request_store()?;

        let task = match task_store.get(task_id).await? {
            Some(task) => task,
            None => {
                return Ok(ToolResult::error(format!(
                    "Root task '{}' was not found.",
                    task_id.as_str()
                )));
            }
        };
        if task.request_id != *request_id {
            return Ok(ToolResult::error(
                "Root task does not belong to this request.",
            ));
        }
        if task.role != TaskRole::Root {
            return Ok(ToolResult::error(format!(
                "Task '{}' is not a root task.",
                task_id.as_str()
            )));
        }
        if task.status != TaskStatus::Running {
            return Ok(ToolResult::error(format!(
                "Root task '{}' is not running.",
                task_id.as_str()
            )));
        }

        let task_status = if parsed.status.is_pass() {
            TaskStatus::Done
        } else {
            TaskStatus::Failed
        };
        let request_status = if parsed.status.is_pass() {
            RequestStatus::Done
        } else {
            RequestStatus::Failed
        };
        let outcome = TaskOutcome::Root {
            is_pass: parsed.status.is_pass(),
            outcome: parsed.outcome.clone(),
        };
        task_store
            .set_task_status_if_current(task_id, TaskStatus::Running, task_status, Some(&outcome))
            .await?
            .ok_or_else(|| {
                ToolError::Internal(format!(
                    "root task '{}' was closed before terminal submission was recorded",
                    task_id.as_str()
                ))
            })?;
        request_store
            .finish_request(request_id, request_status)
            .await?;

        Ok(
            ToolResult::ok(format!("Accepted root {}.", parsed.status.as_str())).with_metadata(
                meta_obj(&[
                    ("kind", json!("root")),
                    ("is_pass", json!(parsed.status.is_pass())),
                    ("outcome", json!(parsed.outcome)),
                    ("request_id", json!(request_id.as_str())),
                    ("task_id", json!(task_id.as_str())),
                ]),
            ),
        )
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    root_submission: RootSubmissionHandle,
) {
    let root = config.get(ToolName::SubmitRootTaskOutcome);
    crate::tools::register_tool(
        registry,
        ToolName::SubmitRootTaskOutcome,
        root,
        text_spec(
            ToolName::SubmitRootTaskOutcome,
            &root.description,
            schema_for!(SubmitRootTaskOutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitRootTaskOutcome::new(root_submission)),
    );
}

pub(super) fn register_schema(registry: &mut ToolRegistry, config: &ToolConfigSet) {
    let root = config.get(ToolName::SubmitRootTaskOutcome);
    crate::tools::register_schema_tool(
        registry,
        ToolName::SubmitRootTaskOutcome,
        root,
        text_spec(
            ToolName::SubmitRootTaskOutcome,
            &root.description,
            schema_for!(SubmitRootTaskOutcomeInput),
        ),
        OutputShape::Text,
    );
}
