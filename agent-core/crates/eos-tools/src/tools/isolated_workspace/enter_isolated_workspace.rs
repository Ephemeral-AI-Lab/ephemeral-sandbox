//! The `enter_isolated_workspace` lifecycle tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_sandbox_api::{EnterIsolatedWorkspaceRequest, SandboxRequestBase};
use eos_types::JsonObject;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::{OutputShape, ToolResult};
use crate::registry::config::ToolConfigSet;
use crate::registry::spec::text_spec;
use crate::registry::ToolRegistry;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;
use crate::tools::SandboxToolService;

use super::{effective_layer_stack_root, render_enter_api_failure, render_enter_result};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct EnterIsolatedWorkspaceInput {
    #[serde(default)]
    layer_stack_root: String,
}

struct EnterIsolatedWorkspace {
    service: SandboxToolService,
}

impl EnterIsolatedWorkspace {
    fn new(service: SandboxToolService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for EnterIsolatedWorkspace {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: EnterIsolatedWorkspaceInput =
            match parse_input(ToolName::EnterIsolatedWorkspace, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        let sandbox_id = ctx.require_sandbox_id()?;
        let agent_run_id = ctx.require_agent_run_id()?;
        let request = EnterIsolatedWorkspaceRequest {
            base: SandboxRequestBase::new(agent_run_id.as_str(), "enter isolated workspace", None),
            layer_stack_root: effective_layer_stack_root(&parsed.layer_stack_root),
        };
        let result = match eos_sandbox_api::enter_isolated_workspace(
            &*self.service.transport,
            sandbox_id,
            &request,
        )
        .await
        {
            Ok(result) => result,
            Err(err) => return render_enter_api_failure(&err),
        };
        render_enter_result(&result)
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    sandbox_service: SandboxToolService,
) {
    let enter = config.get(ToolName::EnterIsolatedWorkspace);
    super::super::register_tool(
        registry,
        ToolName::EnterIsolatedWorkspace,
        enter,
        text_spec(
            ToolName::EnterIsolatedWorkspace,
            &enter.description,
            schema_for!(EnterIsolatedWorkspaceInput),
        ),
        OutputShape::Text,
        Arc::new(EnterIsolatedWorkspace::new(sandbox_service)),
    );
}
