//! Per-tool execution pipeline used by engine tool dispatch.

use eos_tool::{ExecutionMetadata, OutputShape, RegisteredTool, ToolError, ToolResult};
use eos_types::JsonObject;
use serde_json::{json, Value};

use super::hooks::{hook_failure_result, run_hook, HookOutcome, ToolCallHooks};

/// Run one tool call end-to-end.
///
/// Returns `Ok(ToolResult)` for normal execution, including in-band tool-domain
/// errors. Returns `Err(ToolError)` only on framework faults surfaced by hooks or
/// the executor body.
pub async fn execute_tool_once(
    tool: &RegisteredTool,
    raw_input: &JsonObject,
    ctx: &ExecutionMetadata,
    hooks: Option<&ToolCallHooks>,
) -> Result<ToolResult, ToolError> {
    if raw_input.contains_key("background") {
        return Ok(ToolResult::error(format!(
            "Invalid input for {}: `background` is not a tool argument. \
             Use typed subagent or command-session controls instead.",
            tool.name.as_str()
        )));
    }

    if let Some(denial) = run_pre_hooks(tool, raw_input, ctx, hooks).await? {
        return Ok(denial);
    }

    let result = tool.executor().execute(raw_input, ctx).await?;
    let result = validate_output(tool, result);
    Ok(stamp_terminal(tool, result))
}

/// Run a tool's ordered pre-hooks.
pub async fn run_pre_hooks(
    tool: &RegisteredTool,
    raw_input: &JsonObject,
    ctx: &ExecutionMetadata,
    hooks: Option<&ToolCallHooks>,
) -> Result<Option<ToolResult>, ToolError> {
    let mut hook_trace: Vec<Value> = Vec::new();
    for &hook in &tool.hooks {
        match run_hook(hook, raw_input, ctx, hooks).await? {
            HookOutcome::Pass(meta) => hook_trace.push(json!({
                "phase": "pre",
                "hook_name": hook.hook_name(),
                "status": "pass",
                "reason": "",
                "message": "",
                "metadata": meta,
            })),
            HookOutcome::Deny(denial) => {
                return Ok(Some(hook_failure_result(
                    hook,
                    &denial,
                    &hook_trace,
                    raw_input,
                )));
            }
        }
    }
    Ok(None)
}

fn validate_output(tool: &RegisteredTool, result: ToolResult) -> ToolResult {
    if result.is_error {
        return result;
    }
    match tool.output() {
        OutputShape::Text => result,
        OutputShape::Json {
            model_name,
            validate,
        } => match validate(&result.output) {
            Ok(()) => result,
            Err(err) => {
                let mut metadata = result.metadata;
                metadata.insert("output_validation_error".to_owned(), json!(err));
                ToolResult {
                    output: format!(
                        "Invalid output from {}: output did not match {model_name}: {err}.",
                        tool.name.as_str()
                    ),
                    is_error: true,
                    metadata,
                    is_terminal: false,
                }
            }
        },
    }
}

fn stamp_terminal(tool: &RegisteredTool, result: ToolResult) -> ToolResult {
    if tool.is_terminal && !result.is_error {
        ToolResult {
            is_terminal: true,
            ..result
        }
    } else {
        result
    }
}
