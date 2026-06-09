use eos_types::{Attempt, TaskOutcome, WorkItemSpec};

use crate::attempt::{planner_outcome_for_attempt, AttemptResources};
use crate::{Result, WorkflowError};

use super::{AgentContext, ContextRole, ContextSection};

pub(crate) async fn render_worker_agent_context(
    deps: &AttemptResources,
    attempt: &Attempt,
    work_item: &WorkItemSpec,
    worker_task_id: &eos_types::TaskId,
) -> Result<AgentContext> {
    let planner = planner_outcome_for_attempt(deps, attempt).await?;
    let needs = dependency_sections(deps, attempt, work_item).await?;

    Ok(AgentContext {
        role: ContextRole::Worker,
        sections: vec![
            ContextSection::new("plan_spec").with_text(planner.plan_spec),
            ContextSection::new("work_item")
                .with_attrs(vec![
                    ("work_item_id".to_owned(), work_item.id.as_str().to_owned()),
                    ("task_id".to_owned(), worker_task_id.as_str().to_owned()),
                    (
                        "agent_name".to_owned(),
                        work_item.agent_name.as_str().to_owned(),
                    ),
                ])
                .with_children(vec![
                    ContextSection::new("work_spec").with_text(work_item.work_spec.clone())
                ]),
            ContextSection::new("needs").with_children(needs),
        ],
        directive: "Execute only this work item and finish with submit_worker_outcome.".to_owned(),
        context_limits: vec![
            "Use dependency outcomes as input context only.".to_owned(),
            "Do not report on work items outside this assignment.".to_owned(),
        ],
    })
}

async fn dependency_sections(
    deps: &AttemptResources,
    attempt: &Attempt,
    work_item: &WorkItemSpec,
) -> Result<Vec<ContextSection>> {
    let planner = planner_outcome_for_attempt(deps, attempt).await?;
    let mut sections = Vec::with_capacity(work_item.needs.len());
    for need in &work_item.needs {
        let node = attempt
            .execution_tree
            .node(need)
            .ok_or_else(|| WorkflowError::not_found("work item", need.as_str()))?;
        let task_id = node.task_id.as_ref().ok_or_else(|| {
            WorkflowError::invariant(format!(
                "dependency work item {:?} has no bound task",
                need.as_str()
            ))
        })?;
        let task = deps
            .task_store
            .get(task_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("task", task_id.as_str()))?;
        let TaskOutcome::Worker { is_pass, outcome } = task.task_outcome.ok_or_else(|| {
            WorkflowError::invariant(format!(
                "dependency worker task {:?} has no worker outcome",
                task_id.as_str()
            ))
        })?
        else {
            return Err(WorkflowError::invariant(format!(
                "dependency task {:?} did not record a worker outcome",
                task_id.as_str()
            )));
        };
        let work_spec = planner
            .work_items
            .iter()
            .find(|candidate| &candidate.id == need)
            .map(|item| item.work_spec.clone())
            .unwrap_or_default();
        sections.push(
            ContextSection::new("need")
                .with_attrs(vec![
                    ("work_item_id".to_owned(), need.as_str().to_owned()),
                    ("task_id".to_owned(), task_id.as_str().to_owned()),
                    ("is_pass".to_owned(), is_pass.to_string()),
                ])
                .with_children(vec![
                    ContextSection::new("work_spec").with_text(work_spec),
                    ContextSection::new("outcome").with_text(outcome),
                ]),
        );
    }
    Ok(sections)
}
