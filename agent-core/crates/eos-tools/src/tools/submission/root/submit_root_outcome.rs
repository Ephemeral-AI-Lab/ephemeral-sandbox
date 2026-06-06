//! The `submit_root_outcome` terminal tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_state::{RequestStatus, TaskRole, TaskStatus};
use eos_types::JsonObject;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;
use crate::tools::RootSubmissionService;

use super::super::lib::{is_blank, meta_obj, SubmissionStatus};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct SubmitRootOutcomeInput {
    status: SubmissionStatus,
    outcome: String,
}

struct SubmitRootOutcome {
    service: Option<RootSubmissionService>,
}

impl SubmitRootOutcome {
    fn new(service: Option<RootSubmissionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for SubmitRootOutcome {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: SubmitRootOutcomeInput = match parse_input(ToolName::SubmitRootOutcome, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if is_blank(&parsed.outcome) {
            return Ok(ToolResult::error("outcome must be nonblank"));
        }

        let request_id = ctx.require_request_id()?;
        let task_id = ctx.require_task_id()?;
        let service = self
            .service
            .as_ref()
            .ok_or(ToolError::MissingPort("root_submission"))?;

        let task = match service.task_store.get(task_id).await? {
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
        if task.workflow_id.is_some() {
            return Ok(ToolResult::error(
                "submit_root_outcome is only valid for the root task.",
            ));
        }
        if task.role != TaskRole::Root {
            return Ok(ToolResult::error(format!(
                "Task '{}' is not a root task.",
                task_id.as_str()
            )));
        }

        let task_status = match parsed.status {
            SubmissionStatus::Success => TaskStatus::Done,
            SubmissionStatus::Failed => TaskStatus::Failed,
        };
        let request_status = match parsed.status {
            SubmissionStatus::Success => RequestStatus::Done,
            SubmissionStatus::Failed => RequestStatus::Failed,
        };
        let terminal = meta_obj(&[
            ("status", json!(parsed.status.as_str())),
            ("outcome", json!(parsed.outcome)),
        ]);
        service
            .task_store
            .set_task_status(task_id, task_status, None, Some(&terminal))
            .await?;
        service
            .request_store
            .finish_request(request_id, request_status)
            .await?;

        let kind = if parsed.status == SubmissionStatus::Success {
            "root_success"
        } else {
            "root_failure"
        };
        Ok(
            ToolResult::ok(format!("Accepted root {}.", parsed.status.as_str())).with_metadata(
                meta_obj(&[
                    ("submission_kind", json!(kind)),
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
    root_submission: Option<RootSubmissionService>,
) {
    let root = config.get(ToolName::SubmitRootOutcome);
    super::super::super::register_tool(
        registry,
        ToolName::SubmitRootOutcome,
        root,
        text_spec(
            ToolName::SubmitRootOutcome,
            &root.description,
            schema_for!(SubmitRootOutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitRootOutcome::new(root_submission)),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use eos_state::{RequestId, Task};
    use serde_json::json;

    use super::*;
    use crate::support::{metadata, FakeRequestStore, FakeTaskStore};

    fn obj(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn root_task(request_id: &RequestId) -> Task {
        Task {
            id: "root-1".parse().expect("id"),
            request_id: request_id.clone(),
            role: TaskRole::Root,
            instruction: "do the request".to_owned(),
            status: TaskStatus::Running,
            workflow_id: None,
            iteration_id: None,
            attempt_id: None,
            agent_name: Some("root".to_owned()),
            needs: Vec::new(),
            outcomes: Vec::new(),
            terminal_tool_result: None,
        }
    }

    fn root_metadata(request_id: RequestId) -> ExecutionMetadata {
        let mut ctx = metadata();
        ctx.request_id = Some(request_id);
        ctx.task_id = Some("root-1".parse().expect("id"));
        ctx
    }

    fn executor(
        task_store: Arc<FakeTaskStore>,
        request_store: Arc<FakeRequestStore>,
    ) -> SubmitRootOutcome {
        SubmitRootOutcome::new(Some(RootSubmissionService::new(task_store, request_store)))
    }

    #[tokio::test]
    async fn main_role_terminals() {
        let request_id: RequestId = RequestId::new_v4();
        let task_store = Arc::new(FakeTaskStore::new());
        task_store.put(root_task(&request_id));
        let request_store = Arc::new(FakeRequestStore::new());
        let ctx = root_metadata(request_id.clone());
        let executor = executor(task_store.clone(), request_store.clone());

        let res = executor
            .execute(
                &obj(&[("status", json!("success")), ("outcome", json!("all done"))]),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(!res.is_error, "{}", res.output);
        assert_eq!(res.metadata["submission_kind"], json!("root_success"));
        assert_eq!(
            request_store.finished(),
            vec![(request_id.as_str().to_owned(), RequestStatus::Done)]
        );

        let res = executor
            .execute(
                &obj(&[("status", json!("success")), ("outcome", json!("   "))]),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(res.output.contains("outcome must be nonblank"));

        let other = root_metadata(RequestId::new_v4());
        let res = executor
            .execute(
                &obj(&[("status", json!("failed")), ("outcome", json!("blocked"))]),
                &other,
            )
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(
            res.output.contains("does not belong to this request"),
            "{}",
            res.output
        );
    }

    #[tokio::test]
    async fn root_rejects_non_root_task() {
        let request_id = RequestId::new_v4();
        let task_store = Arc::new(FakeTaskStore::new());
        let mut task = root_task(&request_id);
        task.role = TaskRole::Generator;
        task_store.put(task);
        let request_store = Arc::new(FakeRequestStore::new());
        let ctx = root_metadata(request_id);
        let res = executor(task_store, request_store)
            .execute(
                &obj(&[("status", json!("success")), ("outcome", json!("x"))]),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(res.output.contains("is not a root task"), "{}", res.output);
    }
}
