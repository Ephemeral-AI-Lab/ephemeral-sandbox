//! Phase-6 acceptance-criteria tests for the composition root.
//!
//! In-crate (`#[cfg(test)]`) so they can read the `pub(crate)` DI graph fields
//! and shared test seams directly without widening the public API. Each test
//! names the AC it proves (impl-eos-runtime.md §11).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use eos_config::{DatabaseConfig, DatabaseUrl, ProvidersConfig, WorkflowConfig};
use eos_db::Database;
use eos_engine::{EngineError, EngineStream, EventSource, StreamEvent};
use eos_llm_client::{ContentBlock, LlmClient, LlmRequest, LlmStream, ProviderError};
use eos_tool::{Hook, ToolName};
use eos_types::{
    AgentDefinition, AgentRegistry, AgentRunId, AgentType, Page, RequestId, RequestListFilter,
    RequestStatus, Task, TaskRole, TaskStatus, WorkflowStatus,
};
use serde_json::json;

use crate::entry::root_task_id_for;
use crate::runtime_services::support::{build_test_state, build_test_state_with_message_records};
use crate::runtime_services::{EventCallback, EventSourceFactory};
use crate::{RequestOutcome, RequestRunInput, RuntimeServices};
use eos_testkit::{
    agent_def, factory_by_agent, factory_from, factory_root_blocks_after, test_tools_root,
    tool_use_turn,
};

fn root_agent() -> AgentDefinition {
    agent_def(
        "root",
        &["read_file", "delegate_workflow", "ask_advisor"],
        &["submit_root_outcome"],
    )
}

/// The advisor helper agent: read-only tools + the (ungated) advisor terminal.
/// Resolved by name in the engine-driven `ask_advisor` run.
fn advisor_agent() -> AgentDefinition {
    let mut def = agent_def("advisor", &["read_file"], &["submit_advisor_feedback"]);
    def.agent_type = AgentType::Advisor;
    def
}

#[derive(Debug)]
struct NoopLlmClient;

#[async_trait::async_trait]
impl LlmClient for NoopLlmClient {
    async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

fn planner_agent() -> AgentDefinition {
    let mut def = agent_def("planner", &["read_file"], &["submit_planner_outcome"]);
    def.context_recipe = Some("planner".to_owned());
    def
}

fn stream_of(events: Vec<StreamEvent>) -> EngineStream {
    Box::pin(futures::stream::iter(events.into_iter().map(Ok)))
}

type AgentTurns = Vec<Vec<StreamEvent>>;

fn one_step_workflow_turns() -> (AgentTurns, AgentTurns, AgentTurns) {
    let planner_payload = json!({
        "tasks": [{"id": "g1", "agent_name": "coder", "needs": []}],
        "task_specs": {"g1": "implement g1"},
        "reducers": [{"id": "r1", "needs": ["g1"], "prompt": "reduce g1"}],
    });
    let gen_payload = json!({"status": "success", "outcome": "generated g1"});
    let red_payload = json!({"status": "success", "outcome": "reduced"});

    (
        vec![
            tool_use_turn(
                "toolu_p_advise",
                "ask_advisor",
                json!({"tool_name": "submit_planner_outcome", "tool_payload": planner_payload.clone()}),
            ),
            tool_use_turn("toolu_p_submit", "submit_planner_outcome", planner_payload),
        ],
        vec![
            tool_use_turn(
                "toolu_g_advise",
                "ask_advisor",
                json!({"tool_name": "submit_generator_outcome", "tool_payload": gen_payload.clone()}),
            ),
            tool_use_turn("toolu_g_submit", "submit_generator_outcome", gen_payload),
        ],
        vec![
            tool_use_turn(
                "toolu_r_advise",
                "ask_advisor",
                json!({"tool_name": "submit_reducer_outcome", "tool_payload": red_payload.clone()}),
            ),
            tool_use_turn("toolu_r_submit", "submit_reducer_outcome", red_payload),
        ],
    )
}

fn sqlite_url(dir: &std::path::Path) -> String {
    format!("sqlite://{}", dir.join("t.db").display())
}

async fn run_request(
    services: &RuntimeServices,
    request_id: &RequestId,
    prompt: impl Into<String>,
    sandbox_id: Option<&str>,
    on_event: Option<EventCallback>,
) -> anyhow::Result<RequestOutcome> {
    let mut input = RequestRunInput::new(
        request_id.clone(),
        prompt,
        std::env::current_dir()?.display().to_string(),
        WorkflowConfig::default(),
    );
    if let Some(sandbox_id) = sandbox_id {
        input = input.with_sandbox_id(sandbox_id);
    }
    crate::run_request(services, input, on_event).await
}

mod background;
mod cancel;
mod delegation;
mod provisioning;
mod root_agent;
mod subagent;
