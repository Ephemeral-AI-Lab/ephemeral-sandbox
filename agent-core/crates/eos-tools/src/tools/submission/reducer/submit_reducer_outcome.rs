use std::sync::Arc;

use async_trait::async_trait;
use eos_state::ReducerSubmission;
use eos_types::JsonObject;
use schemars::schema_for;
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
use crate::tools::AttemptSubmissionService;

use super::super::lib::{
    is_blank, meta_obj, submission_ack_result, OutcomeInput, SubmissionStatus,
};

struct SubmitReducerOutcome {
    service: Option<AttemptSubmissionService>,
}

impl SubmitReducerOutcome {
    fn new(service: Option<AttemptSubmissionService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for SubmitReducerOutcome {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: OutcomeInput = match parse_input(ToolName::SubmitReducerOutcome, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if is_blank(&parsed.outcome) {
            return Ok(ToolResult::error("outcome must be nonblank"));
        }
        let attempt_id = ctx.require_attempt_id()?.clone();
        let task_id = ctx.require_task_id()?.clone();
        let submission = ReducerSubmission {
            attempt_id,
            task_id: task_id.clone(),
            status: parsed.status.outcome_status(),
            outcome: parsed.outcome.clone(),
            terminal_tool_result: JsonObject::new(),
        };
        let ack = self
            .service
            .as_ref()
            .ok_or(ToolError::MissingPort("attempt_submission"))?
            .port
            .apply_reducer(submission)
            .await?;
        Ok(submission_ack_result(
            ack,
            &format!("Accepted reducer {}.", parsed.status.as_str()),
            &meta_obj(&[
                (
                    "submission_kind",
                    json!(if parsed.status == SubmissionStatus::Success {
                        "reducer_success"
                    } else {
                        "reducer_failure"
                    }),
                ),
                ("task_id", json!(task_id.as_str())),
                (
                    "attempt_id",
                    json!(ctx.attempt_id.as_ref().map(eos_types::AttemptId::as_str)),
                ),
            ]),
        ))
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    attempt_submission: Option<AttemptSubmissionService>,
) {
    let reducer = config.get(ToolName::SubmitReducerOutcome);
    super::super::super::register_tool(
        registry,
        ToolName::SubmitReducerOutcome,
        reducer,
        text_spec(
            ToolName::SubmitReducerOutcome,
            &reducer.description,
            schema_for!(OutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitReducerOutcome::new(attempt_submission)),
    );
}
