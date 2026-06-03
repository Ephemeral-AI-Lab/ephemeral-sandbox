//! Post-message assistant tool dispatch.

use std::collections::{BTreeMap, BTreeSet};

use eos_llm_client::ContentBlock;
use eos_tools::{
    execute_tool_once, lifecycle_batch_decision, reject_terminal_batch, DispatchCall,
    ExecutionMetadata, RegisteredTool, ToolName, ToolResult,
};
use eos_types::{JsonObject, ToolUseId};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

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
            .and_then(|_| results.get(call.tool_use_id.as_str()))
            .filter(|result| result.is_terminal)
            .cloned()
    })
}

fn metadata_for_call(ctx: &QueryContext, tool_use_id: &ToolUseId) -> ExecutionMetadata {
    let mut metadata = ctx.tool_metadata.clone();
    metadata.tool_use_id = Some(tool_use_id.clone());
    metadata
}

#[derive(Debug)]
struct ForegroundCompletion {
    call: ToolUseRequest,
    result: ToolResult,
}

async fn execute_foreground_tool(
    call: ToolUseRequest,
    tool: RegisteredTool,
    metadata: ExecutionMetadata,
) -> Result<ForegroundCompletion, EngineError> {
    let result = execute_tool_once(&tool, &call.input, &metadata).await?;
    Ok(ForegroundCompletion { call, result })
}

async fn dispatch_single_foreground_tool(
    ctx: &QueryContext,
    call: ToolUseRequest,
    tool: RegisteredTool,
) -> Result<ForegroundCompletion, EngineError> {
    let metadata = metadata_for_call(ctx, &call.tool_use_id);
    execute_foreground_tool(call, tool, metadata).await
}

fn join_error(err: &tokio::task::JoinError) -> EngineError {
    EngineError::Internal(format!("foreground tool task failed: {err}"))
}

async fn dispatch_many_foreground_tools(
    ctx: &QueryContext,
    runnable: Vec<(ToolUseRequest, RegisteredTool)>,
) -> Result<Vec<ForegroundCompletion>, EngineError> {
    let expected = runnable.len();
    let capacity = expected.saturating_mul(2).max(16);
    let (tx, mut rx) = mpsc::channel(capacity);
    let mut tasks = JoinSet::new();

    for (call, tool) in runnable {
        let metadata = metadata_for_call(ctx, &call.tool_use_id);
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
                dispatch_single_foreground_tool(ctx, call, tool).await?,
            ])
        }
        _ => dispatch_many_foreground_tools(ctx, runnable).await,
    }
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

    let mut events = Vec::new();
    let mut tool_results = Vec::new();
    let mut results_by_id = rejected.clone();
    let mut runnable = Vec::new();

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
        runnable.push((call.clone(), tool.clone()));
    }

    for completion in dispatch_foreground_tools(ctx, runnable).await? {
        if completion.result.is_terminal {
            ctx.terminal_result = Some(completion.result.clone());
        }
        events.push(completed_event(&completion.call, &completion.result));
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
        ctx.terminal_result = terminal_result.clone();
    }

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
    use tokio::sync::Barrier;
    use tokio::time::{timeout, Duration};

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

    #[derive(Debug)]
    struct BarrierExecutor {
        count: Arc<AtomicUsize>,
        barrier: Arc<Barrier>,
    }

    #[async_trait]
    impl ToolExecutor for BarrierExecutor {
        async fn execute(
            &self,
            _input: &JsonObject,
            _ctx: &eos_tools::ExecutionMetadata,
        ) -> Result<ToolResult, eos_tools::ToolError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.barrier.wait().await;
            Ok(ToolResult::ok("ok"))
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

    fn tool_with_result(
        name: ToolName,
        terminal: bool,
        count: Arc<AtomicUsize>,
        result: ToolResult,
    ) -> RegisteredTool {
        RegisteredTool::new(
            name,
            ToolIntent::ReadOnly,
            terminal,
            spec(name),
            OutputShape::Text,
            Arc::new(CountingExecutor { count, result }),
        )
    }

    fn tool(name: ToolName, terminal: bool, count: Arc<AtomicUsize>) -> RegisteredTool {
        tool_with_result(name, terminal, count, ToolResult::ok("ok"))
    }

    fn barrier_tool(
        name: ToolName,
        count: Arc<AtomicUsize>,
        barrier: Arc<Barrier>,
    ) -> RegisteredTool {
        RegisteredTool::new(
            name,
            ToolIntent::ReadOnly,
            false,
            spec(name),
            OutputShape::Text,
            Arc::new(BarrierExecutor { count, barrier }),
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
            notifier: crate::NotificationService::new(),
            run_handles: None,
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
        assert!(outcome.terminal_result.is_none());
        assert!(outcome.events.iter().all(|event| matches!(
            event,
            StreamEvent::ToolExecutionCompleted { is_error: true, .. }
        )));
    }

    #[tokio::test]
    async fn foreground_multi_tool_batch_runs_in_parallel_and_preserves_final_results() {
        let first_count = Arc::new(AtomicUsize::new(0));
        let second_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let mut registry = ToolRegistry::new();
        registry.register(barrier_tool(
            ToolName::ReadFile,
            first_count.clone(),
            barrier.clone(),
        ));
        registry.register(barrier_tool(
            ToolName::Grep,
            second_count.clone(),
            barrier.clone(),
        ));
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
        let outcome = timeout(
            Duration::from_millis(200),
            dispatch_assistant_tools(&mut ctx, &calls),
        )
        .await
        .expect("parallel foreground dispatch timed out")
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

    #[tokio::test]
    async fn terminal_tool_error_does_not_project_terminal_result() {
        let terminal_count = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(tool_with_result(
            ToolName::SubmitRootOutcome,
            true,
            terminal_count.clone(),
            ToolResult::error("validation failed"),
        ));
        let mut ctx = ctx(registry);

        let calls = [ToolUseRequest {
            tool_use_id: "toolu-1".parse().expect("valid id"),
            name: "submit_root_outcome".to_owned(),
            input: JsonObject::new(),
        }];
        let outcome = dispatch_assistant_tools(&mut ctx, &calls)
            .await
            .expect("dispatch");

        assert_eq!(terminal_count.load(Ordering::SeqCst), 1);
        assert!(ctx.terminal_result.is_none());
        assert!(outcome.terminal_result.is_none());
        assert!(matches!(
            outcome.tool_results.first(),
            Some(ContentBlock::ToolResult {
                is_error: true,
                is_terminal: false,
                ..
            })
        ));
    }
}
