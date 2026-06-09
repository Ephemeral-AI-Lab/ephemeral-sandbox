#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use eos_types::WorkflowApi as _;
use eos_types::{AttemptStatus, CancelError, JsonObject, TaskStatus};
use serde_json::json;

use super::*;
use crate::support::{root_task, MemoryStores, QueueRunner};

/// A `CancelPort` fake mirroring `EngineCancelPort::cancel_task`'s persisted
/// flip; these store-level tests have no live-run registry.
struct TestCancelPort {
    task_store: Arc<dyn TaskStore>,
}

#[async_trait]
impl CancelPort for TestCancelPort {
    async fn cancel_task(
        &self,
        task_id: &eos_types::TaskId,
        reason: &str,
    ) -> Result<(), CancelError> {
        if let Some(task) = self.task_store.get(task_id).await? {
            if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
                let mut terminal = JsonObject::new();
                terminal.insert("fail_reason".to_owned(), "cancelled".into());
                terminal.insert("reason".to_owned(), reason.into());
                self.task_store
                    .set_task_status_if_current(
                        task_id,
                        task.status,
                        TaskStatus::Cancelled,
                        None,
                        Some(&terminal),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn cancel_agent_run(
        &self,
        _agent_run_id: &AgentRunId,
        _reason: &str,
    ) -> Result<(), CancelError> {
        Ok(())
    }
}

// `cancel_workflow` (by natural `WorkflowId`) decomposes through the delegated
// tree (workflow + iteration + attempt CANCELLED, active tasks CANCELLED with a
// `cancelled` marker, latched before close) without mutating the parent.
#[tokio::test]
async fn cancel_workflow_cancels_child_state_without_touching_parent() {
    let stores = Arc::new(MemoryStores::default());
    let runner = Arc::new(QueueRunner::default());
    let deps = stores.deps(runner);
    let parent = root_task("parent", TaskStatus::Running);
    stores.seed_task(parent.clone());
    let cancel_port: Arc<dyn CancelPort> = Arc::new(TestCancelPort {
        task_store: stores.clone(),
    });
    let service = WorkflowService::new(
        WorkflowStarter::new(deps),
        stores.clone(),
        stores.clone(),
        stores.clone(),
        stores.clone(),
        cancel_port,
    );

    let agent_run_id: AgentRunId = "agent-run-1".parse().expect("agent run id");
    let started = service
        .start_workflow(StartWorkflowRequest {
            parent_task_id: parent.id.clone(),
            agent_run_id,
            workflow_goal: "delegated goal".to_owned(),
        })
        .await
        .unwrap();

    service
        .cancel_workflow(&started.workflow_id, "stop now")
        .await
        .unwrap();

    let workflow = stores.workflow(&started.workflow_id).unwrap();
    assert_eq!(workflow.status, WorkflowStatus::Cancelled);
    let iteration_id = workflow.iteration_ids.first().unwrap();
    let iteration = stores.iteration(iteration_id).unwrap();
    assert_eq!(iteration.status, IterationStatus::Cancelled);
    let attempt_id = iteration.attempt_ids.first().unwrap();
    let attempt = stores.attempt(attempt_id).unwrap();
    assert_eq!(attempt.status(), AttemptStatus::Cancelled);
    assert!(attempt.fail_reason().is_none());
    let planner_task = stores.task(attempt.planner_task_id().unwrap()).unwrap();
    assert_eq!(planner_task.status, TaskStatus::Cancelled);
    assert_eq!(
        planner_task
            .terminal_tool_result
            .unwrap()
            .get("fail_reason"),
        Some(&json!("cancelled"))
    );
    // `cancel_workflow` must never mutate the parent task (anchor §3).
    assert_eq!(stores.task(&parent.id).unwrap().status, TaskStatus::Running);
}
