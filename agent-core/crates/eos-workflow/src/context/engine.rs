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
        let mut iterations = self
            .deps
            .iteration_store
            .list_for_workflow(workflow_id)
            .await?;
        // Match Python `sorted(iterations, key=sequence_no)` (engine.py:208) rather
        // than relying on an unstated store ordering contract.
        iterations.sort_by_key(|iteration| iteration.sequence_no);
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
        let mut attempts = self
            .deps
            .attempt_store
            .list_for_iteration(&current.iteration_id)
            .await?;
        // Match Python `sorted(attempts, key=attempt_sequence_no)` (engine.py:231)
        // rather than relying on an unstated store ordering contract.
        attempts.sort_by_key(|attempt| attempt.attempt_sequence_no);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Arc;

    use eos_state::{
        ExecutionTaskOutcome, IterationCreationReason, RequestId, Task, TaskOutcomeStatus,
    };

    use super::*;
    use crate::context::{render_context_xml, render_task_guidance, ContextScope, ContextSection};
    use crate::ids::{generator_task_id, reducer_task_id};
    use crate::testsupport::{tid, MemoryStores};

    fn deps(stores: &Arc<MemoryStores>) -> ContextEngineDeps {
        ContextEngineDeps {
            workflow_store: stores.clone(),
            iteration_store: stores.clone(),
            attempt_store: stores.clone(),
            task_store: stores.clone(),
        }
    }

    fn outcome(
        status: TaskOutcomeStatus,
        role: ExecutionRole,
        task_id: &eos_state::TaskId,
        text: &str,
    ) -> ExecutionTaskOutcome {
        ExecutionTaskOutcome {
            status,
            role,
            task_id: task_id.clone(),
            outcome: text.to_owned(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_task(
        stores: &MemoryStores,
        id: &eos_state::TaskId,
        request_id: &RequestId,
        role: TaskRole,
        instruction: &str,
        status: TaskStatus,
        needs: Vec<eos_state::TaskId>,
        outcomes: Vec<ExecutionTaskOutcome>,
        attempt: &Attempt,
    ) {
        stores.seed_task(Task {
            id: id.clone(),
            request_id: request_id.clone(),
            role,
            instruction: instruction.to_owned(),
            status,
            workflow_id: Some(attempt.workflow_id.clone()),
            iteration_id: Some(attempt.iteration_id.clone()),
            attempt_id: Some(attempt.id.clone()),
            agent_name: Some(
                if role == TaskRole::Reducer {
                    "reducer"
                } else {
                    "coder"
                }
                .to_owned(),
            ),
            needs,
            outcomes,
            terminal_tool_result: None,
        });
    }

    // AC-eos-workflow-09: planner context mirrors test_agent_context.py
    // (workflow shape + prior-iteration + previous-attempt execution outcomes).
    #[tokio::test]
    async fn build_planner_context_matches_source() {
        let stores = Arc::new(MemoryStores::default());
        let request_id = RequestId::new_v4();
        let workflow = stores.seed_workflow("Build the complete feature.").await;

        let prior = stores
            .seed_iteration(
                &workflow.id,
                1,
                IterationCreationReason::Initial,
                "Build storage.",
                2,
            )
            .await;
        let prior_outcomes = serde_json::to_string(&vec![outcome(
            TaskOutcomeStatus::Success,
            ExecutionRole::Reducer,
            &tid("attempt1:red:verify_storage"),
            "Storage layer is implemented and verified.",
        )])
        .unwrap();
        eos_state::IterationStore::close_succeeded(
            stores.as_ref(),
            &prior.id,
            &prior_outcomes,
            Some(eos_state::UtcDateTime::now()),
        )
        .await
        .unwrap();

        let current = stores
            .seed_iteration(
                &workflow.id,
                2,
                IterationCreationReason::DeferredGoalContinuation,
                "Finish the API and CLI slice.",
                3,
            )
            .await;
        let previous_attempt = stores.seed_attempt(&current.id, &workflow.id, 1).await;
        let gen_id = generator_task_id(&previous_attempt.id, "api").unwrap();
        let red_id = reducer_task_id(&previous_attempt.id, "verify_api").unwrap();
        eos_state::AttemptStore::set_generator_task_ids(
            stores.as_ref(),
            &previous_attempt.id,
            std::slice::from_ref(&gen_id),
        )
        .await
        .unwrap();
        eos_state::AttemptStore::set_reducer_task_ids(
            stores.as_ref(),
            &previous_attempt.id,
            std::slice::from_ref(&red_id),
        )
        .await
        .unwrap();
        exec_task(
            &stores,
            &gen_id,
            &request_id,
            TaskRole::Generator,
            "Implement API.",
            TaskStatus::Done,
            Vec::new(),
            vec![outcome(
                TaskOutcomeStatus::Success,
                ExecutionRole::Generator,
                &gen_id,
                "API endpoints were implemented.",
            )],
            &previous_attempt,
        );
        exec_task(
            &stores,
            &red_id,
            &request_id,
            TaskRole::Reducer,
            "Verify API.",
            TaskStatus::Failed,
            vec![gen_id.clone()],
            vec![outcome(
                TaskOutcomeStatus::Failed,
                ExecutionRole::Reducer,
                &red_id,
                "Verification failed because the CLI command still calls the old endpoint.",
            )],
            &previous_attempt,
        );
        eos_state::AttemptStore::close(
            stores.as_ref(),
            &previous_attempt.id,
            eos_state::AttemptStatus::Failed,
            Some(eos_state::AttemptFailReason::TaskFailed),
            Some(&[]),
            eos_state::UtcDateTime::now(),
        )
        .await
        .unwrap();
        let current_attempt = stores.seed_attempt(&current.id, &workflow.id, 2).await;

        let context = ContextEngine::new(deps(&stores))
            .build(
                "planner",
                &ContextScope::for_planner(
                    workflow.id.clone(),
                    current.id.clone(),
                    current_attempt.id.clone(),
                ),
            )
            .await
            .unwrap();
        let xml = render_context_xml(&context);

        assert!(xml.contains("<context role=\"planner\">"), "{xml}");
        assert!(xml.contains("<workflow>"));
        assert!(xml.contains("<prior_iterations>"));
        assert!(xml.contains(&format!("<iteration sequence=\"{}\">", prior.sequence_no)));
        assert!(xml.contains(
            "<task task_id=\"attempt1:red:verify_storage\" role=\"reducer\" status=\"success\">"
        ));
        assert!(xml.contains(&format!(
            "<current_iteration sequence=\"{}\">",
            current.sequence_no
        )));
        assert!(xml.contains("<attempt sequence=\"1\" status=\"failed\">"));
        assert!(xml.contains(&format!(
            "<task task_id=\"{}\" role=\"generator\" status=\"success\">",
            gen_id.as_str()
        )));
        assert!(xml.contains(&format!(
            "<task task_id=\"{}\" role=\"reducer\" status=\"failed\">",
            red_id.as_str()
        )));
        assert!(!xml.contains("<outcomes>"));
        // Planner outcomes are omitted from prior history.
        assert!(!xml
            .split("<prior_iterations>")
            .nth(1)
            .unwrap()
            .contains("planner"));

        let guidance = render_task_guidance(&context);
        assert!(guidance.contains("<workflow>: workflow goal and current planning frame"));
        assert!(guidance.contains("Planner outcomes are omitted"));
    }

    // AC-eos-workflow-09: generator context = dependencies + assigned_task.
    #[tokio::test]
    async fn build_generator_context_is_dependencies_plus_assigned_task() {
        let stores = Arc::new(MemoryStores::default());
        let request_id = RequestId::new_v4();
        let workflow = stores.seed_workflow("Build the complete feature.").await;
        let iteration = stores
            .seed_iteration(
                &workflow.id,
                1,
                IterationCreationReason::Initial,
                &workflow.workflow_goal,
                2,
            )
            .await;
        let attempt = stores.seed_attempt(&iteration.id, &workflow.id, 1).await;
        let dep_id = generator_task_id(&attempt.id, "storage").unwrap();
        let task_id = generator_task_id(&attempt.id, "api").unwrap();
        exec_task(
            &stores,
            &dep_id,
            &request_id,
            TaskRole::Generator,
            "Build storage.",
            TaskStatus::Done,
            Vec::new(),
            vec![outcome(
                TaskOutcomeStatus::Success,
                ExecutionRole::Generator,
                &dep_id,
                "Storage done.",
            )],
            &attempt,
        );
        exec_task(
            &stores,
            &task_id,
            &request_id,
            TaskRole::Generator,
            "Implement the API endpoints.",
            TaskStatus::Pending,
            vec![dep_id.clone()],
            Vec::new(),
            &attempt,
        );

        let context = ContextEngine::new(deps(&stores))
            .build(
                "generator",
                &ContextScope::for_generator(
                    workflow.id.clone(),
                    iteration.id.clone(),
                    attempt.id.clone(),
                    task_id.clone(),
                ),
            )
            .await
            .unwrap();
        let xml = render_context_xml(&context);

        assert!(xml.contains("<context role=\"generator\">"), "{xml}");
        assert!(xml.contains("<dependencies>"));
        assert!(xml.contains(&format!("<dependency task_id=\"{}\">", dep_id.as_str())));
        assert!(xml.contains(&format!("<assigned_task task_id=\"{}\">", task_id.as_str())));
        assert!(xml.contains("Implement the API endpoints."));
        assert!(!xml.contains("<workflow>"));
        assert!(!xml.contains("<needs>"));
        assert!(render_task_guidance(&context)
            .contains("Complete <assigned_task> using <dependencies>."));
    }

    // AC-eos-workflow-09: reducer context uses assigned_task, not assigned_prompt.
    #[tokio::test]
    async fn build_reducer_context_uses_assigned_task() {
        let stores = Arc::new(MemoryStores::default());
        let request_id = RequestId::new_v4();
        let workflow = stores.seed_workflow("Build the complete feature.").await;
        let iteration = stores
            .seed_iteration(
                &workflow.id,
                1,
                IterationCreationReason::Initial,
                &workflow.workflow_goal,
                2,
            )
            .await;
        let attempt = stores.seed_attempt(&iteration.id, &workflow.id, 1).await;
        let dep_id = generator_task_id(&attempt.id, "api").unwrap();
        let task_id = reducer_task_id(&attempt.id, "verify_api").unwrap();
        exec_task(
            &stores,
            &dep_id,
            &request_id,
            TaskRole::Generator,
            "Build API.",
            TaskStatus::Done,
            Vec::new(),
            vec![outcome(
                TaskOutcomeStatus::Success,
                ExecutionRole::Generator,
                &dep_id,
                "API done.",
            )],
            &attempt,
        );
        exec_task(
            &stores,
            &task_id,
            &request_id,
            TaskRole::Reducer,
            "Verify the API and CLI slice.",
            TaskStatus::Pending,
            vec![dep_id.clone()],
            Vec::new(),
            &attempt,
        );

        let context = ContextEngine::new(deps(&stores))
            .build(
                "reducer",
                &ContextScope::for_reducer(
                    workflow.id.clone(),
                    iteration.id.clone(),
                    attempt.id.clone(),
                    task_id.clone(),
                ),
            )
            .await
            .unwrap();
        let xml = render_context_xml(&context);

        assert!(xml.contains("<context role=\"reducer\">"), "{xml}");
        assert!(xml.contains(&format!("<assigned_task task_id=\"{}\">", task_id.as_str())));
        assert!(xml.contains("Verify the API and CLI slice."));
        assert!(!xml.contains("<assigned_prompt>"));
        assert!(!xml.contains("<needs>"));
    }

    // AC-eos-workflow-09: a recipe whose id != scope role is rejected.
    #[tokio::test]
    async fn build_rejects_recipe_role_mismatch() {
        let stores = Arc::new(MemoryStores::default());
        let err = ContextEngine::new(deps(&stores))
            .build(
                "planner",
                &ContextScope::for_generator(
                    eos_state::WorkflowId::new_v4(),
                    eos_state::IterationId::new_v4(),
                    eos_state::AttemptId::new_v4(),
                    tid("task"),
                ),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, WorkflowError::Recipe(ref msg) if msg.contains("cannot build role")),
            "{err:?}"
        );
    }

    // AC-eos-workflow-09 golden: deterministic XML render (escaping, attr order,
    // nesting, trailing newline) over a fixed-id context.
    #[test]
    fn render_context_xml_golden() {
        let context = AgentContext {
            role: ContextRole::Generator,
            sections: vec![
                ContextSection::new("dependencies").with_children(vec![ContextSection::new(
                    "dependency",
                )
                .with_attrs(vec![("task_id".to_owned(), "dep-1".to_owned())])
                .with_children(vec![ContextSection::new("task")
                    .with_attrs(vec![
                        ("task_id".to_owned(), "dep-1".to_owned()),
                        ("role".to_owned(), "generator".to_owned()),
                        ("status".to_owned(), "success".to_owned()),
                    ])
                    .with_text("Storage done.")])]),
                ContextSection::new("assigned_task")
                    .with_attrs(vec![("task_id".to_owned(), "task-1".to_owned())])
                    .with_text("Implement the API <endpoints>."),
            ],
            directive: "Complete <assigned_task> using <dependencies>.".to_owned(),
            context_limits: Vec::new(),
        };
        let expected = "\
<context role=\"generator\">
<dependencies>
<dependency task_id=\"dep-1\">
<task task_id=\"dep-1\" role=\"generator\" status=\"success\">
Storage done.
</task>
</dependency>
</dependencies>
<assigned_task task_id=\"task-1\">
Implement the API &lt;endpoints&gt;.
</assigned_task>
</context>
";
        assert_eq!(render_context_xml(&context), expected);
    }
}
