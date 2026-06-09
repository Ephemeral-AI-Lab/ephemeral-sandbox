use std::collections::BTreeMap;
use std::sync::Arc;

use eos_types::{
    Attempt, AttemptFailReason, AttemptStage, ExecutionNode, Task, TaskId, TaskOutcome, TaskRole,
    TaskStatus, WorkItemId, WorkItemSpec, WorkerOutcomeSubmission,
};

use crate::{Result, WorkflowError};

use super::work_items::{planner_outcome_for_attempt, work_item_by_id};
use super::{AgentLaunch, AgentLaunchFactory, AgentRunReport, AttemptRun};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeState {
    Missing,
    Running,
    Passed,
    Failed,
}

/// Worker wave execution and settlement for one attempt.
pub(crate) struct WorkItemsRun {
    attempt_run: Arc<AttemptRun>,
}

impl WorkItemsRun {
    pub(crate) fn new(attempt_run: Arc<AttemptRun>) -> Self {
        Self { attempt_run }
    }

    pub(crate) async fn advance(&self) -> Result<()> {
        loop {
            let attempt = self.attempt_run.fresh_attempt().await?;
            if attempt.is_closed() {
                return Ok(());
            }
            if attempt.stage() != AttemptStage::Run {
                return Ok(());
            }
            let planner = planner_outcome_for_attempt(self.attempt_run.deps(), &attempt).await?;
            let states = self.node_states(&attempt).await?;
            if states.values().any(|state| *state == NodeState::Failed) {
                return self
                    .attempt_run
                    .close_attempt_failed(AttemptFailReason::TaskFailed)
                    .await;
            }
            if !states.is_empty() && states.values().all(|state| *state == NodeState::Passed) {
                return self.attempt_run.close_attempt_passed().await;
            }

            let running_count = states
                .values()
                .filter(|state| **state == NodeState::Running)
                .count();
            let capacity = self
                .attempt_run
                .deps()
                .max_concurrent_task_runs
                .saturating_sub(running_count);
            let ready = self.ready_unbound_nodes(&attempt, &states);
            if capacity == 0 || ready.is_empty() {
                if running_count == 0 && self.unbound_nodes_are_blocked(&attempt, &states) {
                    return self
                        .attempt_run
                        .close_attempt_failed(AttemptFailReason::TaskFailed)
                        .await;
                }
                return Ok(());
            }
            let mut spawned = 0usize;
            for node in ready.into_iter().take(capacity) {
                let work_item = work_item_by_id(&planner.work_items, &node.work_item_id)?.clone();
                self.spawn_worker(&attempt, &work_item).await?;
                spawned += 1;
            }
            if spawned == 0 {
                return Ok(());
            }
        }
    }

    pub(crate) async fn record_worker_outcome(
        &self,
        submission: WorkerOutcomeSubmission,
    ) -> Result<()> {
        self.attempt_run
            .assert_submission_attempt(&submission.attempt_id)?;
        let attempt = self.attempt_run.assert_stage(AttemptStage::Run).await?;
        let node = attempt
            .execution_tree
            .node(&submission.work_item_id)
            .ok_or_else(|| {
                WorkflowError::not_found("work item", submission.work_item_id.as_str())
            })?;
        if node.task_id.as_ref() != Some(&submission.task_id) {
            return Err(WorkflowError::invariant(format!(
                "worker submission task {:?} does not match work item {:?}",
                submission.task_id.as_str(),
                submission.work_item_id.as_str()
            )));
        }
        let task = self
            .attempt_run
            .deps()
            .task_store
            .get(&submission.task_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("worker task", submission.task_id.as_str()))?;
        if task.role != TaskRole::Worker || task.status != TaskStatus::Running {
            return Err(WorkflowError::invariant(format!(
                "worker task {:?} is not running",
                submission.task_id.as_str()
            )));
        }
        let task_status = if submission.status.is_pass() {
            TaskStatus::Done
        } else {
            TaskStatus::Failed
        };
        let task_outcome = TaskOutcome::Worker {
            is_pass: submission.status.is_pass(),
            outcome: submission.outcome,
        };
        self.attempt_run
            .deps()
            .task_store
            .set_task_status_if_current(
                &submission.task_id,
                TaskStatus::Running,
                task_status,
                Some(&task_outcome),
            )
            .await?
            .ok_or_else(|| {
                WorkflowError::invariant(format!(
                    "worker task {:?} is no longer running",
                    submission.task_id.as_str()
                ))
            })?;
        self.advance().await
    }

    async fn spawn_worker(&self, attempt: &Attempt, work_item: &WorkItemSpec) -> Result<()> {
        let task_id = TaskId::new_v4();
        let launch = AgentLaunchFactory::new(self.attempt_run.deps().clone())
            .for_worker(attempt, work_item, task_id.clone())
            .await?;
        self.attempt_run
            .deps()
            .task_store
            .insert_task(&Task {
                id: task_id.clone(),
                request_id: launch.request_id.clone(),
                role: TaskRole::Worker,
                instruction: launch.instruction.clone(),
                status: TaskStatus::Running,
                agent_name: Some(launch.agent_name.as_str().to_owned()),
                task_outcome: None,
            })
            .await?;
        self.attempt_run
            .deps()
            .attempt_store
            .bind_worker_task(&attempt.id, &work_item.id, &task_id)
            .await?;
        self.spawn_worker_run(launch);
        Ok(())
    }

    fn spawn_worker_run(&self, launch: AgentLaunch) {
        let attempt_run = Arc::clone(&self.attempt_run);
        let runner = self.attempt_run.deps().runner.clone();
        tokio::spawn(async move {
            let report = runner.run(launch.clone()).await;
            if let Err(err) = WorkItemsRun::new(attempt_run)
                .settle_worker(launch, report)
                .await
            {
                tracing::warn!(error = %err, "worker run could not be settled");
            }
        });
    }

    async fn settle_worker(
        &self,
        launch: AgentLaunch,
        report: Result<AgentRunReport>,
    ) -> Result<()> {
        let summary = match report {
            Ok(report) => report.failure_summary,
            Err(err) => Some(err.to_string()),
        };
        if let Some(summary) = summary {
            tracing::warn!(
                attempt_id = %self.attempt_run.attempt_id().as_str(),
                task_id = %launch.task_id.as_str(),
                %summary,
                "worker run reported a failure summary"
            );
        }
        let Some(task) = self
            .attempt_run
            .deps()
            .task_store
            .get(&launch.task_id)
            .await?
        else {
            return Err(WorkflowError::not_found(
                "worker task",
                launch.task_id.as_str(),
            ));
        };
        if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
            let work_item_id = launch.work_item_id().cloned().ok_or_else(|| {
                WorkflowError::invariant("worker settlement missing work_item_id")
            })?;
            self.synthesize_worker_failure(&launch.task_id, &work_item_id)
                .await?;
        }
        self.advance().await
    }

    async fn synthesize_worker_failure(
        &self,
        task_id: &TaskId,
        work_item_id: &WorkItemId,
    ) -> Result<()> {
        let task_outcome = TaskOutcome::Worker {
            is_pass: false,
            outcome: format!(
                "worker {:?} finished without submit_worker_outcome",
                work_item_id.as_str()
            ),
        };
        self.attempt_run
            .deps()
            .task_store
            .set_task_status_if_current(
                task_id,
                TaskStatus::Running,
                TaskStatus::Failed,
                Some(&task_outcome),
            )
            .await?;
        Ok(())
    }

    async fn node_states(&self, attempt: &Attempt) -> Result<BTreeMap<WorkItemId, NodeState>> {
        let mut states = BTreeMap::new();
        for node in &attempt.execution_tree.nodes {
            let state = match &node.task_id {
                Some(task_id) => self.task_state(task_id).await?,
                None => NodeState::Missing,
            };
            states.insert(node.work_item_id.clone(), state);
        }
        Ok(states)
    }

    async fn task_state(&self, task_id: &TaskId) -> Result<NodeState> {
        let task = self
            .attempt_run
            .deps()
            .task_store
            .get(task_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("worker task", task_id.as_str()))?;
        match task.status {
            TaskStatus::Pending | TaskStatus::Running => Ok(NodeState::Running),
            TaskStatus::Done => match task.task_outcome {
                Some(TaskOutcome::Worker { is_pass: true, .. }) => Ok(NodeState::Passed),
                Some(TaskOutcome::Worker { .. }) => Ok(NodeState::Failed),
                _ => Err(WorkflowError::invariant(format!(
                    "worker task {:?} is done without worker outcome",
                    task_id.as_str()
                ))),
            },
            TaskStatus::Failed | TaskStatus::Blocked | TaskStatus::Cancelled => {
                Ok(NodeState::Failed)
            }
        }
    }

    fn ready_unbound_nodes<'a>(
        &self,
        attempt: &'a Attempt,
        states: &BTreeMap<WorkItemId, NodeState>,
    ) -> Vec<&'a ExecutionNode> {
        attempt
            .execution_tree
            .nodes
            .iter()
            .filter(|node| {
                node.task_id.is_none()
                    && node
                        .needs
                        .iter()
                        .all(|need| states.get(need) == Some(&NodeState::Passed))
            })
            .collect()
    }

    fn unbound_nodes_are_blocked(
        &self,
        attempt: &Attempt,
        states: &BTreeMap<WorkItemId, NodeState>,
    ) -> bool {
        attempt
            .execution_tree
            .nodes
            .iter()
            .any(|node| node.task_id.is_none())
            && !attempt.execution_tree.nodes.iter().any(|node| {
                node.task_id.is_none()
                    && node
                        .needs
                        .iter()
                        .all(|need| states.get(need) == Some(&NodeState::Passed))
            })
    }
}
