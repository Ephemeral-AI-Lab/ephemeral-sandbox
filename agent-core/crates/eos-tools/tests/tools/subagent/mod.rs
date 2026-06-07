#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use eos_types::{JsonObject, SubagentSessionId, WorkflowSessionId};
use serde_json::json;

use super::super::{
    cancel_subagent::CancelSubagent, check_subagent_progress::CheckSubagentProgress,
    run_subagent::RunSubagent,
};
use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::ports::{
    BackgroundSupervisorPort, CancelledSubagent, RunningBackgroundTasks, Sealed, SpawnedSubagent,
    StartedSubagent, StartedWorkflowHandle, SubagentLaunch, SubagentProgress,
    SubagentProgressSnapshot, SubagentSessionStatus, WorkflowControlPort,
};
use crate::runtime::executor::ToolExecutor;
use crate::support::metadata;

#[derive(Default)]
struct FakeBackgroundSupervisor {
    spawned: Mutex<Vec<(String, String)>>,
}

impl Sealed for FakeBackgroundSupervisor {}

#[async_trait]
impl BackgroundSupervisorPort for FakeBackgroundSupervisor {
    async fn spawn(
        &self,
        _ctx: &ExecutionMetadata,
        launch: SubagentLaunch,
    ) -> Result<SpawnedSubagent, ToolError> {
        self.spawned
            .lock()
            .unwrap()
            .push((launch.agent_name, launch.prompt));
        Ok(SpawnedSubagent::Launched(StartedSubagent {
            subagent_session_id: "subagent_1".parse()?,
        }))
    }

    async fn progress(
        &self,
        subagent_session_id: &SubagentSessionId,
        _last_n_messages: u8,
    ) -> Result<SubagentProgress, ToolError> {
        Ok(SubagentProgress::Found(SubagentProgressSnapshot {
            subagent_session_id: subagent_session_id.clone(),
            status: SubagentSessionStatus::Running,
            agent_name: "explorer".to_owned(),
            result: None,
        }))
    }

    async fn cancel(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> Result<CancelledSubagent, ToolError> {
        Ok(CancelledSubagent::Cancelled {
            subagent_session_id: subagent_session_id.clone(),
            reason: reason.to_owned(),
        })
    }

    async fn running_background_tasks(&self) -> RunningBackgroundTasks {
        RunningBackgroundTasks {
            total: 0,
            subagents: 0,
            workflows: 0,
            command_sessions: 0,
        }
    }

    async fn cancel_subagents(&self) -> RunningBackgroundTasks {
        RunningBackgroundTasks {
            total: 0,
            subagents: 0,
            workflows: 0,
            command_sessions: 0,
        }
    }

    async fn register_workflow(&self, _workflow: &StartedWorkflowHandle) {}

    async fn cancel_workflow_record(
        &self,
        _workflow_task_id: &WorkflowSessionId,
        _reason: &str,
    ) -> bool {
        false
    }

    async fn teardown(
        &self,
        _workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        _reason: &str,
    ) -> RunningBackgroundTasks {
        RunningBackgroundTasks {
            total: 0,
            subagents: 0,
            workflows: 0,
            command_sessions: 0,
        }
    }
}

fn obj(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[tokio::test]
async fn run_subagent_returns_session_handle() {
    let supervisor = Arc::new(FakeBackgroundSupervisor::default());
    let ctx = metadata();

    let res = RunSubagent::new(Some(supervisor.clone()))
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
    assert_eq!(res.metadata["subagent_session_id"], json!("subagent_1"));
    assert_eq!(res.metadata["status"], json!("running"));
    assert_eq!(
        supervisor.spawned.lock().unwrap().as_slice(),
        &[("explorer".to_owned(), "inspect the plan".to_owned())]
    );
}

#[tokio::test]
async fn check_subagent_progress_rejects_out_of_range_last_n() {
    let supervisor = Arc::new(FakeBackgroundSupervisor::default());
    let ctx = metadata();

    for last_n in [0, 11] {
        let res = CheckSubagentProgress::new(Some(supervisor.clone()))
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
            &obj(&[("subagent_session_id", json!("")), ("reason", json!("x"))]),
            &ctx,
        )
        .await
        .expect("ok");
    assert!(cancel.is_error);
    assert!(cancel.output.contains("subagent_session_id"));
}
