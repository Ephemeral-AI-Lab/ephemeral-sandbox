//! Post-message assistant tool dispatch.

use std::collections::{BTreeMap, BTreeSet};

use eos_llm_client::ContentBlock;
use eos_tools::{
    execute_tool_once, lifecycle_batch_decision, reject_terminal_batch, DispatchCall,
    ExecutionMetadata, ToolName, ToolResult,
};
use eos_types::{JsonObject, ToolUseId};

use crate::events::StreamEvent;
use crate::query::QueryContext;
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
        ToolName::from_wire(&call.name)
            .and_then(|name| ctx.tool_registry.get(name))
            .filter(|tool| tool.is_terminal)
            .and_then(|_| results.get(call.tool_use_id.as_str()).cloned())
    })
}

fn metadata_for_call(ctx: &QueryContext, tool_use_id: &ToolUseId) -> ExecutionMetadata {
    let mut metadata = ctx.tool_metadata.clone();
    metadata.tool_use_id = Some(tool_use_id.clone());
    metadata
}

/// Dispatch a complete assistant tool batch.
///
/// # Errors
/// Returns [`EngineError`] for framework-level tool failures.
pub async fn dispatch_assistant_tools(
    ctx: &mut QueryContext,
    calls: &[ToolUseRequest],
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
            terminal_result: first_terminal_result(calls, &by_id, ctx),
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

    let mut events = Vec::new();
    let mut tool_results = Vec::new();
    let mut results_by_id = rejected.clone();

    for call in calls {
        if let Some(result) = rejected.get(call.tool_use_id.as_str()) {
            events.push(completed_event(call, result));
            tool_results.push(result_block(&call.tool_use_id, result));
            continue;
        }
        if !dispatched.contains(call.tool_use_id.as_str()) {
            continue;
        }

        let Some(name) = ToolName::from_wire(&call.name) else {
            let result = rejection_result(format!("Unknown tool `{}`.", call.name));
            events.push(completed_event(call, &result));
            tool_results.push(result_block(&call.tool_use_id, &result));
            results_by_id.insert(call.tool_use_id.as_str().to_owned(), result);
            continue;
        };
        let Some(tool) = ctx.tool_registry.get(name) else {
            let result = rejection_result(format!("Unknown tool `{}`.", call.name));
            events.push(completed_event(call, &result));
            tool_results.push(result_block(&call.tool_use_id, &result));
            results_by_id.insert(call.tool_use_id.as_str().to_owned(), result);
            continue;
        };

        events.push(started_event(call));
        let metadata = metadata_for_call(ctx, &call.tool_use_id);
        let result = execute_tool_once(tool, &call.input, &metadata).await?;
        if result.is_terminal {
            ctx.terminal_result = Some(result.clone());
        }
        events.push(completed_event(call, &result));
        tool_results.push(result_block(&call.tool_use_id, &result));
        results_by_id.insert(call.tool_use_id.as_str().to_owned(), result);
    }

    let terminal_result = ctx
        .terminal_result
        .clone()
        .or_else(|| first_terminal_result(calls, &results_by_id, ctx));

    Ok(AssistantToolDispatchOutcome {
        tool_results,
        terminal_result,
        events,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_llm_client::ToolSpec;
    use eos_tools::{
        OutputShape, RegisteredTool, ToolExecutor, ToolIntent, ToolRegistry, ToolResult,
    };
    use eos_types::{AgentRunId, JsonObject};
    use serde_json::json;

    use super::*;
    use crate::test_support::metadata;

    #[derive(Debug)]
    struct CountingExecutor {
        count: Arc<AtomicUsize>,
        result: ToolResult,
    }

    #[async_trait]
    impl ToolExecutor for CountingExecutor {
        async fn execute(
            &self,
            _input: &JsonObject,
            _ctx: &eos_tools::ExecutionMetadata,
        ) -> Result<ToolResult, eos_tools::ToolError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(self.result.clone())
        }
    }

    fn spec(name: ToolName) -> ToolSpec {
        ToolSpec::new(
            name.as_str(),
            "test",
            json!({"type":"object"})
                .as_object()
                .expect("object")
                .clone(),
            None,
        )
    }

    fn tool(name: ToolName, terminal: bool, count: Arc<AtomicUsize>) -> RegisteredTool {
        RegisteredTool::new(
            name,
            ToolIntent::ReadOnly,
            terminal,
            spec(name),
            OutputShape::Text,
            Arc::new(CountingExecutor {
                count,
                result: ToolResult::ok("ok"),
            }),
        )
    }

    fn ctx(registry: ToolRegistry) -> QueryContext {
        QueryContext {
            tool_registry: Arc::new(registry),
            cwd: PathBuf::new(),
            model: "m".to_owned(),
            system_prompt: String::new(),
            max_tokens: 1,
            tool_call_limit: 1,
            agent_name: "root".to_owned(),
            agent_run_id: AgentRunId::new_v4(),
            task_id: None,
            tool_calls_used: 0,
            text_only_no_terminal_turns: 0,
            tool_metadata: metadata(),
            enable_background_tasks: true,
            terminal_tools: BTreeSet::from([ToolName::SubmitRootOutcome]),
            exit_reason: None,
            terminal_result: None,
            event_source: None,
            prompt_report: None,
            notification_rules: Vec::new(),
            notification_fired: BTreeSet::new(),
            notification_state: JsonObject::new(),
        }
    }

    #[tokio::test]
    async fn terminal_batched_with_sibling_rejects_all() {
        let terminal_count = Arc::new(AtomicUsize::new(0));
        let sibling_count = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(tool(
            ToolName::SubmitRootOutcome,
            true,
            terminal_count.clone(),
        ));
        registry.register(tool(ToolName::ReadFile, false, sibling_count.clone()));
        let mut ctx = ctx(registry);

        let calls = vec![
            ToolUseRequest {
                tool_use_id: "toolu-1".parse().expect("valid id"),
                name: "submit_root_outcome".to_owned(),
                input: JsonObject::new(),
            },
            ToolUseRequest {
                tool_use_id: "toolu-2".parse().expect("valid id"),
                name: "read_file".to_owned(),
                input: JsonObject::new(),
            },
        ];
        let outcome = dispatch_assistant_tools(&mut ctx, &calls)
            .await
            .expect("dispatch");

        assert_eq!(terminal_count.load(Ordering::SeqCst), 0);
        assert_eq!(sibling_count.load(Ordering::SeqCst), 0);
        assert_eq!(outcome.tool_results.len(), 2);
        assert!(outcome
            .tool_results
            .iter()
            .all(|block| matches!(block, ContentBlock::ToolResult { is_error: true, .. })));
        assert!(outcome.terminal_result.is_some());
        assert!(outcome.events.iter().all(|event| matches!(
            event,
            StreamEvent::ToolExecutionCompleted { is_error: true, .. }
        )));
    }

    #[tokio::test]
    async fn foreground_fan_in_backpressure_preserves_final_results() {
        let first_count = Arc::new(AtomicUsize::new(0));
        let second_count = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(tool(ToolName::ReadFile, false, first_count.clone()));
        registry.register(tool(ToolName::Grep, false, second_count.clone()));
        let mut ctx = ctx(registry);
        ctx.terminal_tools = BTreeSet::new();

        let calls = vec![
            ToolUseRequest {
                tool_use_id: "toolu-1".parse().expect("valid id"),
                name: "read_file".to_owned(),
                input: JsonObject::new(),
            },
            ToolUseRequest {
                tool_use_id: "toolu-2".parse().expect("valid id"),
                name: "grep".to_owned(),
                input: JsonObject::new(),
            },
        ];
        let outcome = dispatch_assistant_tools(&mut ctx, &calls)
            .await
            .expect("dispatch");
        assert_eq!(first_count.load(Ordering::SeqCst), 1);
        assert_eq!(second_count.load(Ordering::SeqCst), 1);
        assert_eq!(outcome.tool_results.len(), 2);
        assert!(outcome.tool_results.iter().all(|block| matches!(
            block,
            ContentBlock::ToolResult {
                is_error: false,
                ..
            }
        )));
    }
}
