//! The `exit_isolated_workspace` lifecycle tool.

use std::sync::Arc;

use async_trait::async_trait;
use eos_sandbox_api::{ExitIsolatedWorkspaceRequest, SandboxRequestBase};
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

use super::{render_exit_api_failure, render_exit_result};

fn default_grace_s() -> f64 {
    5.0
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct ExitIsolatedWorkspaceInput {
    #[serde(default = "default_grace_s")]
    #[schemars(default = "default_grace_s")]
    grace_s: f64,
}

struct ExitIsolatedWorkspace {
    service: SandboxToolService,
}

impl ExitIsolatedWorkspace {
    fn new(service: SandboxToolService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for ExitIsolatedWorkspace {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: ExitIsolatedWorkspaceInput =
            match parse_input(ToolName::ExitIsolatedWorkspace, input) {
                Ok(v) => v,
                Err(err) => return Ok(err),
            };
        let sandbox_id = ctx.require_sandbox_id()?;
        let agent_run_id = ctx.require_agent_run_id()?;
        let request = ExitIsolatedWorkspaceRequest {
            base: SandboxRequestBase::new(agent_run_id.as_str(), "exit isolated workspace", None),
            grace_s: parsed.grace_s,
        };
        let result = match eos_sandbox_api::exit_isolated_workspace(
            &*self.service.transport,
            sandbox_id,
            &request,
        )
        .await
        {
            Ok(result) => result,
            Err(err) => return render_exit_api_failure(&err),
        };
        render_exit_result(&result)
    }
}

pub(super) fn register(
    registry: &mut ToolRegistry,
    config: &ToolConfigSet,
    sandbox_service: SandboxToolService,
) {
    let exit = config.get(ToolName::ExitIsolatedWorkspace);
    super::super::register_tool(
        registry,
        ToolName::ExitIsolatedWorkspace,
        exit,
        text_spec(
            ToolName::ExitIsolatedWorkspace,
            &exit.description,
            schema_for!(ExitIsolatedWorkspaceInput),
        ),
        OutputShape::Text,
        Arc::new(ExitIsolatedWorkspace::new(sandbox_service)),
    );
}
