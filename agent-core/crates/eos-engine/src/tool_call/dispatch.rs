//! Post-message assistant tool dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use eos_audit::TOOL_CALL_COMPLETED;
use eos_audit::{AuditEvent, AuditNode, AuditSource};
use eos_llm_client::{ContentBlock, Message};
use eos_tools::{
    execute_tool_once, lifecycle_batch_decision, reject_terminal_batch, run_pre_hooks,
    DispatchCall, ExecutionMetadata, RegisteredTool, ToolName, ToolResult,
};
use eos_types::{JsonObject, SystemClock, ToolUseId};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::query::QueryContext;
use crate::telemetry::StreamEvent;
use crate::EngineError;

/// One model-emitted tool request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUseRequest {
    /// Provider tool-use id.
    pub tool_use_id: ToolUseId,
    /// Raw tool name.
    pub name: String,
    /// Tool input.
    pub input: JsonObject,
}

/// Result of dispatching one assistant tool batch.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantToolDispatchOutcome {
    /// Tool-result blocks to append as the next user message.
    pub tool_results: Vec<ContentBlock>,
    /// First terminal result in the batch, when present.
    pub terminal_result: Option<ToolResult>,
    /// Engine stream events emitted during dispatch.
    pub events: Vec<StreamEvent>,
}

fn result_block(tool_use_id: &ToolUseId, result: &ToolResult) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: tool_use_id.clone(),
        content: result.output.clone(),
        is_error: result.is_error,
        metadata: result.metadata.clone(),
        is_terminal: result.is_terminal,
    }
}

fn completed_event(call: &ToolUseRequest, result: &ToolResult) -> StreamEvent {
    StreamEvent::ToolExecutionCompleted {
        agent_name: String::new(),
        agent_run_id: None,
        tool_name: call.name.clone(),
        output: result.output.clone(),
        is_error: result.is_error,
        tool_use_id: call.tool_use_id.clone(),
        metadata: result.metadata.clone(),
        is_terminal: result.is_terminal,
    }
}

fn started_event(call: &ToolUseRequest) -> StreamEvent {
    StreamEvent::ToolExecutionStarted {
        agent_name: String::new(),
        agent_run_id: None,
        tool_name: call.name.clone(),
        tool_input: call.input.clone(),
        tool_use_id: call.tool_use_id.clone(),
    }
}

fn rejection_result(message: impl Into<String>) -> ToolResult {
    ToolResult {
        output: message.into(),
        is_error: true,
        metadata: JsonObject::new(),
        is_terminal: false,
    }
}

fn first_terminal_result(
    calls: &[ToolUseRequest],
    results: &BTreeMap<String, ToolResult>,
    ctx: &QueryContext,
) -> Option<ToolResult> {
    calls.iter().find_map(|call| {
        ctx.tool_registry
            .get_wire(&call.name)
            .filter(|tool| tool.is_terminal)
            .and_then(|_| results.get(call.tool_use_id.as_str()))
            .filter(|result| result.is_terminal)
            .cloned()
    })
}

fn metadata_for_call(
    ctx: &QueryContext,
    conversation: &Arc<[Message]>,
    tool_use_id: &ToolUseId,
) -> ExecutionMetadata {
    let mut metadata = ctx.tool_metadata.clone();
    metadata.tool_use_id = Some(tool_use_id.clone());
    // Per-turn transcript snapshot (port of Python `context.conversation_messages`),
    // read by the stateless advisor-approval gate. Cheap Arc clone, not a copy.
    metadata.conversation = conversation.clone();
    metadata
}

#[derive(Debug)]
struct ForegroundCompletion {
    call: ToolUseRequest,
    result: ToolResult,
    duration_ms: f64,
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

async fn execute_foreground_tool(
    call: ToolUseRequest,
    tool: RegisteredTool,
    metadata: ExecutionMetadata,
) -> Result<ForegroundCompletion, EngineError> {
    let started = Instant::now();
    let result = execute_tool_once(&tool, &call.input, &metadata).await?;
    Ok(ForegroundCompletion {
        call,
        result,
        duration_ms: elapsed_ms(started),
    })
}

async fn dispatch_single_foreground_tool(
    ctx: &QueryContext,
    conversation: &Arc<[Message]>,
    call: ToolUseRequest,
    tool: RegisteredTool,
) -> Result<ForegroundCompletion, EngineError> {
    let metadata = metadata_for_call(ctx, conversation, &call.tool_use_id);
    execute_foreground_tool(call, tool, metadata).await
}

fn join_error(err: &tokio::task::JoinError) -> EngineError {
    EngineError::Internal(format!("foreground tool task failed: {err}"))
}

async fn dispatch_many_foreground_tools(
    ctx: &QueryContext,
    conversation: &Arc<[Message]>,
    runnable: Vec<(ToolUseRequest, RegisteredTool)>,
) -> Result<Vec<ForegroundCompletion>, EngineError> {
    let expected = runnable.len();
    let capacity = expected.saturating_mul(2).max(16);
    let (tx, mut rx) = mpsc::channel(capacity);
    let mut tasks = JoinSet::new();

    for (call, tool) in runnable {
        let metadata = metadata_for_call(ctx, conversation, &call.tool_use_id);
        let tx = tx.clone();
        tasks.spawn(async move {
            let completion = execute_foreground_tool(call, tool, metadata).await;
            let _ignored = tx.send(completion).await;
        });
    }
    drop(tx);

    let mut completions = Vec::with_capacity(expected);
    while completions.len() < expected {
        let Some(item) = rx.recv().await else {
            break;
        };
        match item {
            Ok(completion) => completions.push(completion),
            Err(err) => {
                tasks.abort_all();
                while let Some(joined) = tasks.join_next().await {
                    let _ignored = joined.map_err(|err| join_error(&err));
                }
                return Err(err);
            }
        }
    }

    while let Some(joined) = tasks.join_next().await {
        joined.map_err(|err| join_error(&err))?;
    }

    if completions.len() == expected {
        Ok(completions)
    } else {
        Err(EngineError::Internal(format!(
            "foreground fan-in lost final results: expected {expected}, got {}",
            completions.len()
        )))
    }
}

async fn dispatch_foreground_tools(
    ctx: &QueryContext,
    conversation: &Arc<[Message]>,
    runnable: Vec<(ToolUseRequest, RegisteredTool)>,
) -> Result<Vec<ForegroundCompletion>, EngineError> {
    match runnable.len() {
        0 => Ok(Vec::new()),
        1 => {
            let Some((call, tool)) = runnable.into_iter().next() else {
                return Err(EngineError::Internal(
                    "single foreground dispatch had no runnable call".to_owned(),
                ));
            };
            Ok(vec![
                dispatch_single_foreground_tool(ctx, conversation, call, tool).await?,
            ])
        }
        _ => dispatch_many_foreground_tools(ctx, conversation, runnable).await,
    }
}

/// Run an `ask_advisor` call: its pre-hooks (e.g. `BlockInIsolatedMode`) gate the
/// call, then — if they pass — the engine drives an advisor agent
/// (`advisor::run_advisor`). The advisor run is an engine primitive, so this is
/// the faithful Rust form of Python `ask_advisor` calling `run_agent`.
async fn run_advisor_call(
    ctx: &QueryContext,
    conversation: &Arc<[Message]>,
    call: ToolUseRequest,
    tool: RegisteredTool,
) -> Result<ForegroundCompletion, EngineError> {
    let metadata = metadata_for_call(ctx, conversation, &call.tool_use_id);
    let started = Instant::now();
    let result = if let Some(denial) = run_pre_hooks(&tool, &call.input, &metadata).await? {
        denial
    } else if let Some(handles) = ctx.run_handles.clone() {
        let tool_name = call
            .input
            .get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let tool_payload = call
            .input
            .get("tool_payload")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        crate::runtime::run_advisor(&handles, &metadata, conversation, tool_name, &tool_payload)
            .await
    } else {
        rejection_result(
            "ask_advisor is unavailable: the engine run handles are not wired for this run",
        )
    };
    Ok(ForegroundCompletion {
        call,
        result,
        duration_ms: elapsed_ms(started),
    })
}

fn publish_tool_completed(
    ctx: &QueryContext,
    call: &ToolUseRequest,
    result: &ToolResult,
    duration_ms: f64,
) {
    let Some(sink) = &ctx.audit else {
        return;
    };

    let mut node = AuditNode::builder()
        .agent_name(ctx.agent_name.clone())
        .agent_run_id(ctx.agent_run_id.clone())
        .tool_use_id(call.tool_use_id.clone());
    if let Some(request_id) = &ctx.tool_metadata.request_id {
        node = node.request_id(request_id.clone());
    }
    if let Some(task_id) = ctx
        .task_id
        .clone()
        .or_else(|| ctx.tool_metadata.task_id.clone())
    {
        node = node.task_id(task_id);
    }
    if let Some(sandbox_id) = &ctx.tool_metadata.sandbox_id {
        node = node.sandbox_id(sandbox_id.clone());
    }

    let mut section = JsonObject::new();
    section.insert("tool_name".to_owned(), json!(call.name));
    section.insert("duration_ms".to_owned(), json!(duration_ms));
    section.insert(
        "status".to_owned(),
        json!(if result.is_error { "error" } else { "ok" }),
    );
    section.insert("is_error".to_owned(), json!(result.is_error));
    section.insert("is_terminal".to_owned(), json!(result.is_terminal));

    let mut payload = JsonObject::new();
    payload.insert("tool_call".to_owned(), Value::Object(section));
    let event = AuditEvent::new(
        AuditSource::Engine,
        TOOL_CALL_COMPLETED,
        node.build(),
        payload,
        &SystemClock,
    );

    if let Err(err) = sink.publish(&event) {
        tracing::warn!(error = %err, tool_use_id = call.tool_use_id.as_str(), "tool-call obs publish failed");
    }
}

/// Dispatch a complete assistant tool batch.
///
/// # Errors
/// Returns [`EngineError`] for framework-level tool failures.
pub async fn dispatch_assistant_tools(
    ctx: &mut QueryContext,
    calls: &[ToolUseRequest],
    messages: &[Message],
) -> Result<AssistantToolDispatchOutcome, EngineError> {
    let dispatch_calls: Vec<DispatchCall<'_>> = calls
        .iter()
        .map(|call| DispatchCall {
            tool_use_id: call.tool_use_id.as_str(),
            name: &call.name,
        })
        .collect();

    if let Some(rejections) = reject_terminal_batch(&dispatch_calls, &ctx.tool_registry) {
        let mut by_id = BTreeMap::new();
        for rejection in rejections {
            by_id.insert(rejection.tool_use_id, rejection_result(rejection.message));
        }
        let mut events = Vec::with_capacity(calls.len());
        let mut tool_results = Vec::with_capacity(calls.len());
        for call in calls {
            if let Some(result) = by_id.get(call.tool_use_id.as_str()) {
                events.push(completed_event(call, result));
                tool_results.push(result_block(&call.tool_use_id, result));
            }
        }
        return Ok(AssistantToolDispatchOutcome {
            terminal_result: None,
            tool_results,
            events,
        });
    }

    let lifecycle = lifecycle_batch_decision(&dispatch_calls, &ctx.tool_registry);
    let dispatched: BTreeSet<&str> = lifecycle.dispatched.iter().map(String::as_str).collect();
    let mut rejected = BTreeMap::new();
    for rejection in lifecycle.rejected {
        rejected.insert(rejection.tool_use_id, rejection_result(rejection.message));
    }

    // One transcript snapshot for this dispatch; cloned (cheaply) into each tool's
    // metadata as `conversation` (the advisor gate's only input) and read by the
    // engine-driven `ask_advisor` run.
    let conversation: Arc<[Message]> = Arc::from(messages.to_vec());

    let mut events = Vec::new();
    let mut tool_results = Vec::new();
    let mut results_by_id = rejected.clone();
    let mut runnable = Vec::new();
    let mut advisor_runnable = Vec::new();

    for call in calls {
        if let Some(result) = rejected.get(call.tool_use_id.as_str()) {
            events.push(completed_event(call, result));
            tool_results.push(result_block(&call.tool_use_id, result));
            continue;
        }
        if !dispatched.contains(call.tool_use_id.as_str()) {
            continue;
        }

        let Some(tool) = ctx.tool_registry.get_wire(&call.name) else {
            let result = rejection_result(format!("Unknown tool `{}`.", call.name));
            events.push(completed_event(call, &result));
            tool_results.push(result_block(&call.tool_use_id, &result));
            results_by_id.insert(call.tool_use_id.as_str().to_owned(), result);
            continue;
        };

        events.push(started_event(call));
        // `ask_advisor` is engine-driven (an advisor agent), not a
        // generic foreground executor — route it out of the parallel fan-out.
        if tool.name.as_builtin() == Some(ToolName::AskAdvisor) {
            advisor_runnable.push((call.clone(), tool.clone()));
        } else {
            runnable.push((call.clone(), tool.clone()));
        }
    }

    let mut completions = dispatch_foreground_tools(ctx, &conversation, runnable).await?;
    for (call, tool) in advisor_runnable {
        completions.push(run_advisor_call(ctx, &conversation, call, tool).await?);
    }

    for completion in completions {
        events.push(completed_event(&completion.call, &completion.result));
        publish_tool_completed(
            ctx,
            &completion.call,
            &completion.result,
            completion.duration_ms,
        );
        tool_results.push(result_block(
            &completion.call.tool_use_id,
            &completion.result,
        ));
        results_by_id.insert(
            completion.call.tool_use_id.as_str().to_owned(),
            completion.result,
        );
    }

    let terminal_result = first_terminal_result(calls, &results_by_id, ctx);
    if terminal_result.is_some() {
        ctx.set_terminal_result(terminal_result.clone());
    }

    Ok(AssistantToolDispatchOutcome {
        tool_results,
        terminal_result,
        events,
    })
}

#[cfg(test)]
#[path = "../../tests/tool_call/dispatch/mod.rs"]
mod tests;
