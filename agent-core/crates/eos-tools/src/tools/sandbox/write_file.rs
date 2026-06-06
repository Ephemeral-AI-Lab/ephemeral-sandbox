use std::collections::BTreeMap;

use async_trait::async_trait;
use eos_sandbox_api::WriteFileRequest;
use eos_types::JsonObject;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::ToolResult;
use crate::runtime::execution::parse_input;
use crate::runtime::executor::ToolExecutor;

use super::super::SandboxToolService;
use super::lib::outputs::MutationOutput;
use super::lib::{cwd, failure_status, mutation_result, request_base, resolve_path};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct WriteFileInput {
    file_path: String,
    content: String,
}

pub(super) struct WriteFile {
    service: SandboxToolService,
}

impl WriteFile {
    pub(super) fn new(service: SandboxToolService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for WriteFile {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: WriteFileInput = match parse_input(ToolName::WriteFile, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        let path = resolve_path(ctx, &parsed.file_path);
        let sandbox_id = ctx.require_sandbox_id()?;
        let request = WriteFileRequest {
            base: request_base(ctx, &format!("write {path}"))?,
            path: path.clone(),
            content: parsed.content.clone(),
            overwrite: true,
        };
        let result =
            match eos_sandbox_api::write_file(&*self.service.transport, sandbox_id, &request).await
            {
                Ok(result) => result,
                Err(err) => return Ok(ToolResult::error(err.to_string())),
            };
        let bytes = parsed.content.len() as u64;
        let output = MutationOutput {
            cwd: cwd(ctx),
            file_path: path,
            status: if result.base.success {
                "written".to_owned()
            } else {
                failure_status(result.base.conflict_reason.as_deref())
            },
            changed_paths: result.base.changed_paths,
            changed_path_kinds: result.changed_path_kinds,
            mutation_source: result.mutation_source,
            conflict_reason: result.base.conflict_reason,
            error: result.base.error.unwrap_or_default(),
            extra: BTreeMap::from([("bytes_written".to_owned(), json!(bytes))]),
        };
        Ok(mutation_result(result.base.success, output))
    }
}
