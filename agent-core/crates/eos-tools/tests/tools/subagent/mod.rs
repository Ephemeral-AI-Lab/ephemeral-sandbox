#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use eos_agent_run::{AgentRunApi, AgentRunError, AgentRunOutcome, SpawnAgentRequest};
use eos_llm_client::ContentBlock;
use eos_types::{AgentRunId, JsonObject, SubagentSessionId};
use serde_json::json;

use super::super::{
    cancel_subagent::CancelSubagent, check_subagent_progress::CheckSubagentProgress,
    run_subagent::RunSubagent,
};
use crate::ports::{
    CancelledSubagent, Sealed, SubagentProgress, SubagentSessionPort, SubagentSessionStatus,
};
use crate::runtime::executor::ToolExecutor;
use crate::support::metadata;

#[derive(Default)]
struct FakeBackgroundSession {
    spawned: Mutex<Vec<(String, String)>>,
}

impl Sealed for FakeBackgroundSession {}

#[async_trait]
impl AgentRunApi for FakeBackgroundSession {
    async fn spawn_agent(
        &self,
        request: SpawnAgentRequest,
    ) -> Result<AgentRunId, AgentRunError> {
        let prompt = request
            .initial_messages
            .first()
            .and_then(|message| message.content.first())
            .and_then(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        self.spawned
            .lock()
            .unwrap()
            .push((request.agent_name.as_str().to_owned(), prompt));
        Ok("agent-run-child".parse().unwrap())
    }

    async fn wait_for_agent_outcomes(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<AgentRunOutcome, AgentRunError> {
        Err(AgentRunError::NotActiveInProcess(agent_run_id.clone()))
    }

    async fn poll_agent_run_outcome(
        &self,
        _agent_run_id: &AgentRunId,
    ) -> Result<Option<AgentRunOutcome>, AgentRunError> {
        Ok(None)
    }

    async fn cancel_agent_run(
        &self,
        _agent_run_id: &AgentRunId,
        _reason: &str,
    ) -> Result<(), AgentRunError> {
        Ok(())
    }
}

#[async_trait]
impl SubagentSessionPort for FakeBackgroundSession {
    async fn register_background_session(
        &self,
        _agent_run_id: &AgentRunId,
        _agent_name: &str,
    ) -> SubagentSessionId {
        "subagent_1".parse().unwrap()
    }

    async fn subagent_session_snapshot(
        &self,
        subagent_session_id: &SubagentSessionId,
    ) -> Option<SubagentProgress> {
        Some(SubagentProgress::Found {
            subagent_session_id: subagent_session_id.clone(),
            status: SubagentSessionStatus::Running,
            agent_name: "explorer".to_owned(),
            result: None,
        })
    }

    async fn cancel_background_session(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> CancelledSubagent {
        CancelledSubagent::Cancelled {
            subagent_session_id: subagent_session_id.clone(),
            reason: reason.to_owned(),
        }
    }

    async fn cancel_background_agent_run(&self, agent_run_id: &AgentRunId, _reason: &str) -> bool {
        agent_run_id.as_str() == "agent-run-child"
    }

    async fn count_background_sessions(&self) -> usize {
        0
    }

    async fn cancel_all_background_sessions(&self, _reason: &str) {}

    async fn poll_complete_background_sessions(&self) -> usize {
        0
    }
}

fn obj(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[tokio::test]
async fn run_subagent_returns_agent_run_id() {
    let background = Arc::new(FakeBackgroundSession::default());
    let ctx = metadata();

    let res = RunSubagent::new(Some(background.clone()), Some(background.clone()))
        .execute(
            &obj(&[
                ("agent_name", json!("explorer")),
                ("prompt", json!("inspect the plan")),
            ]),
            &ctx,
        )
        .await
        .expect("ok");

    assert!(!res.is_error, "{}", res.output);
    assert!(res.output.contains("[SUBAGENT LAUNCHED]"), "{}", res.output);
    assert_eq!(res.metadata["agent_run_id"], json!("agent-run-child"));
    assert_eq!(res.metadata["status"], json!("running"));
    assert_eq!(
        background.spawned.lock().unwrap().as_slice(),
        &[("explorer".to_owned(), "inspect the plan".to_owned())]
    );
}

#[tokio::test]
async fn check_subagent_progress_rejects_out_of_range_last_n() {
    let background = Arc::new(FakeBackgroundSession::default());
    let ctx = metadata();

    for last_n in [0, 11] {
        let res = CheckSubagentProgress::new(Some(background.clone()))
            .execute(
                &obj(&[
                    ("subagent_session_id", json!("subagent_1")),
                    ("last_n_messages", json!(last_n)),
                ]),
                &ctx,
            )
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(res.output.contains("last_n_messages"), "{}", res.output);
    }
}

#[tokio::test]
async fn subagent_controls_reject_empty_session_id() {
    let ctx = metadata();
    let progress = CheckSubagentProgress::new(None)
        .execute(
            &obj(&[
                ("subagent_session_id", json!("")),
                ("last_n_messages", json!(5)),
            ]),
            &ctx,
        )
        .await
        .expect("ok");
    assert!(progress.is_error);
    assert!(progress.output.contains("subagent_session_id"));

    let cancel = CancelSubagent::new(None)
        .execute(
            &obj(&[("agent_run_id", json!("")), ("reason", json!("x"))]),
            &ctx,
        )
        .await
        .expect("ok");
    assert!(cancel.is_error);
    assert!(cancel.output.contains("agent_run_id"));
}
