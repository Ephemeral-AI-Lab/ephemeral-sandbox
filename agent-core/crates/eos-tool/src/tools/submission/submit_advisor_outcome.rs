//! The `submit_advisor_outcome` terminal tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{AdvisorVerdict, JsonObject};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::registry::{text_spec, ToolConfigSet};
use crate::tools::parse_input;
use crate::{
    ExecutionMetadata, OutputShape, ToolError, ToolExecutor, ToolName, ToolRegistry, ToolResult,
};

use super::support::{is_blank, meta_obj};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct SubmitAdvisorOutcomeInput {
    verdict: AdvisorVerdict,
    outcome: String,
}

struct SubmitAdvisorOutcome;

#[async_trait]
impl ToolExecutor for SubmitAdvisorOutcome {
    async fn execute(
        &self,
        input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: SubmitAdvisorOutcomeInput =
            match parse_input(ToolName::SubmitAdvisorOutcome, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        if is_blank(&parsed.outcome) {
            return Ok(ToolResult::error("outcome must be nonblank"));
        }
        Ok(
            ToolResult::ok(parsed.outcome.clone()).with_metadata(meta_obj(&[
                ("kind", json!("advisor")),
                ("verdict", json!(parsed.verdict)),
                ("outcome", json!(parsed.outcome)),
            ])),
        )
    }
}

pub(super) fn register(registry: &mut ToolRegistry, config: &ToolConfigSet) {
    let advisor = config.get(ToolName::SubmitAdvisorOutcome);
    crate::tools::register_tool(
        registry,
        ToolName::SubmitAdvisorOutcome,
        advisor,
        text_spec(
            ToolName::SubmitAdvisorOutcome,
            &advisor.description,
            schema_for!(SubmitAdvisorOutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitAdvisorOutcome),
    );
}

pub(super) fn register_schema(registry: &mut ToolRegistry, config: &ToolConfigSet) {
    let advisor = config.get(ToolName::SubmitAdvisorOutcome);
    crate::tools::register_schema_tool(
        registry,
        ToolName::SubmitAdvisorOutcome,
        advisor,
        text_spec(
            ToolName::SubmitAdvisorOutcome,
            &advisor.description,
            schema_for!(SubmitAdvisorOutcomeInput),
        ),
        OutputShape::Text,
    );
}
