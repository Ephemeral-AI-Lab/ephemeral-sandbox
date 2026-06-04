use std::sync::Arc;

use async_trait::async_trait;
use eos_agent_def::{AgentDefinition, AgentName, AgentRegistry, AgentRole};
use eos_state::{
    Attempt, AttemptStore, IterationStore, RequestId, Task, TaskId, TaskStore, WorkflowId,
    WorkflowStore,
};

use crate::context::{AgentEntryComposer, ContextScope};
use crate::ids::WorkflowLifecycleConfig;
use crate::{Result, WorkflowError};

use super::AttemptOrchestratorRegistry;
use crate::iteration::OpenIterationCoordinatorRegistry;

/// Result of one agent run at the workflow seam.
///
/// Under Path A-recording the runner no longer ferries a terminal submission
/// back: the submit tool records the agent's submission straight to the
/// orchestrator *during* the run. This report carries only whether the engine
/// run itself broke (a framework fault), which the single `advance_run_stage`
/// loop uses as the still-RUNNING exhaustion summary for a dead agent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentRunReport {
    /// A framework-fault summary if the engine run broke; `None` on a clean run.
    pub failure_summary: Option<String>,
}

impl AgentRunReport {
    /// A clean run (the agent recorded its own submission, or none — the loop
    /// catches a dead agent at join time).
    #[must_use]
    pub fn ok() -> Self {
        Self {
            failure_summary: None,
        }
    }

    /// A run that broke with `summary` (a framework fault).
    #[must_use]
    pub fn failed(summary: impl Into<String>) -> Self {
        Self {
            failure_summary: Some(summary.into()),
        }
    }
}

/// Runtime adapter seam over the engine's agent runner.
#[async_trait]
pub trait AgentRunner: Send + Sync {
    /// Run one launched agent to completion.
    async fn run(&self, launch: AgentLaunch) -> Result<AgentRunReport>;
}

/// Launch descriptor for one workflow agent.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLaunch {
    /// Task id.
    pub task_id: TaskId,
    /// Request id.
    pub request_id: RequestId,
    /// Attempt id.
    pub attempt_id: Option<eos_state::AttemptId>,
    /// Agent role.
    pub role: AgentRole,
    /// Profile name.
    pub agent_name: String,
    /// Rendered context row.
    pub context: String,
    /// Rendered task guidance row.
    pub task_guidance: Option<String>,
    /// Needs edges.
    pub needs: Vec<TaskId>,
    /// Resolved definition.
    pub agent_def: Option<AgentDefinition>,
    /// Workflow id.
    pub workflow_id: Option<WorkflowId>,
    /// Skill row.
    pub skill: Option<String>,
}

/// Per-attempt dependency bundle.
#[derive(Clone)]
pub struct AttemptDeps {
    /// Workflow store.
    pub workflow_store: Arc<dyn WorkflowStore>,
    /// Iteration store.
    pub iteration_store: Arc<dyn IterationStore>,
    /// Attempt store.
    pub attempt_store: Arc<dyn AttemptStore>,
    /// Task store.
    pub task_store: Arc<dyn TaskStore>,
    /// Agent registry.
    pub agent_registry: Arc<AgentRegistry>,
    /// Active orchestrator registry.
    pub orchestrator_registry: Arc<AttemptOrchestratorRegistry>,
    /// Open iteration coordinator registry.
    pub iteration_coordinators: Option<Arc<OpenIterationCoordinatorRegistry>>,
    /// Lifecycle knobs.
    pub lifecycle_config: WorkflowLifecycleConfig,
    /// Optional composer.
    pub composer: Option<Arc<AgentEntryComposer>>,
    /// Agent runner seam.
    pub runner: Arc<dyn AgentRunner>,
    /// Per-attempt run cap.
    pub max_concurrent_task_runs: usize,
}

impl std::fmt::Debug for AttemptDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttemptDeps")
            .field("max_concurrent_task_runs", &self.max_concurrent_task_runs)
            .field(
                "has_iteration_coordinators",
                &self.iteration_coordinators.is_some(),
            )
            .field("has_composer", &self.composer.is_some())
            .finish_non_exhaustive()
    }
}

impl AttemptDeps {
    /// Create deps with sane workflow defaults.
    #[must_use]
    pub fn new(
        workflow_store: Arc<dyn WorkflowStore>,
        iteration_store: Arc<dyn IterationStore>,
        attempt_store: Arc<dyn AttemptStore>,
        task_store: Arc<dyn TaskStore>,
        agent_registry: Arc<AgentRegistry>,
        runner: Arc<dyn AgentRunner>,
    ) -> Self {
        Self {
            workflow_store,
            iteration_store,
            attempt_store,
            task_store,
            agent_registry,
            runner,
            orchestrator_registry: Arc::new(AttemptOrchestratorRegistry::new()),
            iteration_coordinators: None,
            lifecycle_config: WorkflowLifecycleConfig::default(),
            composer: None,
            max_concurrent_task_runs: 8,
        }
    }

    pub(crate) async fn request_id_for_attempt(&self, attempt: &Attempt) -> Result<RequestId> {
        let iteration = self
            .iteration_store
            .get(&attempt.iteration_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("iteration", attempt.iteration_id.as_str()))?;
        let workflow = self
            .workflow_store
            .get(&iteration.workflow_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("workflow", iteration.workflow_id.as_str()))?;
        Ok(workflow.request_id)
    }
}

/// Role-parametrized launch factory.
#[derive(Debug, Clone)]
pub struct AgentLaunchFactory {
    deps: AttemptDeps,
}

struct LaunchBuildArgs<'a> {
    base_agent_name: &'a str,
    role: AgentRole,
    scope: ContextScope,
    task_id: TaskId,
    request_id: RequestId,
    attempt_id: Option<eos_state::AttemptId>,
    needs: Vec<TaskId>,
    workflow_id: Option<WorkflowId>,
}

impl AgentLaunchFactory {
    /// Create a launch factory.
    #[must_use]
    pub fn new(deps: AttemptDeps) -> Self {
        Self { deps }
    }

    pub(crate) async fn for_planner(
        &self,
        attempt: &Attempt,
        task_id: TaskId,
    ) -> Result<AgentLaunch> {
        let iteration = self
            .deps
            .iteration_store
            .get(&attempt.iteration_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("iteration", attempt.iteration_id.as_str()))?;
        self.build(LaunchBuildArgs {
            base_agent_name: "planner",
            role: AgentRole::Planner,
            scope: ContextScope::for_planner(
                iteration.workflow_id.clone(),
                iteration.id,
                attempt.id.clone(),
            ),
            task_id,
            request_id: self.deps.request_id_for_attempt(attempt).await?,
            attempt_id: Some(attempt.id.clone()),
            needs: Vec::new(),
            workflow_id: Some(iteration.workflow_id),
        })
        .await
    }

    pub(crate) async fn for_generator(
        &self,
        attempt: &Attempt,
        task: &Task,
        base_agent_name: &str,
    ) -> Result<AgentLaunch> {
        let iteration = self
            .deps
            .iteration_store
            .get(&attempt.iteration_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("iteration", attempt.iteration_id.as_str()))?;
        self.build(LaunchBuildArgs {
            base_agent_name,
            role: AgentRole::Generator,
            scope: ContextScope::for_generator(
                iteration.workflow_id.clone(),
                iteration.id,
                attempt.id.clone(),
                task.id.clone(),
            ),
            task_id: task.id.clone(),
            request_id: task.request_id.clone(),
            attempt_id: Some(attempt.id.clone()),
            needs: task.needs.clone(),
            workflow_id: Some(iteration.workflow_id),
        })
        .await
    }

    pub(crate) async fn for_reducer(&self, attempt: &Attempt, task: &Task) -> Result<AgentLaunch> {
        let iteration = self
            .deps
            .iteration_store
            .get(&attempt.iteration_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("iteration", attempt.iteration_id.as_str()))?;
        self.build(LaunchBuildArgs {
            base_agent_name: "reducer",
            role: AgentRole::Reducer,
            scope: ContextScope::for_reducer(
                iteration.workflow_id.clone(),
                iteration.id,
                attempt.id.clone(),
                task.id.clone(),
            ),
            task_id: task.id.clone(),
            request_id: task.request_id.clone(),
            attempt_id: Some(attempt.id.clone()),
            needs: task.needs.clone(),
            workflow_id: Some(iteration.workflow_id),
        })
        .await
    }

    async fn build(&self, args: LaunchBuildArgs<'_>) -> Result<AgentLaunch> {
        let name = AgentName::new(args.base_agent_name)?;
        let agent_def = self
            .deps
            .agent_registry
            .get(&name)
            .ok_or_else(|| {
                WorkflowError::AgentDefinition(format!(
                    "workflow agent definition {:?} is not registered",
                    args.base_agent_name
                ))
            })?
            .as_ref()
            .clone();
        if let Some(composer) = &self.deps.composer {
            let messages = composer.compose(args.base_agent_name, &args.scope).await?;
            Ok(AgentLaunch {
                task_id: args.task_id,
                request_id: args.request_id,
                attempt_id: args.attempt_id,
                role: args.role,
                agent_name: args.base_agent_name.to_owned(),
                context: messages.context,
                task_guidance: messages.task_guidance,
                needs: args.needs,
                agent_def: Some(messages.agent_def),
                workflow_id: args.workflow_id,
                skill: messages.skill,
            })
        } else {
            Ok(AgentLaunch {
                task_id: args.task_id,
                request_id: args.request_id,
                attempt_id: args.attempt_id,
                role: args.role,
                agent_name: args.base_agent_name.to_owned(),
                context: format!(
                    "{} context for {}",
                    args.scope.role.as_str(),
                    args.base_agent_name
                ),
                task_guidance: None,
                needs: args.needs,
                agent_def: Some(agent_def),
                workflow_id: args.workflow_id,
                skill: None,
            })
        }
    }
}
