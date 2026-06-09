//! [`RunHost`] — the narrow seam over agent-core's run-to-completion entry.
//!
//! The launcher drives one async call per request — "run this prompt against this
//! sandbox to completion" — and only needs the resolved Done/Failed disposition
//! back. Hiding agent-core's `run_request` behind this trait keeps the launcher's
//! lifecycle logic (run-meta writes, cancellation, reaping) testable against a
//! fake without standing up a full [`AgentCoreRuntime`] graph (its in-memory test
//! construction lives crate-local to `eos-agent-core`, unreachable from here). This
//! is a load-bearing substitution boundary, so it is a `dyn` trait by intent.
//!
//! Production wiring is [`RuntimeHost`]: it owns the per-deployment
//! `workspace_root` + [`WorkflowConfig`], assembles the per-request
//! [`RequestRunInput`], and calls [`eos_agent_core::run_request`].

use async_trait::async_trait;

use eos_agent_core::{run_request, AgentCoreRuntime, EngineEventSink, RequestRunInput};
use eos_types::TaskStatus;
use eos_types::{RequestId, SandboxId};
use eos_workflow::WorkflowConfig;

/// How a completed (non-cancelled) host run resolved. Cancellation is owned by the
/// launcher (it drops the run future), so it is not a host outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// The root agent submitted its outcome and the root task closed `Done`.
    Done,
    /// The root task closed `Failed`, or `run_request` itself errored (provision,
    /// row creation, or read-back). Both surface to the user as a failed run.
    Failed,
}

/// The run-to-completion seam the launcher depends on.
///
/// One call runs the request's root agent inline through agent-core and returns
/// the resolved [`RunOutcome`]. Implementations must be cancel-safe at `.await`
/// points: the launcher races this future against a cancellation token and drops
/// it on cancel (agent-core's own framework guard handles the abandoned root
/// task; backend leaves that row as supporting detail per spec).
#[async_trait]
pub trait RunHost: Send + Sync {
    /// Run `prompt` for `request_id` bound to `sandbox_id`, forwarding
    /// `on_event` as the engine stream callback. Returns the resolved outcome.
    async fn run(
        &self,
        request_id: RequestId,
        prompt: String,
        sandbox_id: SandboxId,
        on_event: Option<EngineEventSink>,
    ) -> RunOutcome;
}

/// Production [`RunHost`]: the backend composition root's handle on agent-core.
///
/// Holds the injected [`AgentCoreRuntime`] graph plus the per-deployment run
/// template (`workspace_root` + [`WorkflowConfig`]) — v1 has no per-request
/// workflow override, so these are constant across requests. The resolved
/// `sandbox_id` is threaded into [`RequestRunInput`] so `run_request`'s internal
/// gateway acquire short-circuits to the binding the launcher already pinned
/// (idempotent per request id).
pub struct RuntimeHost {
    services: AgentCoreRuntime,
    workspace_root: String,
    workflow_config: WorkflowConfig,
}

impl std::fmt::Debug for RuntimeHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeHost")
            .field("workspace_root", &self.workspace_root)
            .finish_non_exhaustive()
    }
}

impl RuntimeHost {
    /// Build the production host from the injected services and run template.
    #[must_use]
    pub fn new(
        services: AgentCoreRuntime,
        workspace_root: impl Into<String>,
        workflow_config: WorkflowConfig,
    ) -> Self {
        Self {
            services,
            workspace_root: workspace_root.into(),
            workflow_config,
        }
    }
}

#[async_trait]
impl RunHost for RuntimeHost {
    async fn run(
        &self,
        request_id: RequestId,
        prompt: String,
        sandbox_id: SandboxId,
        on_event: Option<EngineEventSink>,
    ) -> RunOutcome {
        let input = RequestRunInput::new(
            request_id,
            prompt,
            self.workspace_root.clone(),
            self.workflow_config.clone(),
        )
        .with_sandbox_id(sandbox_id.as_str());
        match run_request(&self.services, input, on_event).await {
            Ok(outcome) if outcome.status == TaskStatus::Done => RunOutcome::Done,
            Ok(_) => RunOutcome::Failed,
            Err(err) => {
                tracing::warn!(error = %err, "run_request returned an error; resolving run as failed");
                RunOutcome::Failed
            }
        }
    }
}
