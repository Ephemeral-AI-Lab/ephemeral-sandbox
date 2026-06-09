//! The `submit_subagent_outcome` terminal tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::JsonObject;
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
pub(super) struct SubmitSubagentOutcomeInput {
    outcome: String,
}

struct SubmitSubagentOutcome;

#[async_trait]
impl ToolExecutor for SubmitSubagentOutcome {
    async fn execute(
        &self,
        input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: SubmitSubagentOutcomeInput =
            match parse_input(ToolName::SubmitSubagentOutcome, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        if is_blank(&parsed.outcome) {
            return Ok(ToolResult::error("outcome must be nonblank"));
        }
        Ok(
            ToolResult::ok(parsed.outcome.clone()).with_metadata(meta_obj(&[
                ("kind", json!("subagent")),
                ("outcome", json!(parsed.outcome)),
            ])),
        )
    }
}

pub(super) fn register(registry: &mut ToolRegistry, config: &ToolConfigSet) {
    let subagent = config.get(ToolName::SubmitSubagentOutcome);
    crate::tools::register_tool(
        registry,
        ToolName::SubmitSubagentOutcome,
        subagent,
        text_spec(
            ToolName::SubmitSubagentOutcome,
            &subagent.description,
            schema_for!(SubmitSubagentOutcomeInput),
        ),
        OutputShape::Text,
        Arc::new(SubmitSubagentOutcome),
    );
}

pub(super) fn register_schema(registry: &mut ToolRegistry, config: &ToolConfigSet) {
    let subagent = config.get(ToolName::SubmitSubagentOutcome);
    crate::tools::register_schema_tool(
        registry,
        ToolName::SubmitSubagentOutcome,
        subagent,
        text_spec(
            ToolName::SubmitSubagentOutcome,
            &subagent.description,
            schema_for!(SubmitSubagentOutcomeInput),
        ),
        OutputShape::Text,
    );
}
