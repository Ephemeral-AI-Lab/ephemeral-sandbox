//! The `eos-workflow` [`AgentRunner`] adapter: runs one delegated-workflow agent
//! (planner / generator / reducer) through the shared engine loop.
//!
//! **Path A-recording (Phase-7 complete).** This runner is a thin engine-run
//! wrapper. The submit tool drives the harness *during* the run: a
//! `submit_planner/generator/reducer_outcome` resolves the wired recording
//! [`WorkflowAttemptSubmissionApi`] from `ExecutionMetadata.attempt_submission` and records
//! the agent's real submission straight to the per-attempt orchestrator's
//! non-advancing `record_*` variants (materialize / mark task Done|Failed). The
//! runner therefore does not ferry a typed terminal back — it reports only
//! whether the engine run itself broke (`failure_summary = run.error`). The
//! single `advance_run_stage` loop owns launching + closure (D4: exactly one
//! writer), and catches a dead agent (one that never submitted) at join time via
//! the still-RUNNING exhaustion guard. The parent task is never mutated
//! (GC-eos-agent-core-03).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use eos_agent_run::AgentRunService as RunnerAgentRunService;
use eos_llm_client::Message;
use eos_types::{
    AgentName as SpawnAgentName, AgentRunApi, AgentRunId, SpawnAgentRequest, SpawnAgentTarget,
    TaskRole, WorkflowApi, WorkflowAttemptSubmissionApi, WorkflowCoordinates, WorkflowTaskRole,
};
use eos_workflow::{AgentLaunch, AgentRunReport, AgentRunner, Result as WorkflowResult};

use crate::runtime::{build_agent_loop_launcher, AgentCoreRuntime, EngineEventSink};

/// Runtime adapter over the shared engine loop, supplied to `AttemptResources.runner`.
pub(crate) struct RuntimeAgentRunner {
    services: AgentCoreRuntime,
    workspace_root: String,
    /// The recording attempt-submission API (the wired `AttemptSubmissionAdapter`
    /// over the shared attempt registry). Stateless and shared across all runs.
    attempt_submission: Arc<dyn WorkflowAttemptSubmissionApi>,
    /// The workflow API slot. `WorkflowService` is built after this
    /// runner is captured by `AttemptResources`, then published before any workflow
    /// task agent can start.
    workflow_service: Arc<OnceLock<Arc<dyn WorkflowApi>>>,
    /// Optional stream-event callback shared with the root request.
    event_sink: Option<EngineEventSink>,
}

impl std::fmt::Debug for RuntimeAgentRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAgentRunner").finish_non_exhaustive()
    }
}

impl RuntimeAgentRunner {
    pub(crate) fn new(
        services: AgentCoreRuntime,
        workspace_root: impl Into<String>,
        attempt_submission: Arc<dyn WorkflowAttemptSubmissionApi>,
        workflow_service: Arc<OnceLock<Arc<dyn WorkflowApi>>>,
        event_sink: Option<EngineEventSink>,
    ) -> Self {
        Self {
            services,
            workspace_root: workspace_root.into(),
            attempt_submission,
            workflow_service,
            event_sink,
        }
    }
}

#[async_trait]
impl AgentRunner for RuntimeAgentRunner {
    async fn run(&self, launch: AgentLaunch) -> WorkflowResult<AgentRunReport> {
        let agent_run_id = AgentRunId::new_v4();
        let mut prompt = launch.context().to_owned();
        if let Some(guidance) = launch.task_guidance() {
            prompt.push_str("\n\n");
            prompt.push_str(guidance);
        }
        if let Some(skill) = launch.skill() {
            prompt.push_str("\n\n");
            prompt.push_str(skill);
        }

        let Some(workflow_service) = self.workflow_service.get().cloned() else {
            return Ok(AgentRunReport {
                failure_summary: Some("workflow API not initialized".to_owned()),
            });
        };
        let loop_launcher = build_agent_loop_launcher(
            &self.services,
            self.attempt_submission.clone(),
            workflow_service,
            self.event_sink.clone(),
        );
        let agent_runs = Arc::new(
            RunnerAgentRunService::new(
                self.services.agent_core.agent_registry.clone(),
                loop_launcher,
                self.services.db.agent_run_store.clone(),
                self.services.message_records.message_records.clone(),
            )
            .with_runtime_state_hooks(
                {
                    let agent_state = self.services.agent_state.clone();
                    move |request, agent_run_id| {
                        agent_state.record_spawn_request(request, agent_run_id)
                    }
                },
                {
                    let agent_state = self.services.agent_state.clone();
                    move |agent_run_id| agent_state.remove(agent_run_id)
                },
            ),
        );
        let failure_summary = match agent_runs
            .spawn_agent(SpawnAgentRequest {
                agent_name: SpawnAgentName::new(launch.agent_def().name.as_str())
                    .expect("loaded agent name is valid"),
                agent_run_id: Some(agent_run_id),
                initial_messages: vec![Message::from_user_text(prompt)],
                target: SpawnAgentTarget::Workflow {
                    request_id: launch.request_id().clone(),
                    task_id: launch.task_id().clone(),
                    workflow: WorkflowCoordinates {
                        workflow_id: launch.workflow_id().clone(),
                        iteration_id: launch.iteration_id().clone(),
                        attempt_id: launch.attempt_id().clone(),
                    },
                    role: workflow_task_agent_run_role(launch.role()),
                },
                sandbox_id: None,
                workspace_root: self.workspace_root.clone(),
                is_isolated_workspace_mode: false,
                persist: true,
            })
            .await
        {
            Ok(agent_run_id) => agent_runs
                .wait_for_agent_outcome(&agent_run_id)
                .await
                .map_or_else(|err| Some(err.to_string()), |outcome| outcome.error),
            Err(err) => Some(err.to_string()),
        };

        // The submit tool already recorded the agent's submission during the run
        // (Path A-recording); the runner reports only a framework fault, which
        // the loop uses as the still-RUNNING exhaustion summary for a dead agent.
        Ok(AgentRunReport { failure_summary })
    }
}

fn workflow_task_agent_run_role(role: TaskRole) -> WorkflowTaskRole {
    match role {
        TaskRole::Planner => WorkflowTaskRole::Planner,
        TaskRole::Generator => WorkflowTaskRole::Generator,
        TaskRole::Reducer => WorkflowTaskRole::Reducer,
        TaskRole::Root => unreachable!("workflow task-agent-run role cannot be root"),
    }
}
