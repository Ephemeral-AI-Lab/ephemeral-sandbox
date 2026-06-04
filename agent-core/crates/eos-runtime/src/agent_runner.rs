//! The `eos-workflow` [`AgentRunner`] adapter: runs one delegated-workflow agent
//! (planner / generator / reducer) through the shared engine loop.
//!
//! **Path A-recording (Phase-7 complete).** This runner is a thin engine-run
//! wrapper. The submit tool drives the harness *during* the run: a
//! `submit_planner/generator/reducer_outcome` resolves the wired recording
//! [`PlanSubmissionPort`] from `ExecutionMetadata.plan_submission` and records
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
use eos_engine::{run_ephemeral_agent, EphemeralRunInput, NotificationService};
use eos_llm_client::Message;
use eos_tools::{
    BackgroundSupervisorPort, CommandSessionSupervisorPort, NotificationSink, PlanSubmissionPort,
    WorkflowControlPort,
};
use eos_types::AgentRunId;
use eos_workflow::{AgentLaunch, AgentRunReport, AgentRunner, Result as WorkflowResult};

use crate::app_state::AppState;
use crate::tool_context::{build_metadata, MetadataParams};

/// Runtime adapter over the shared engine loop, supplied to `AttemptDeps.runner`.
pub(crate) struct RuntimeAgentRunner {
    state: AppState,
    /// The recording plan-submission port (the wired `PlanSubmissionAdapter`
    /// over the shared attempt registry). Stateless and shared across all runs.
    plan_submission: Arc<dyn PlanSubmissionPort>,
    /// The workflow-control port, late-bound at composition (it is built
    /// downstream of this runner via the starter→attempt_deps→runner chain).
    /// `get()` is `Some` by the time any run starts, so workflow agents' hooks
    /// can read `workflow_depth` (deferral) and `find_outstanding` (no-inflight).
    workflow_control: Arc<OnceLock<Arc<dyn WorkflowControlPort>>>,
    background_supervisor: Arc<dyn BackgroundSupervisorPort>,
    command_session_supervisor: Arc<dyn CommandSessionSupervisorPort>,
    notifier: NotificationService,
}

impl std::fmt::Debug for RuntimeAgentRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAgentRunner").finish_non_exhaustive()
    }
}

impl RuntimeAgentRunner {
    pub(crate) fn new(
        state: AppState,
        plan_submission: Arc<dyn PlanSubmissionPort>,
        workflow_control: Arc<OnceLock<Arc<dyn WorkflowControlPort>>>,
        background_supervisor: Arc<dyn BackgroundSupervisorPort>,
        command_session_supervisor: Arc<dyn CommandSessionSupervisorPort>,
        notifier: NotificationService,
    ) -> Self {
        Self {
            state,
            plan_submission,
            workflow_control,
            background_supervisor,
            command_session_supervisor,
            notifier,
        }
    }
}

#[async_trait]
impl AgentRunner for RuntimeAgentRunner {
    async fn run(&self, launch: AgentLaunch) -> WorkflowResult<AgentRunReport> {
        let Some(agent_def) = launch.agent_def.clone() else {
            return Ok(AgentRunReport::failed(
                "workflow launch carried no agent definition",
            ));
        };

        let agent_run_id = AgentRunId::new_v4();
        let sink: Arc<dyn NotificationSink> = Arc::new(self.notifier.clone());
        let metadata = build_metadata(
            &self.state,
            MetadataParams {
                agent_name: launch.agent_name.clone(),
                sandbox_id: None,
                agent_run_id: agent_run_id.clone(),
                request_id: Some(launch.request_id.clone()),
                task_id: Some(launch.task_id.clone()),
                attempt_id: launch.attempt_id.clone(),
                workflow_id: launch.workflow_id.clone(),
                workflow_control: self.workflow_control.get().cloned(),
                plan_submission: Some(self.plan_submission.clone()),
                background_supervisor: Some(self.background_supervisor.clone()),
                command_session_supervisor: Some(self.command_session_supervisor.clone()),
                notifications: sink,
            },
        );

        let mut prompt = launch.context.clone();
        if let Some(guidance) = &launch.task_guidance {
            prompt.push_str("\n\n");
            prompt.push_str(guidance);
        }
        if let Some(skill) = &launch.skill {
            prompt.push_str("\n\n");
            prompt.push_str(skill);
        }

        let run = run_ephemeral_agent(
            &self.state.engine_run_handles(),
            EphemeralRunInput {
                agent: agent_def,
                initial_messages: vec![Message::from_user_text(prompt)],
                task_id: Some(launch.task_id.clone()),
                agent_run_id,
                tool_metadata: metadata,
                notifier: self.notifier.clone(),
                persist_agent_run: true,
            },
            None,
        )
        .await;

        // The submit tool already recorded the agent's submission during the run
        // (Path A-recording); the runner reports only a framework fault, which
        // the loop uses as the still-RUNNING exhaustion summary for a dead agent.
        Ok(AgentRunReport {
            failure_summary: run.error,
        })
    }
}
