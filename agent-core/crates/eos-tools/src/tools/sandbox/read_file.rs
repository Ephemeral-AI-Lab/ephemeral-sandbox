use async_trait::async_trait;
use eos_sandbox_api::ReadFileRequest;
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
use super::lib::outputs::ReadFileOutput;
use super::lib::{cwd, invalid_input, ok_json, request_base, resolve_path, MAX_READ_FILE_LINES};

fn default_one() -> u32 {
    1
}
fn default_max_read_lines() -> u32 {
    MAX_READ_FILE_LINES
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct ReadFileInput {
    file_path: String,
    #[serde(default = "default_one")]
    #[schemars(default = "default_one", range(min = 1))]
    start_line: u32,
    #[serde(default = "default_max_read_lines")]
    #[schemars(default = "default_max_read_lines", range(min = 1))]
    end_line: u32,
}

pub(super) struct ReadFile {
    service: SandboxToolService,
}

impl ReadFile {
    pub(super) fn new(service: SandboxToolService) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for ReadFile {
    async fn execute(
        &self,
        input: &JsonObject,
        ctx: &ExecutionMetadata,
    ) -> Result<ToolResult, ToolError> {
        let parsed: ReadFileInput = match parse_input(ToolName::ReadFile, input) {
            Ok(v) => v,
            Err(err) => return Ok(err),
        };
        // `default_end_line_to_window`: auto-window only when end_line is omitted.
        let end_line = if input.contains_key("end_line") {
            parsed.end_line
        } else {
            parsed
                .start_line
                .saturating_add(MAX_READ_FILE_LINES.saturating_sub(1))
        };
        if parsed.start_line == 0 {
            return Ok(invalid_input(ToolName::ReadFile, "start_line must be >= 1"));
        }
        if end_line == 0 {
            return Ok(invalid_input(ToolName::ReadFile, "end_line must be >= 1"));
        }
        if end_line < parsed.start_line {
            return Ok(invalid_input(
                ToolName::ReadFile,
                "end_line cannot be smaller than start_line",
            ));
        }
        if end_line - parsed.start_line + 1 > MAX_READ_FILE_LINES {
            return Ok(invalid_input(
                ToolName::ReadFile,
                format!("read_file can return at most {MAX_READ_FILE_LINES} lines"),
            ));
        }

        let path = resolve_path(ctx, &parsed.file_path);
        let sandbox_id = ctx.require_sandbox_id()?;
        let request = ReadFileRequest {
            base: request_base(ctx, &format!("read {path}"))?,
            path: path.clone(),
        };
        let result = match eos_sandbox_api::read_file(
            &*self.service.transport,
            sandbox_id,
            &request,
        )
        .await
        {
            Ok(result) => result,
            Err(err) => return Ok(ToolResult::error(err.to_string())),
        };
        if !result.base.success {
            return Ok(ToolResult::error(format!("Failed to read file: {path}")));
        }
        if !result.exists {
            return Ok(ToolResult::error(format!("Path does not exist: {path}")));
        }

        let output =
            build_read_file_output(ctx, &path, &result.content, parsed.start_line, end_line);
        Ok(ok_json(&output))
    }
}

/// `build_read_file_result`: window + `cat -n`-style line numbering done in the
/// tool over the daemon's full-file content.
fn build_read_file_output(
    ctx: &ExecutionMetadata,
    file_path: &str,
    content: &str,
    start_line: u32,
    end_line: u32,
) -> ReadFileOutput {
    let lines: Vec<&str> = if content.is_empty() {
        Vec::new()
    } else {
        content.split('\n').collect()
    };
    let total = lines.len() as u32;
    let start = start_line.max(1);
    let end = end_line.min(total);
    let mut rendered = Vec::new();
    if total > 0 && start <= end {
        for n in start..=end {
            rendered.push(format!("{n:4}: {}", lines[(n - 1) as usize]));
        }
    }
    ReadFileOutput {
        cwd: cwd(ctx),
        file_path: file_path.to_owned(),
        total_lines: total,
        start_line: start,
        end_line: end,
        content: rendered.join("\n"),
    }
}
