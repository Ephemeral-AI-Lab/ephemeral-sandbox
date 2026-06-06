#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use eos_llm_client::ToolSpec;
use eos_tools::{
    OutputShape, RegisteredTool, ToolExecutor, ToolIntent, ToolKey, ToolRegistry, ToolResult,
};
use eos_types::{AgentRunId, JsonObject};
use serde_json::json;
use tokio::sync::Barrier;
use tokio::time::{timeout, Duration};

use super::*;
use eos_testkit::metadata;

#[derive(Debug, Default)]
struct RecordingAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

impl RecordingAuditSink {
    fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().expect("audit lock").clone()
    }
}

impl eos_audit::AuditSink for RecordingAuditSink {
    fn publish(&self, event: &AuditEvent) -> Result<(), eos_audit::AuditError> {
        self.events.lock().expect("audit lock").push(event.clone());
        Ok(())
    }
}

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
    wire_spec(name.as_str())
}

fn wire_spec(name: &str) -> ToolSpec {
    ToolSpec::new(
        name,
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

fn dynamic_tool(name: &str, count: Arc<AtomicUsize>) -> RegisteredTool {
    RegisteredTool::new(
        ToolKey::dynamic(name),
        ToolIntent::ReadOnly,
        false,
        wire_spec(name),
        OutputShape::Text,
        Arc::new(CountingExecutor {
            count,
            result: ToolResult::ok("ok"),
        }),
    )
}

fn barrier_tool(name: ToolName, count: Arc<AtomicUsize>, barrier: Arc<Barrier>) -> RegisteredTool {
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
        reasoning_effort: None,
        tool_call_limit: 1,
        agent_name: "root".to_owned(),
        agent_run_id: AgentRunId::new_v4(),
        task_id: None,
        tool_calls_used: 0,
        text_only_no_terminal_turns: 0,
        tool_metadata: metadata(),
        terminal_tools: BTreeSet::from([ToolName::SubmitRootOutcome]),
        exit_reason: None,
        terminal_result: None,
        event_source: None,
        prompt_report: None,
        notification_rules: Vec::new(),
        notification_fired: BTreeSet::new(),
        notifier: crate::NotificationService::new(),
        audit: None,
        run_handles: None,
    }
}

#[tokio::test]
async fn dynamic_plugin_tool_dispatches_by_wire_name() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(dynamic_tool("lsp.hover", count.clone()));
    let mut ctx = ctx(registry);
    let audit = Arc::new(RecordingAuditSink::default());
    ctx.audit = Some(audit.clone());
    let agent_run_id = ctx.agent_run_id.clone();

    let calls = vec![ToolUseRequest {
        tool_use_id: "toolu-1".parse().expect("valid id"),
        name: "lsp.hover".to_owned(),
        input: JsonObject::new(),
    }];
    let outcome = dispatch_assistant_tools(&mut ctx, &calls, &[])
        .await
        .expect("dispatch");

    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(outcome.tool_results.len(), 1);
    assert!(matches!(
        &outcome.tool_results[0],
        ContentBlock::ToolResult {
            content,
            is_error: false,
            ..
        } if content == "ok"
    ));

    let events = audit.events();
    assert_eq!(events.len(), 1);
    let obs = events[0].to_obs_envelope();
    assert_eq!(obs.event_type, TOOL_CALL_COMPLETED);
    assert_eq!(obs.ids.agent_run_id.as_deref(), Some(agent_run_id.as_str()));
    assert_eq!(obs.ids.tool_use_id.as_deref(), Some("toolu-1"));
    assert_eq!(obs.payload["tool_call"]["tool_name"], json!("lsp.hover"));
    assert_eq!(obs.payload["tool_call"]["status"], json!("ok"));
    assert_eq!(obs.payload["tool_call"]["is_terminal"], json!(false));
    assert!(obs.payload["tool_call"]["duration_ms"]
        .as_f64()
        .is_some_and(|duration| duration >= 0.0));
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
    let outcome = dispatch_assistant_tools(&mut ctx, &calls, &[])
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
        ToolName::EditFile,
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
            name: "edit_file".to_owned(),
            input: JsonObject::new(),
        },
    ];
    let outcome = timeout(
        Duration::from_millis(200),
        dispatch_assistant_tools(&mut ctx, &calls, &[]),
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
    let outcome = dispatch_assistant_tools(&mut ctx, &calls, &[])
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
