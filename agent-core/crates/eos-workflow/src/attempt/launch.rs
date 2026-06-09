use std::fs;
use std::sync::Arc;

use async_trait::async_trait;
use eos_tool::{render_tool_instruction, ToolInstructions, ToolName};
use eos_types::{
    AgentDefinition, AgentName, AgentRegistry, AgentType, Attempt, AttemptStore, IterationStore,
    PlanId, RequestId, TaskId, TaskStore, WorkItemId, WorkItemSpec, WorkflowCoordinates,
    WorkflowStore, WorkflowTaskRole,
};

use crate::config::WorkflowLifecycleConfig;
use crate::context::{
    render_context_xml, render_planner_agent_context, render_task_guidance,
    render_worker_agent_context, ContextScope,
};
use crate::{Result, WorkflowError};

use super::{ActiveAttemptRuns, OpenIterationCoordinatorRegistry};

/// Result of one agent run at the workflow seam.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentRunReport {
    /// A framework-fault summary if the engine run broke; `None` on a clean run.
    pub failure_summary: Option<String>,
}

impl AgentRunReport {
    /// A clean run.
    #[must_use]
    pub fn ok() -> Self {
        Self {
            failure_summary: None,
        }
    }

    /// A run that broke with `summary`.
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

/// Planner or worker launch discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLaunchKind {
    /// Planner run for the attempt.
    Planner,
    /// Worker run for one work item.
    Worker {
        /// Planner-authored work item id.
        work_item_id: WorkItemId,
    },
}

/// Launch descriptor for one workflow agent.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLaunch {
    /// Launch kind.
    pub kind: AgentLaunchKind,
    /// Opaque task id.
    pub task_id: TaskId,
    /// Owning request.
    pub request_id: RequestId,
    /// Workflow coordinates.
    pub coords: WorkflowCoordinates,
    /// Attempt-local plan id.
    pub plan_id: PlanId,
    /// Bound profile name.
    pub agent_name: AgentName,
    /// Persisted task instruction.
    pub instruction: String,
    /// Rendered context row.
    pub context: String,
    /// Rendered task guidance row.
    pub task_guidance: Option<String>,
    /// Resolved definition.
    pub agent_def: AgentDefinition,
    /// Skill row.
    pub skill: Option<String>,
}

impl AgentLaunch {
    /// Workflow task role.
    #[must_use]
    pub const fn role(&self) -> WorkflowTaskRole {
        match self.kind {
            AgentLaunchKind::Planner => WorkflowTaskRole::Planner,
            AgentLaunchKind::Worker { .. } => WorkflowTaskRole::Worker,
        }
    }

    /// Task id.
    #[must_use]
    pub const fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    /// Worker work item id, if this launch is a worker.
    #[must_use]
    pub const fn work_item_id(&self) -> Option<&WorkItemId> {
        match &self.kind {
            AgentLaunchKind::Planner => None,
            AgentLaunchKind::Worker { work_item_id } => Some(work_item_id),
        }
    }

    /// Attempt id.
    #[must_use]
    pub const fn attempt_id(&self) -> &eos_types::AttemptId {
        &self.coords.attempt_id
    }

    /// Iteration id.
    #[must_use]
    pub const fn iteration_id(&self) -> &eos_types::IterationId {
        &self.coords.iteration_id
    }

    /// Workflow id.
    #[must_use]
    pub const fn workflow_id(&self) -> &eos_types::WorkflowId {
        &self.coords.workflow_id
    }
}

/// Per-attempt dependency bundle.
#[derive(Clone)]
pub struct AttemptResources {
    /// Workflow store.
    pub(crate) workflow_store: Arc<dyn WorkflowStore>,
    /// Iteration store.
    pub(crate) iteration_store: Arc<dyn IterationStore>,
    /// Attempt store.
    pub(crate) attempt_store: Arc<dyn AttemptStore>,
    /// Task store.
    pub(crate) task_store: Arc<dyn TaskStore>,
    /// Agent registry.
    pub(crate) agent_registry: Arc<AgentRegistry>,
    /// Active attempt registry.
    pub(crate) active_attempt_runs: Arc<ActiveAttemptRuns>,
    /// Open iteration coordinator registry.
    pub(crate) iteration_coordinators: Option<Arc<OpenIterationCoordinatorRegistry>>,
    /// Lifecycle knobs.
    pub(crate) lifecycle_config: WorkflowLifecycleConfig,
    /// Agent runner seam.
    pub(crate) runner: Arc<dyn AgentRunner>,
    /// Per-attempt worker run cap.
    pub(crate) max_concurrent_task_runs: usize,
}

impl std::fmt::Debug for AttemptResources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttemptResources")
            .field("max_concurrent_task_runs", &self.max_concurrent_task_runs)
            .field(
                "has_iteration_coordinators",
                &self.iteration_coordinators.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl AttemptResources {
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
            active_attempt_runs: Arc::new(ActiveAttemptRuns::new()),
            iteration_coordinators: None,
            lifecycle_config: WorkflowLifecycleConfig::default(),
            max_concurrent_task_runs: 8,
        }
    }

    /// Use a caller-owned active attempt registry.
    #[must_use]
    pub fn with_active_attempt_runs(mut self, registry: Arc<ActiveAttemptRuns>) -> Self {
        self.active_attempt_runs = registry;
        self
    }

    /// Use a caller-owned open-iteration coordinator registry.
    #[must_use]
    pub fn with_iteration_coordinators(
        mut self,
        registry: Arc<OpenIterationCoordinatorRegistry>,
    ) -> Self {
        self.iteration_coordinators = Some(registry);
        self
    }

    /// Use caller-supplied lifecycle knobs.
    #[must_use]
    pub fn with_lifecycle_config(mut self, config: WorkflowLifecycleConfig) -> Self {
        self.lifecycle_config = config;
        self
    }

    /// Use a caller-supplied per-attempt worker-run concurrency cap.
    #[must_use]
    pub fn with_max_concurrent_task_runs(mut self, max: usize) -> Self {
        self.max_concurrent_task_runs = max;
        self
    }

    pub(crate) async fn request_id_for_attempt(&self, attempt: &Attempt) -> Result<RequestId> {
        let workflow = self
            .workflow_store
            .get(&attempt.workflow_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("workflow", attempt.workflow_id.as_str()))?;
        Ok(workflow.request_id)
    }
}

/// Role-parametrized launch factory.
#[derive(Debug, Clone)]
pub struct AgentLaunchFactory {
    deps: AttemptResources,
}

impl AgentLaunchFactory {
    /// Create a launch factory.
    #[must_use]
    pub fn new(deps: AttemptResources) -> Self {
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
        let agent_name = AgentName::new("planner")?;
        let agent_def = self.agent_definition(&agent_name, WorkflowTaskRole::Planner)?;
        let context = render_planner_agent_context(&self.deps, attempt, &task_id).await?;
        let context_xml = render_context_xml(&context);
        Ok(AgentLaunch {
            kind: AgentLaunchKind::Planner,
            task_id,
            request_id: self.deps.request_id_for_attempt(attempt).await?,
            coords: WorkflowCoordinates {
                workflow_id: attempt.workflow_id.clone(),
                iteration_id: iteration.id,
                attempt_id: attempt.id.clone(),
            },
            plan_id: attempt.plan_id.clone(),
            agent_name,
            instruction: context_xml.clone(),
            context: context_xml,
            task_guidance: Some(wrap_task_guidance(
                &render_task_guidance(&context),
                &agent_def,
            )),
            skill: build_skill_message(&agent_def)?,
            agent_def,
        })
    }

    pub(crate) async fn for_worker(
        &self,
        attempt: &Attempt,
        work_item: &WorkItemSpec,
        task_id: TaskId,
    ) -> Result<AgentLaunch> {
        let iteration = self
            .deps
            .iteration_store
            .get(&attempt.iteration_id)
            .await?
            .ok_or_else(|| WorkflowError::not_found("iteration", attempt.iteration_id.as_str()))?;
        let agent_def = self.agent_definition(&work_item.agent_name, WorkflowTaskRole::Worker)?;
        let context = render_worker_agent_context(&self.deps, attempt, work_item, &task_id).await?;
        let context_xml = render_context_xml(&context);
        Ok(AgentLaunch {
            kind: AgentLaunchKind::Worker {
                work_item_id: work_item.id.clone(),
            },
            task_id,
            request_id: self.deps.request_id_for_attempt(attempt).await?,
            coords: WorkflowCoordinates {
                workflow_id: attempt.workflow_id.clone(),
                iteration_id: iteration.id,
                attempt_id: attempt.id.clone(),
            },
            plan_id: attempt.plan_id.clone(),
            agent_name: work_item.agent_name.clone(),
            instruction: work_item.work_spec.clone(),
            context: context_xml,
            task_guidance: Some(wrap_task_guidance(
                &render_task_guidance(&context),
                &agent_def,
            )),
            skill: build_skill_message(&agent_def)?,
            agent_def,
        })
    }

    fn agent_definition(
        &self,
        agent_name: &AgentName,
        role: WorkflowTaskRole,
    ) -> Result<AgentDefinition> {
        let agent_def = self
            .deps
            .agent_registry
            .get(agent_name)
            .ok_or_else(|| {
                WorkflowError::AgentDefinition(format!(
                    "workflow agent definition {:?} is not registered",
                    agent_name.as_str()
                ))
            })?
            .as_ref()
            .clone();
        if agent_def.agent_type != AgentType::Agent {
            return Err(WorkflowError::invariant(format!(
                "workflow {} launch is bound to agent {:?} with type {:?}, expected agent",
                role.as_str(),
                agent_name.as_str(),
                agent_def.agent_type
            )));
        }
        Ok(agent_def)
    }
}

fn wrap_task_guidance(prose: &str, agent_def: &AgentDefinition) -> String {
    let body = prose.trim_end();
    if let Some(block) = terminal_selection_block(agent_def) {
        format!("<Task Guidance>\n{body}\n\n{block}\n</Task Guidance>")
    } else {
        format!("<Task Guidance>\n{body}\n</Task Guidance>")
    }
}

fn build_skill_message(agent_def: &AgentDefinition) -> Result<Option<String>> {
    let Some(path) = &agent_def.skill else {
        return Ok(None);
    };
    let raw =
        fs::read_to_string(path).map_err(|err| WorkflowError::AgentDefinition(err.to_string()))?;
    let body = strip_frontmatter(&raw).trim().to_owned();
    let skill_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("skill");
    let mut parts = vec![
        format!("Load skill: {skill_name}"),
        String::new(),
        "<skill>".to_owned(),
        body,
        "</skill>".to_owned(),
    ];
    if let Some(block) = terminal_selection_block(agent_def) {
        parts.push(String::new());
        parts.push(block);
    }
    Ok(Some(parts.join("\n")))
}

fn strip_frontmatter(raw: &str) -> &str {
    let Some(rest) = raw.strip_prefix("---") else {
        return raw;
    };
    let Some((_, body)) = rest.split_once("\n---") else {
        return raw;
    };
    body
}

fn terminal_selection_block(agent_def: &AgentDefinition) -> Option<String> {
    let mut terminals = Vec::new();
    for terminal in &agent_def.terminals {
        let Ok(name) = terminal.parse::<ToolName>() else {
            continue;
        };
        terminals.push(name);
    }
    if terminals.is_empty() {
        None
    } else {
        let catalog = render_tool_instruction(&terminals, ToolInstructions::SelectionGuidance);
        Some(format!(
            "<terminal_tool_selection>\n{catalog}\n</terminal_tool_selection>"
        ))
    }
}

#[allow(dead_code)]
fn _scope_for_launch(launch: &AgentLaunch) -> ContextScope {
    match &launch.kind {
        AgentLaunchKind::Planner => ContextScope::for_planner(
            launch.coords.workflow_id.clone(),
            launch.coords.iteration_id.clone(),
            launch.coords.attempt_id.clone(),
            launch.task_id.clone(),
        ),
        AgentLaunchKind::Worker { work_item_id } => ContextScope::for_worker(
            launch.coords.workflow_id.clone(),
            launch.coords.iteration_id.clone(),
            launch.coords.attempt_id.clone(),
            launch.task_id.clone(),
            work_item_id.clone(),
        ),
    }
}
