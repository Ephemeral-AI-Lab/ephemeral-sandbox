#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use eos_types::{AgentRunId, JsonObject, SubagentSessionId, TaskId, WorkflowId, WorkflowSessionId};
use serde_json::json;

use super::super::{
    cancel_workflow::CancelWorkflow, check_workflow_status::CheckWorkflowStatus,
    delegate_workflow::DelegateWorkflow,
};
use crate::core::error::ToolError;
use crate::core::metadata::ExecutionMetadata;
use crate::ports::{
    BackgroundSupervisorPort, CancelledSubagent, OutstandingWorkflow, RunningBackgroundTasks,
    Sealed, SpawnedSubagent, StartedWorkflowHandle, SubagentLaunch, SubagentProgress,
    WorkflowControlPort,
};
use crate::runtime::executor::ToolExecutor;
use crate::support::metadata;

fn obj(pairs: &[(&str, serde_json::Value)]) -> JsonObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

#[derive(Default)]
struct RecordingSupervisor {
    workflows: Mutex<Vec<String>>,
    cancelled_workflows: Mutex<Vec<String>>,
}

impl Sealed for RecordingSupervisor {}

#[async_trait]
impl BackgroundSupervisorPort for RecordingSupervisor {
    async fn spawn(
        &self,
        _ctx: &ExecutionMetadata,
        _launch: SubagentLaunch,
    ) -> Result<SpawnedSubagent, ToolError> {
        unreachable!()
    }

    async fn progress(
        &self,
        _subagent_session_id: &SubagentSessionId,
        _last_n_messages: u8,
    ) -> Result<SubagentProgress, ToolError> {
        unreachable!()
    }

    async fn cancel(
        &self,
        _subagent_session_id: &SubagentSessionId,
        _reason: &str,
    ) -> Result<CancelledSubagent, ToolError> {
        unreachable!()
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
        self.running_background_tasks().await
    }

    async fn register_workflow(&self, workflow: &StartedWorkflowHandle) {
        self.workflows
            .lock()
            .unwrap()
            .push(workflow.workflow_task_id.as_str().to_owned());
    }

    async fn cancel_workflow_record(
        &self,
        workflow_task_id: &WorkflowSessionId,
        _reason: &str,
    ) -> bool {
        self.cancelled_workflows
            .lock()
            .unwrap()
            .push(workflow_task_id.as_str().to_owned());
        true
    }

    async fn teardown(
        &self,
        _workflow_control: Option<Arc<dyn WorkflowControlPort>>,
        _reason: &str,
    ) -> RunningBackgroundTasks {
        self.running_background_tasks().await
    }
}

struct OutstandingControl;

impl Sealed for OutstandingControl {}

#[async_trait]
impl WorkflowControlPort for OutstandingControl {
    async fn start(
        &self,
        _parent_task_id: &TaskId,
        _agent_run_id: &AgentRunId,
        _workflow_goal: &str,
    ) -> Result<StartedWorkflowHandle, ToolError> {
        unreachable!("outstanding short-circuit returns before start")
    }

    async fn status(
        &self,
        _workflow_id: &WorkflowId,
        _workflow_task_id: Option<&WorkflowSessionId>,
    ) -> Result<String, ToolError> {
        unreachable!()
    }

    async fn cancel(
        &self,
        _workflow_task_id: &WorkflowSessionId,
        _reason: &str,
    ) -> Result<String, ToolError> {
        unreachable!()
    }

    async fn find_outstanding(
        &self,
        _parent_task_id: &TaskId,
        _agent_run_id: &AgentRunId,
    ) -> Result<Vec<OutstandingWorkflow>, ToolError> {
        Ok(vec![OutstandingWorkflow {
            workflow_id: WorkflowId::new_v4(),
            workflow_task_id: WorkflowSessionId::new_v4(),
            workflow_goal: "prior goal".to_owned(),
        }])
    }

    async fn workflow_depth(&self, _workflow_id: &WorkflowId) -> Result<u32, ToolError> {
        Ok(1)
    }
}

#[tokio::test]
async fn delegate_workflow_outstanding_is_error() {
    let mut ctx = metadata();
    ctx.task_id = Some("parent".parse().unwrap());

    let res = DelegateWorkflow::new(
        Some(Arc::new(OutstandingControl)),
        Some(Arc::new(RecordingSupervisor::default())),
    )
    .execute(&obj(&[("goal", json!("do something"))]), &ctx)
    .await
    .expect("ok");

    assert!(res.is_error, "outstanding-workflow branch must be is_error");
    assert!(res.output.contains("already outstanding"), "{}", res.output);
}

struct StartingControl;

impl Sealed for StartingControl {}

#[async_trait]
impl WorkflowControlPort for StartingControl {
    async fn start(
        &self,
        _parent_task_id: &TaskId,
        _agent_run_id: &AgentRunId,
        _workflow_goal: &str,
    ) -> Result<StartedWorkflowHandle, ToolError> {
        Ok(StartedWorkflowHandle {
            workflow_id: WorkflowId::new_v4(),
            workflow_task_id: "wf_1".parse()?,
        })
    }

    async fn status(
        &self,
        _workflow_id: &WorkflowId,
        _workflow_task_id: Option<&WorkflowSessionId>,
    ) -> Result<String, ToolError> {
        unreachable!()
    }

    async fn cancel(
        &self,
        _workflow_task_id: &WorkflowSessionId,
        _reason: &str,
    ) -> Result<String, ToolError> {
        unreachable!()
    }

    async fn find_outstanding(
        &self,
        _parent_task_id: &TaskId,
        _agent_run_id: &AgentRunId,
    ) -> Result<Vec<OutstandingWorkflow>, ToolError> {
        Ok(Vec::new())
    }

    async fn workflow_depth(&self, _workflow_id: &WorkflowId) -> Result<u32, ToolError> {
        Ok(1)
    }
}

#[tokio::test]
async fn delegate_workflow_registers_background_record() {
    let supervisor = Arc::new(RecordingSupervisor::default());
    let mut ctx = metadata();
    ctx.task_id = Some("parent".parse().unwrap());

    let res = DelegateWorkflow::new(Some(Arc::new(StartingControl)), Some(supervisor.clone()))
        .execute(&obj(&[("goal", json!("do something"))]), &ctx)
        .await
        .expect("ok");

    assert!(!res.is_error, "{res:?}");
    assert_eq!(
        supervisor.workflows.lock().unwrap().as_slice(),
        ["wf_1"],
        "delegate_workflow must register the workflow as background work"
    );
}

#[tokio::test]
async fn workflow_controls_reject_empty_ids() {
    let ctx = metadata();

    for input in [
        obj(&[("workflow_id", json!(""))]),
        obj(&[
            ("workflow_id", json!("workflow-1")),
            ("workflow_task_id", json!("")),
        ]),
    ] {
        let res = CheckWorkflowStatus::new(None)
            .execute(&input, &ctx)
            .await
            .expect("ok");
        assert!(res.is_error);
        assert!(res.output.contains("workflow"), "{}", res.output);
    }

    let cancel = CancelWorkflow::new(None, None)
        .execute(&obj(&[("workflow_task_id", json!(""))]), &ctx)
        .await
        .expect("ok");
    assert!(cancel.is_error);
    assert!(cancel.output.contains("workflow_task_id"));
}
