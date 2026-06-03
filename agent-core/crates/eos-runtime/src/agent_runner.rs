//! The `eos-workflow` [`AgentRunner`] adapter: runs one delegated-workflow agent
//! (planner / generator / reducer) through the shared engine loop.
//!
//! **Phase-6 scope.** The orchestrator drives `runner.run(launch)` (in a spawned
//! task) and applies the returned `AgentRunReport::terminal`. Capturing a *typed*
//! terminal (`PlannerPlan` / `GeneratorSubmission` / `ReducerSubmission`) from a
//! generic engine run requires a capturing `PlanSubmissionPort`, which would
//! otherwise double-apply against the real `PlanSubmissionAdapter`; that is the
//! Phase-7 delegated-execution gate. Here the workflow agent runs with
//! `plan_submission = None`, so the run never yields a typed terminal and this
//! adapter reports `no_terminal`. The orchestrator then closes the attempt
//! cleanly (`synthesize_planner_failure`) — the parent task is never mutated
//! (GC-eos-runtime-03).

use std::sync::Arc;

use async_trait::async_trait;
use eos_llm_client::Message;
use eos_tools::SubagentSupervisorPort;
use eos_types::AgentRunId;
use eos_workflow::{AgentLaunch, AgentRunReport, AgentRunner, Result as WorkflowResult};

use crate::agent_loop::{run_ephemeral_agent, EphemeralRunInput};
use crate::app_state::AppState;
use crate::tool_context::{build_metadata, MetadataParams};

/// Runtime adapter over the shared engine loop, supplied to `AttemptDeps.runner`.
pub(crate) struct RuntimeAgentRunner {
    state: AppState,
    subagent_supervisor: Arc<dyn SubagentSupervisorPort>,
}

impl std::fmt::Debug for RuntimeAgentRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAgentRunner").finish_non_exhaustive()
    }
}

impl RuntimeAgentRunner {
    pub(crate) fn new(
        state: AppState,
        subagent_supervisor: Arc<dyn SubagentSupervisorPort>,
    ) -> Self {
        Self {
            state,
            subagent_supervisor,
        }
    }
}

#[async_trait]
impl AgentRunner for RuntimeAgentRunner {
    async fn run(&self, launch: AgentLaunch) -> WorkflowResult<AgentRunReport> {
        let Some(agent_def) = launch.agent_def.clone() else {
            return Ok(AgentRunReport::no_terminal(
                "workflow launch carried no agent definition",
            ));
        };

        let agent_run_id = AgentRunId::new_v4();
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
                workflow_control: None,
                subagent_supervisor: Some(self.subagent_supervisor.clone()),
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
            &self.state,
            EphemeralRunInput {
                agent: agent_def,
                initial_messages: vec![Message::from_user_text(prompt)],
                task_id: Some(launch.task_id.clone()),
                agent_run_id,
                tool_metadata: metadata,
                persist_agent_run: true,
            },
            None,
        )
        .await;

        let summary = run.error.unwrap_or_else(|| {
            "workflow agent produced no typed terminal (phase-6 delegated-execution residual)"
                .to_owned()
        });
        Ok(AgentRunReport::no_terminal(summary))
    }
}
