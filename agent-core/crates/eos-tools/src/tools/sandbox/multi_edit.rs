use async_trait::async_trait;
use eos_sandbox_api::{EditFileRequest, SearchReplaceEdit};
use eos_types::JsonObject;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::ToolResult;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;

use super::super::SandboxToolService;
use super::lib::{default_empty, default_false, edit_output, request_base, resolve_path};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct MultiEditOp {
    old_text: String,
    #[serde(default = "default_empty")]
    new_text: String,
    #[serde(default = "default_false")]
    replace_all: bool,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct MultiEditInput {
    file_path: String,
    edits: Vec<MultiEditOp>,
    #[serde(default = "default_empty")]
    description: String,
}

pub(super) struct MultiEdit {
    service: SandboxToolService,
}

impl MultiEdit {
    pub(super) fn new(service: SandboxToolService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for MultiEdit {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: MultiEditInput = match parse_input(ToolName::MultiEdit, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.edits.is_empty() {
            return Ok(ToolResult::error("Provide at least one edit in `edits`."));
        }
        let path = resolve_path(ctx, &parsed.file_path);
        let sandbox_id = ctx.require_sandbox_id()?;
        let count = parsed.edits.len() as u64;
        let description = if parsed.description.is_empty() {
            format!("multi-edit {path}")
        } else {
            parsed.description.clone()
        };
        let request = EditFileRequest {
            base: request_base(ctx, &description)?,
            path: path.clone(),
            edits: parsed
                .edits
                .into_iter()
                .map(|op| SearchReplaceEdit {
                    old_text: op.old_text,
                    new_text: op.new_text,
                    replace_all: op.replace_all,
                })
                .collect(),
        };
        let result = match eos_sandbox_api::edit_file(
            &*self.service.transport,
            sandbox_id,
            &request,
        )
        .await
        {
            Ok(result) => result,
            Err(err) => return Ok(ToolResult::error(err.to_string())),
        };
        // multi_edit reports the count of edits submitted (not result.applied_edits).
        let applied = if result.base.success { count } else { 0 };
        Ok(edit_output(
            ctx,
            path,
            &result.base,
            result.changed_path_kinds,
            result.mutation_source,
            applied,
        ))
    }
}
