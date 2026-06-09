use eos_types::Attempt;

use crate::attempt::AttemptResources;
use crate::{Result, WorkflowError};

use super::{AgentContext, ContextRole, ContextSection};

pub(crate) async fn render_planner_agent_context(
    deps: &AttemptResources,
    attempt: &Attempt,
    planner_task_id: &eos_types::TaskId,
) -> Result<AgentContext> {
    let iteration = deps
        .iteration_store
        .get(&attempt.iteration_id)
        .await?
        .ok_or_else(|| WorkflowError::not_found("iteration", attempt.iteration_id.as_str()))?;
    let workflow = deps
        .workflow_store
        .get(&attempt.workflow_id)
        .await?
        .ok_or_else(|| WorkflowError::not_found("workflow", attempt.workflow_id.as_str()))?;

    let prior_attempts = deps
        .attempt_store
        .list_for_iteration(&iteration.id)
        .await?
        .into_iter()
        .filter(|candidate| candidate.id != attempt.id)
        .map(|candidate| {
            ContextSection::new("attempt").with_attrs(vec![
                ("attempt_id".to_owned(), candidate.id.as_str().to_owned()),
                (
                    "sequence_no".to_owned(),
                    candidate.attempt_sequence_no.to_string(),
                ),
                ("status".to_owned(), candidate.status().as_str().to_owned()),
            ])
        })
        .collect::<Vec<_>>();

    Ok(AgentContext {
        role: ContextRole::Planner,
        sections: vec![
            ContextSection::new("workflow")
                .with_attrs(vec![
                    ("workflow_id".to_owned(), workflow.id.as_str().to_owned()),
                    (
                        "request_id".to_owned(),
                        workflow.request_id.as_str().to_owned(),
                    ),
                ])
                .with_children(vec![
                    ContextSection::new("workflow_goal").with_text(workflow.workflow_goal)
                ]),
            ContextSection::new("current_iteration")
                .with_attrs(vec![
                    ("iteration_id".to_owned(), iteration.id.as_str().to_owned()),
                    ("attempt_id".to_owned(), attempt.id.as_str().to_owned()),
                    (
                        "planner_task_id".to_owned(),
                        planner_task_id.as_str().to_owned(),
                    ),
                ])
                .with_children(vec![
                    ContextSection::new("workflow_goal").with_text(iteration.workflow_goal),
                    ContextSection::new("iteration_goal").with_text(iteration.iteration_goal),
                ]),
            ContextSection::new("prior_attempts").with_children(prior_attempts),
        ],
        directive: "Author a worker plan and finish with submit_plan_outcome.".to_owned(),
        context_limits: vec![
            "Do not execute work items yourself; create worker work_items.".to_owned(),
            "Use work_item_id values that are unique within this plan.".to_owned(),
        ],
    })
}
