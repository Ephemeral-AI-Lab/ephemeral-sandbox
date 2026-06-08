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
use eos_agent_def::AgentRole;
use eos_agent_message_records::{AgentRunRecordKind, WorkflowTaskRole};
use eos_agent_run::AgentRunApi;
use eos_engine::{
    run_agent, AgentRunControlFactory, AgentRunInput, AgentRunRegistry, BackgroundTeardownPort,
};
use eos_llm_client::Message;
use eos_tools::{
    AttemptSubmissionPort, AttemptSubmissionService, CommandSessionPort, SubagentSessionPort,
    WorkflowServicePort, WorkflowSessionPort,
};
use eos_types::AgentRunId;
use eos_workflow::{AgentLaunch, AgentRunReport, AgentRunner, Result as WorkflowResult};

use crate::runtime_services::RuntimeServices;
use crate::tool_context::{build_metadata, MetadataParams};

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
    workflow_service: Arc<OnceLock<Arc<dyn WorkflowServicePort>>>,
    /// Request-scoped factory that mints one fresh `AgentRunControl` (notifier,
    /// foreground, background supervisor, heartbeat, cancellation) per run — the
    /// runner stores no per-agent mutable supervisor or notifier.
    control_factory: Arc<AgentRunControlFactory>,
    /// Live-run registry for recursive cancellation.
    agent_run_registry: AgentRunRegistry,
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
        workflow_service: Arc<OnceLock<Arc<dyn WorkflowServicePort>>>,
        control_factory: Arc<AgentRunControlFactory>,
        agent_run_registry: AgentRunRegistry,
    ) -> Self {
        Self {
            services,
            workspace_root: workspace_root.into(),
            attempt_submission,
            workflow_service,
            control_factory,
            agent_run_registry,
        }
    }
}

#[async_trait]
impl AgentRunner for RuntimeAgentRunner {
    async fn run(&self, launch: AgentLaunch) -> WorkflowResult<AgentRunReport> {
        let agent_run_id = AgentRunId::new_v4();
        // Each delegated planner/generator/reducer run owns its own
        // AgentRunControl — a fresh notifier, foreground executor, background
        // supervisor, command-completion heartbeat, and cancellation token.
        let control = self
            .control_factory
            .persisted(agent_run_id.clone(), Some(launch.task_id().clone()));
        self.agent_run_registry.insert(control.clone());
        let background = control.background();
        let agent_run_service: Arc<dyn AgentRunApi> = Arc::new(background.clone());
        let subagent_sessions: Arc<dyn SubagentSessionPort> = Arc::new(background.clone());
        let workflow_sessions: Arc<dyn WorkflowSessionPort> = Arc::new(background.clone());
        let background_session: Arc<dyn BackgroundTeardownPort> = Arc::new(background.clone());
        let command_session_port: Arc<dyn CommandSessionPort> = Arc::new(background);
        let metadata = build_metadata(
            &self.workspace_root,
            MetadataParams {
                agent_name: launch.agent_name().to_owned(),
                sandbox_id: None,
                agent_run_id: agent_run_id.clone(),
                request_id: Some(launch.request_id().clone()),
                task_id: Some(launch.task_id().clone()),
                attempt_id: Some(launch.attempt_id().clone()),
                workflow_id: Some(launch.workflow_id().clone()),
                is_isolated_workspace_mode: false,
            },
        );

        let mut prompt = launch.context().to_owned();
        if let Some(guidance) = launch.task_guidance() {
            prompt.push_str("\n\n");
            prompt.push_str(guidance);
        }
        if let Some(skill) = launch.skill() {
            prompt.push_str("\n\n");
            prompt.push_str(skill);
        }

        let run = run_agent(
            &self.services.engine_run_handles(&self.workspace_root),
            AgentRunInput {
                agent: launch.agent_def().clone(),
                initial_messages: vec![Message::from_user_text(prompt)],
                task_id: Some(launch.task_id().clone()),
                agent_run_id,
                tool_metadata: metadata,
                attempt_submission: Some(AttemptSubmissionService::new(
                    self.attempt_submission.clone(),
                )),
                agent_run_service: Some(agent_run_service),
                subagent_sessions: Some(subagent_sessions),
                workflow_service: self.workflow_service.get().cloned(),
                workflow_sessions: Some(workflow_sessions),
                background_session: Some(background_session),
                command_session_port: Some(command_session_port),
                notifier: control.notifications(),
                cancellation: control.cancellation(),
                foreground: control.foreground(),
                agent_run_registry: Some(self.agent_run_registry.clone()),
                persist_agent_run: true,
                record_kind: AgentRunRecordKind::WorkflowTask {
                    workflow_id: launch.workflow_id().clone(),
                    iteration_id: launch.iteration_id().clone(),
                    attempt_id: launch.attempt_id().clone(),
                    role: workflow_message_record_role(launch.role()),
                },
            },
            None,
        )
        .await;

        // `run_agent` claims and finalizes (or skips, if a concurrent cancel won)
        // the row + child teardown, removing the live-run entry. Dropping `control`
        // releases its heartbeat (RAII).
        drop(control);

        // The submit tool already recorded the agent's submission during the run
        // (Path A-recording); the runner reports only a framework fault, which
        // the loop uses as the still-RUNNING exhaustion summary for a dead agent.
        Ok(AgentRunReport {
            failure_summary: run.error,
        })
    }
}

fn workflow_message_record_role(role: AgentRole) -> WorkflowTaskRole {
    match role {
        AgentRole::Planner => WorkflowTaskRole::Planner,
        AgentRole::Generator => WorkflowTaskRole::Generator,
        AgentRole::Reducer => WorkflowTaskRole::Reducer,
        AgentRole::Root | AgentRole::Helper | AgentRole::Subagent => WorkflowTaskRole::Generator,
    }
}
