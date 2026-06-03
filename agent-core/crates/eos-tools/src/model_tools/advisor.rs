//! The `ask_advisor` helper tool — a blocking read-only advisor audit of a
//! pending terminal submission.
//!
//! Only the model-facing spec/registration lives here. *Execution* is the engine
//! running an ephemeral advisor agent: `dispatch_assistant_tools` intercepts
//! `ToolName::AskAdvisor` and routes it to `eos_engine::advisor::run_advisor`
//! (advisor remediation plan §2a). `eos-tools` is upstream of `eos-engine`, so the
//! run cannot be driven from this crate; the executor below is therefore never
//! reached in the engine loop and exists only so the tool registers with a body.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::JsonObject;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::error::ToolError;
use crate::executor::ToolExecutor;
use crate::metadata::ExecutionMetadata;
use crate::name::ToolName;
use crate::registry::ToolRegistry;
use crate::result::{OutputShape, ToolResult};
use crate::spec::text_spec;

const ASK_ADVISOR_DESCRIPTION: &str = include_str!("descriptions/ask_advisor.md");

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct AskAdvisorInput {
    /// The terminal tool the caller intends to call.
    tool_name: String,
    /// The arguments the caller intends to pass.
    #[serde(default)]
    tool_payload: JsonObject,
}

struct AskAdvisor;

#[async_trait]
impl ToolExecutor for AskAdvisor {
    async fn execute(
        &self,
        _input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        // The engine dispatch loop intercepts `ask_advisor` and runs it as an
        // ephemeral advisor agent; this body is unreachable in the loop.
        Ok(ToolResult::error(
            "ask_advisor is dispatched by the engine and has no in-tool executor",
        ))
    }
}

pub(crate) fn register(registry: &mut ToolRegistry) {
    super::register_tool(
        registry,
        ToolName::AskAdvisor,
        text_spec(
            ToolName::AskAdvisor,
            ASK_ADVISOR_DESCRIPTION,
            schema_for!(AskAdvisorInput),
        ),
        OutputShape::Text,
        Arc::new(AskAdvisor),
    );
}
