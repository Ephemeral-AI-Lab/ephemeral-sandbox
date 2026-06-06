use std::collections::BTreeMap;

use eos_sandbox_api::{ExecCommandResult, SandboxRequestBase};
use eos_types::JsonObject;
use serde::Serialize;
use serde_json::{json, Value};

use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::core::name::ToolName;
use crate::core::result::ToolResult;

pub(super) mod outputs;
mod registration;

#[cfg(test)]
#[path = "../../../tests/tools/sandbox/mod.rs"]
mod tests;

use outputs::{CommandToolOutput, MutationOutput};

pub(super) const MAX_READ_FILE_LINES: u32 = 200;
pub(super) const MAX_YIELD_TIME_MS: u32 = 30_000;

pub(super) fn register(
    registry: &mut crate::registry::ToolRegistry,
    config: &crate::registry::config::ToolConfigSet,
    sandbox_service: super::super::SandboxToolService,
    command_service: super::super::CommandToolService,
) {
    registration::register(registry, config, sandbox_service, command_service);
}

pub(super) fn request_base(
    ctx: &ExecutionMetadata,
    description: &str,
) -> Result<SandboxRequestBase, ToolError> {
    let agent_run_id = ctx.require_agent_run_id()?;
    Ok(SandboxRequestBase::new(
        agent_run_id.as_str(),
        description,
        ctx.sandbox_invocation_id.clone(),
    ))
}

/// Absolute paths pass through; relative paths resolve under `workspace_root`.
pub(super) fn resolve_path(ctx: &ExecutionMetadata, path: &str) -> String {
    if path.starts_with('/') {
        return path.to_owned();
    }
    let workspace_root = ctx.workspace_root.trim();
    if workspace_root.is_empty() {
        path.to_owned()
    } else {
        format!("{}/{path}", workspace_root.trim_end_matches('/'))
    }
}

pub(super) fn cwd(ctx: &ExecutionMetadata) -> String {
    ctx.workspace_root.trim().to_owned()
}

pub(super) fn serialize_output<T: Serialize>(value: &T) -> Result<String, ToolResult> {
    serde_json::to_string(value)
        .map_err(|err| ToolResult::error(format!("failed to serialize tool output: {err}")))
}

pub(super) fn ok_json<T: Serialize>(value: &T) -> ToolResult {
    match serialize_output(value) {
        Ok(output) => ToolResult::ok(output),
        Err(result) => result,
    }
}

pub(super) fn invalid_input(tool: ToolName, message: impl std::fmt::Display) -> ToolResult {
    ToolResult::error(format!(
        "Invalid input for {}: {message}. Please retry the tool call with valid arguments.",
        tool.as_str()
    ))
}

pub(super) fn failure_status(conflict_reason: Option<&str>) -> String {
    match conflict_reason {
        Some("base_mismatch" | "version_conflict" | "drift") => "aborted_version",
        Some("lock_conflict" | "locked") => "aborted_lock",
        Some("not_found" | "missing") => "not_found",
        _ => "failed",
    }
    .to_owned()
}

pub(super) fn default_false() -> bool {
    false
}

pub(super) fn default_empty() -> String {
    String::new()
}

pub(super) fn mutation_result(success: bool, output: MutationOutput) -> ToolResult {
    let serialized = match serialize_output(&output) {
        Ok(output) => output,
        Err(result) => return result,
    };
    let mut result = if success {
        ToolResult::ok(serialized)
    } else {
        ToolResult::error(serialized)
    };
    result
        .metadata
        .insert("status".to_owned(), Value::String(output.status));
    result
}

pub(super) fn edit_output(
    ctx: &ExecutionMetadata,
    file_path: String,
    base: &eos_sandbox_api::SandboxResultBase,
    changed_path_kinds: BTreeMap<String, String>,
    mutation_source: String,
    applied_edits: u64,
) -> ToolResult {
    let output = MutationOutput {
        cwd: cwd(ctx),
        file_path,
        status: if base.success {
            "edited".to_owned()
        } else {
            failure_status(base.conflict_reason.as_deref())
        },
        changed_paths: base.changed_paths.clone(),
        changed_path_kinds,
        mutation_source,
        conflict_reason: base.conflict_reason.clone(),
        error: base.error.clone().unwrap_or_default(),
        extra: BTreeMap::from([("applied_edits".to_owned(), json!(applied_edits))]),
    };
    mutation_result(base.success, output)
}

pub(super) fn default_yield_ms() -> u32 {
    1000
}

pub(super) fn validate_command_timing(
    tool: ToolName,
    yield_time_ms: u32,
    timeout: Option<u32>,
    max_output_tokens: Option<u32>,
) -> Option<ToolResult> {
    if yield_time_ms > MAX_YIELD_TIME_MS {
        return Some(invalid_input(
            tool,
            format!("yield_time_ms must be <= {MAX_YIELD_TIME_MS}"),
        ));
    }
    if timeout == Some(0) {
        return Some(invalid_input(tool, "timeout must be >= 1"));
    }
    if max_output_tokens == Some(0) {
        return Some(invalid_input(tool, "max_output_tokens must be >= 1"));
    }
    None
}

/// Whether the daemon says the live session is gone, so the supervisor's stored
/// terminal result can be recovered.
pub(super) fn is_command_session_not_found(result: &ExecCommandResult) -> bool {
    result.is_session_not_found()
}

/// Project an [`ExecCommandResult`] into the completion payload the supervisor
/// stores.
pub(super) fn command_result_value(result: &ExecCommandResult) -> Value {
    json!({
        "status": result.status,
        "exit_code": result.exit_code,
        "output": {
            "stdout": result.output.stdout,
            "stderr": result.output.stderr,
        },
    })
}

/// Render a supervisor-stored terminal `result` value into the tool output DTO
/// (the recover-race return path).
pub(super) fn command_tool_result_from_value(result: &Value) -> ToolResult {
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("ok")
        .to_owned();
    let exit_code = result
        .get("exit_code")
        .and_then(Value::as_i64)
        .map(|code| code as i32);
    let stdout = result
        .get("output")
        .and_then(|output| output.get("stdout"))
        .and_then(Value::as_str)
        .or_else(|| result.get("stdout").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned();
    let stderr = result
        .get("output")
        .and_then(|output| output.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let is_error = eos_sandbox_api::KnownCommandStatus::is_error_raw(&status);
    let mut output_map = BTreeMap::new();
    output_map.insert("stdout".to_owned(), stdout.clone());
    output_map.insert("stderr".to_owned(), stderr.clone());
    let payload = CommandToolOutput {
        status: status.clone(),
        exit_code,
        output: output_map,
        command_session_id: None,
        stdout,
        stderr,
        changed_paths: Vec::new(),
        changed_path_kinds: BTreeMap::new(),
        mutation_source: String::new(),
        conflict_reason: None,
        error: None,
    };
    let mut metadata = JsonObject::new();
    metadata.insert("status".to_owned(), json!(status));
    ToolResult {
        output: match serialize_output(&payload) {
            Ok(output) => output,
            Err(result) => return result,
        },
        is_error,
        metadata,
        is_terminal: false,
    }
}

pub(super) fn command_tool_result(result: &ExecCommandResult) -> ToolResult {
    let is_error = result.is_error_status();
    let mut output_map = BTreeMap::new();
    output_map.insert("stdout".to_owned(), result.output.stdout.clone());
    output_map.insert("stderr".to_owned(), result.output.stderr.clone());
    let payload = CommandToolOutput {
        status: result.status.clone(),
        exit_code: result.exit_code,
        output: output_map,
        command_session_id: result.command_session_id.clone(),
        stdout: result.output.stdout.clone(),
        stderr: result.output.stderr.clone(),
        changed_paths: result.base.changed_paths.clone(),
        changed_path_kinds: result.changed_path_kinds.clone(),
        mutation_source: result.mutation_source.clone(),
        conflict_reason: result.base.conflict_reason.clone(),
        error: result.base.error.clone(),
    };
    let mut metadata = JsonObject::new();
    metadata.insert("status".to_owned(), json!(result.status));
    if let Some(id) = &result.command_session_id {
        metadata.insert("command_session_id".to_owned(), json!(id));
    }
    ToolResult {
        output: match serialize_output(&payload) {
            Ok(output) => output,
            Err(result) => return result,
        },
        is_error,
        metadata,
        is_terminal: false,
    }
}
