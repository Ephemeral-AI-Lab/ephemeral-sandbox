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
pub(super) struct EditFileInput {
    file_path: String,
    #[serde(default = "default_empty")]
    old_text: String,
    #[serde(default = "default_empty")]
    new_text: String,
    #[serde(default = "default_false")]
    replace_all: bool,
    #[serde(default = "default_empty")]
    description: String,
}

pub(super) struct EditFile {
    service: SandboxToolService,
}

impl EditFile {
    pub(super) fn new(service: SandboxToolService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for EditFile {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: EditFileInput = match parse_input(ToolName::EditFile, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        if parsed.old_text.is_empty() {
            return Ok(ToolResult::error(
                "Provide `old_text` (text to find) and `new_text` (replacement).",
            ));
        }
        let path = resolve_path(ctx, &parsed.file_path);
        let sandbox_id = ctx.require_sandbox_id()?;
        let description = if parsed.description.is_empty() {
            format!("edit {path}")
        } else {
            parsed.description.clone()
        };
        let request = EditFileRequest {
            base: request_base(ctx, &description)?,
            path: path.clone(),
            edits: vec![SearchReplaceEdit {
                old_text: parsed.old_text,
                new_text: parsed.new_text,
                replace_all: parsed.replace_all,
            }],
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
        let applied = if result.base.success {
            u64::from(result.applied_edits)
        } else {
            0
        };
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
