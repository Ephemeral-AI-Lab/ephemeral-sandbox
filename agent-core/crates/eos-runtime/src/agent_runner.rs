//! The `eos-workflow` [`AgentRunner`] adapter: runs one delegated-workflow agent
//! (planner / generator / reducer) through the shared engine loop.
//!
//! **Path A-recording (Phase-7 complete).** This runner is a thin engine-run
//! wrapper. The submit tool drives the harness *during* the run: a
//! `submit_planner/generator/reducer_outcome` resolves the wired recording
//! [`AttemptSubmissionPort`] from `ExecutionMetadata.attempt_submission` and records
//! the agent's real submission straight to the per-attempt orchestrator's
//! non-advancing `record_*` variants (materialize / mark task Done|Failed). The
//! runner therefore does not ferry a typed terminal back — it reports only
//! whether the engine run itself broke (`failure_summary = run.error`). The
//! single `advance_run_stage` loop owns launching + closure (D4: exactly one
//! writer), and catches a dead agent (one that never submitted) at join time via
//! the still-RUNNING exhaustion guard. The parent task is never mutated
//! (GC-eos-runtime-03).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use eos_agent_run::AgentRunService as RunnerAgentRunService;
use eos_llm_client::Message;
use eos_types::{
    AgentName as AgentPortName, AgentRunApi, AgentRunId, AgentRunMessageRecordKind,
    AttemptSubmissionPort, SpawnAgentRequest, TaskRole, WorkflowApi, WorkflowTaskRole,
};
use eos_workflow::{AgentLaunch, AgentRunReport, AgentRunner, Result as WorkflowResult};

use crate::runtime_services::{build_agent_loop_launcher, EventCallback, RuntimeServices};

/// Runtime adapter over the shared engine loop, supplied to `AttemptDeps.runner`.
pub(crate) struct RuntimeAgentRunner {
    services: RuntimeServices,
    workspace_root: String,
    /// The recording attempt-submission port (the wired `AttemptSubmissionAdapter`
    /// over the shared attempt registry). Stateless and shared across all runs.
    attempt_submission: Arc<dyn AttemptSubmissionPort>,
    /// The workflow-control port, late-bound at composition (it is built
    /// downstream of this runner via the `starter→attempt_deps→runner` chain).
    /// `get()` is `Some` by the time any run starts, so workflow agents' hooks
    /// can read `workflow_depth` (deferral) and `find_outstanding` (no-inflight).
    workflow_service: Arc<OnceLock<Arc<dyn WorkflowApi>>>,
    /// Optional stream-event callback shared with the root request.
    event_callback: Option<EventCallback>,
}

impl std::fmt::Debug for RuntimeAgentRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAgentRunner").finish_non_exhaustive()
    }
}

impl RuntimeAgentRunner {
    pub(crate) fn new(
        services: RuntimeServices,
        workspace_root: impl Into<String>,
        attempt_submission: Arc<dyn AttemptSubmissionPort>,
        workflow_service: Arc<OnceLock<Arc<dyn WorkflowApi>>>,
        event_callback: Option<EventCallback>,
    ) -> Self {
        Self {
            services,
            workspace_root: workspace_root.into(),
            attempt_submission,
            workflow_service,
            event_callback,
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

        let (loop_launcher, agent_run_api_cell) = build_agent_loop_launcher(
            &self.services,
            self.attempt_submission.clone(),
            self.workflow_service.clone(),
            self.event_callback.clone(),
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
        let agent_run_api: Arc<dyn AgentRunApi> = agent_runs.clone();
        let _ = agent_run_api_cell.set(agent_run_api);
        let failure_summary = match agent_runs
            .spawn_agent(SpawnAgentRequest {
                agent_name: AgentPortName::new(launch.agent_def().name.as_str())
                    .expect("loaded agent name is valid"),
                agent_run_id: Some(agent_run_id),
                initial_messages: vec![Message::from_user_text(prompt)],
                parent_agent_run_id: None,
                request_id: Some(launch.request_id().clone()),
                task_id: Some(launch.task_id().clone()),
                attempt_id: Some(launch.attempt_id().clone()),
                workflow_id: Some(launch.workflow_id().clone()),
                sandbox_id: None,
                workspace_root: self.workspace_root.clone(),
                is_isolated_workspace_mode: false,
                persist: true,
                record_kind: AgentRunMessageRecordKind::WorkflowTask {
                    workflow_id: launch.workflow_id().clone(),
                    iteration_id: launch.iteration_id().clone(),
                    attempt_id: launch.attempt_id().clone(),
                    role: workflow_message_record_role(launch.role()),
                },
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

fn workflow_message_record_role(role: TaskRole) -> WorkflowTaskRole {
    match role {
        TaskRole::Planner => WorkflowTaskRole::Planner,
        TaskRole::Generator => WorkflowTaskRole::Generator,
        TaskRole::Reducer => WorkflowTaskRole::Reducer,
        TaskRole::Root => WorkflowTaskRole::Generator,
    }
}
