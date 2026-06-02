use std::sync::Arc;

use eos_state::{
    attempt_execution_outcomes, Attempt, AttemptStore, ExecutionRole, ExecutionTaskOutcome,
    IterationStore, TaskOutcomeStatus, TaskRole, TaskStatus, TaskStore, WorkflowStore,
};

use crate::{Result, WorkflowError};

use super::xml::render_task_outcome;
use super::{AgentContext, ContextRole, ContextScope, ContextSection};

/// Store bundle consumed by context builders.
#[derive(Clone)]
pub struct ContextEngineDeps {
    /// Workflow store.
    pub workflow_store: Arc<dyn WorkflowStore>,
    /// Iteration store.
    pub iteration_store: Arc<dyn IterationStore>,
    /// Attempt store.
    pub attempt_store: Arc<dyn AttemptStore>,
    /// Task store.
    pub task_store: Arc<dyn TaskStore>,
}

impl std::fmt::Debug for ContextEngineDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextEngineDeps").finish_non_exhaustive()
    }
}

/// Role-scoped context packet builder.
#[derive(Clone)]
pub struct ContextEngine {
    deps: ContextEngineDeps,
}

impl std::fmt::Debug for ContextEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextEngine").finish_non_exhaustive()
    }
}

impl ContextEngine {
    /// Create a context engine.
    #[must_use]
    pub fn new(deps: ContextEngineDeps) -> Self {
        Self { deps }
    }

    /// Build the role context for `scope`.
    ///
    /// # Errors
    /// Returns [`WorkflowError`] if the recipe is unknown/mismatched or required
    /// persisted state cannot be loaded.
    pub async fn build(&self, recipe_id: &str, scope: &ContextScope) -> Result<AgentContext> {
        validate_context_recipe(recipe_id, scope.role)?;
        match scope.role {
            ContextRole::Planner => self.build_planner_context(scope).await,
            ContextRole::Generator => {
                self.build_execution_context(scope, ContextRole::Generator)
                    .await
            }
            ContextRole::Reducer => {
                self.build_execution_context(scope, ContextRole::Reducer)
                    .await
            }
        }
    }

    async fn build_planner_context(&self, scope: &ContextScope) -> Result<AgentContext> {
        let workflow_id = scope.workflow_id()?;
        let iteration_id = scope.iteration_id()?;
        let attempt_id = scope.attempt_id()?;
        let workflow = self
            .deps
            .workflow_store
            .get(workflow_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("workflow", workflow_id.as_str()))?;
        let iteration = self
            .deps
            .iteration_store
            .get(iteration_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("iteration", iteration_id.as_str()))?;
        let current_attempt = self
            .deps
            .attempt_store
            .get(attempt_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("attempt", attempt_id.as_str()))?;

        let mut workflow_children =
            vec![ContextSection::new("goal").with_text(workflow.workflow_goal)];
        let prior_iterations = self
            .prior_iteration_sections(&workflow.id, iteration.sequence_no)
            .await?;
        if !prior_iterations.is_empty() {
            workflow_children
                .push(ContextSection::new("prior_iterations").with_children(prior_iterations));
        }

        let mut current_children =
            vec![ContextSection::new("goal").with_text(iteration.iteration_goal)];
        let previous_attempts = self.previous_attempt_sections(&current_attempt).await?;
        if !previous_attempts.is_empty() {
            current_children
                .push(ContextSection::new("previous_attempts").with_children(previous_attempts));
        }
        workflow_children.push(
            ContextSection::new("current_iteration")
                .with_attrs(vec![(
                    "sequence".to_owned(),
                    iteration.sequence_no.to_string(),
                )])
                .with_children(current_children),
        );

        Ok(AgentContext {
            role: ContextRole::Planner,
            sections: vec![ContextSection::new("workflow").with_children(workflow_children)],
            directive: "Plan generator and reducer tasks for <current_iteration><goal>.".to_owned(),
            context_limits: vec![
                "Prior iterations omit internal attempt history.".to_owned(),
                "Planner outcomes are omitted from iteration and workflow history.".to_owned(),
            ],
        })
    }

    async fn build_execution_context(
        &self,
        scope: &ContextScope,
        role: ContextRole,
    ) -> Result<AgentContext> {
        let attempt_id = scope.attempt_id()?;
        let task_id = scope.task_id()?;
        self.deps
            .attempt_store
            .get(attempt_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("attempt", attempt_id.as_str()))?;
        let task = self
            .deps
            .task_store
            .get(task_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("task", task_id.as_str()))?;

        let mut sections = Vec::new();
        let dependencies = self.dependency_sections(&task.needs).await?;
        if !dependencies.is_empty() {
            sections.push(ContextSection::new("dependencies").with_children(dependencies));
        }
        sections.push(
            ContextSection::new("assigned_task")
                .with_attrs(vec![("task_id".to_owned(), task.id.as_str().to_owned())])
                .with_text(task.instruction),
        );
        Ok(AgentContext {
            role,
            sections,
            directive: "Complete <assigned_task> using <dependencies>.".to_owned(),
            context_limits: Vec::new(),
        })
    }

    async fn prior_iteration_sections(
        &self,
        workflow_id: &eos_state::WorkflowId,
        current_sequence: i64,
    ) -> Result<Vec<ContextSection>> {
        let iterations = self
            .deps
            .iteration_store
            .list_for_workflow(workflow_id)
            .await?;
        let mut sections = Vec::new();
        for iteration in iterations {
            if iteration.sequence_no >= current_sequence {
                continue;
            }
            let Some(outcomes) = iteration.outcomes.as_deref() else {
                continue;
            };
            let parsed = parse_outcomes_record(outcomes)?;
            if parsed.is_empty() {
                continue;
            }
            sections.push(
                ContextSection::new("iteration")
                    .with_attrs(vec![(
                        "sequence".to_owned(),
                        iteration.sequence_no.to_string(),
                    )])
                    .with_children(parsed.iter().map(render_task_outcome).collect()),
            );
        }
        Ok(sections)
    }

    async fn previous_attempt_sections(&self, current: &Attempt) -> Result<Vec<ContextSection>> {
        let attempts = self
            .deps
            .attempt_store
            .list_for_iteration(&current.iteration_id)
            .await?;
        let mut sections = Vec::new();
        for attempt in attempts {
            if attempt.attempt_sequence_no >= current.attempt_sequence_no {
                continue;
            }
            let outcomes =
                attempt_execution_outcomes(&attempt, Some(self.deps.task_store.as_ref())).await?;
            if outcomes.is_empty() {
                continue;
            }
            sections.push(
                ContextSection::new("attempt")
                    .with_attrs(vec![
                        (
                            "sequence".to_owned(),
                            attempt.attempt_sequence_no.to_string(),
                        ),
                        (
                            "status".to_owned(),
                            format!("{:?}", attempt.status).to_lowercase(),
                        ),
                    ])
                    .with_children(outcomes.iter().map(render_task_outcome).collect()),
            );
        }
        Ok(sections)
    }

    async fn dependency_sections(
        &self,
        needs: &[eos_state::TaskId],
    ) -> Result<Vec<ContextSection>> {
        let mut sections = Vec::with_capacity(needs.len());
        for task_id in needs {
            let task = self
                .deps
                .task_store
                .get(task_id)
                .await?
                .ok_or_else(|| WorkflowError::not_found("dependency task", task_id.as_str()))?;
            let mut outcomes = task.outcomes.clone();
            if outcomes.is_empty() {
                if task.status != TaskStatus::Done {
                    return Err(WorkflowError::invariant(format!(
                        "dependency task {:?} has no execution outcome",
                        task_id.as_str()
                    )));
                }
                outcomes.push(ExecutionTaskOutcome {
                    status: TaskOutcomeStatus::Success,
                    role: execution_role(task.role),
                    task_id: task_id.clone(),
                    outcome: "(no outcome recorded)".to_owned(),
                });
            }
            sections.push(
                ContextSection::new("dependency")
                    .with_attrs(vec![("task_id".to_owned(), task_id.as_str().to_owned())])
                    .with_children(outcomes.iter().map(render_task_outcome).collect()),
            );
        }
        Ok(sections)
    }
}

fn validate_context_recipe(recipe_id: &str, role: ContextRole) -> Result<()> {
    let expected = role.as_str();
    if !matches!(recipe_id, "planner" | "generator" | "reducer") {
        return Err(WorkflowError::Recipe(format!(
            "unknown context recipe: {recipe_id:?}"
        )));
    }
    if recipe_id != expected {
        return Err(WorkflowError::Recipe(format!(
            "context recipe {recipe_id:?} cannot build role {expected:?}"
        )));
    }
    Ok(())
}

fn parse_outcomes_record(raw: &str) -> Result<Vec<ExecutionTaskOutcome>> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(raw)?)
}

fn execution_role(role: TaskRole) -> ExecutionRole {
    if role == TaskRole::Reducer {
        ExecutionRole::Reducer
    } else {
        ExecutionRole::Generator
    }
}
