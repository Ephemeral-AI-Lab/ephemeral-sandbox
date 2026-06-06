//! The `cancel_subagent` control tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::{JsonObject, SubagentSessionId};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::ports::BackgroundSupervisorPort;
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;

use super::lib::empty_subagent_session_error;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct CancelSubagentInput {
    subagent_session_id: SubagentSessionId,
    #[serde(default)]
    reason: String,
}

pub(in crate::tools::subagent) struct CancelSubagent {
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
}

impl CancelSubagent {
    pub(in crate::tools::subagent) fn new(
        background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
    ) -> Self {
        Self {
            background_supervisor,
        }
    }
}

#[async_trait]
impl ToolExecutor for CancelSubagent {
    async fn execute(
        &self,
        input: &JsonObject,
        _ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: CancelSubagentInput = match parse_input(ToolName::CancelSubagent, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.subagent_session_id.as_str().is_empty() {
            return Ok(empty_subagent_session_error(ToolName::CancelSubagent));
        }
        self.background_supervisor
            .as_deref()
            .ok_or(ToolError::MissingPort("background_supervisor"))?
            .cancel(&parsed.subagent_session_id, &parsed.reason)
            .await
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
) {
    let cancel = config.get(ToolName::CancelSubagent);
    super::super::register_tool(
        registry,
        ToolName::CancelSubagent,
        cancel,
        text_spec(
            ToolName::CancelSubagent,
            &cancel.description,
            schema_for!(CancelSubagentInput),
        ),
        OutputShape::Text,
        Arc::new(CancelSubagent::new(background_supervisor)),
    );
}
