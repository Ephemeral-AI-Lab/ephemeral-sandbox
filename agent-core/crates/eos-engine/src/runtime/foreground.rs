//! [`ForegroundExecutor`] — the per-agent-run registry of foreground cancelable
//! effects (spec §6.5).
//!
//! Foreground work is awaited inline by the query loop, so it needs no records,
//! heartbeat, progress delivery, or notification latches — only
//! cancel-reachability. The existing foreground `JoinSet` remains the execution
//! substrate; this type is **not** a mirror supervisor. It exists so that, on
//! cancellation, the run can reach the effects its tools spawned (inline advisor
//! runs and any registered [`CancelableResource`]) and tear them down.
//!
//! `exec_command` is deliberately **not** a foreground `CancelableResource`: its
//! active future is dropped by the foreground `JoinSet` abort on cancel, and a
//! daemon-owned running command session is torn down by the
//! `CommandSessionManager`'s one per-caller daemon RPC, not a per-invocation
//! resource.

use std::collections::HashMap;
use std::sync::Mutex;

use eos_tools::{CancelPort, CancelableResource, ToolError};
use eos_types::AgentRunId;

/// Request-scoped, stateless factory for per-run [`ForegroundExecutor`]s.
///
/// It carries no per-agent mutable state; it exists only to keep
/// foreground/background construction symmetric under `AgentRunControlFactory`.
#[derive(Clone, Default, Debug)]
pub struct ForegroundExecutorFactory;

impl ForegroundExecutorFactory {
    /// Build a fresh foreground executor for one agent run.
    #[must_use]
    pub fn create(&self, agent_run_id: AgentRunId) -> ForegroundExecutor {
        ForegroundExecutor::new(agent_run_id)
    }
}

/// Stable id for a registered foreground resource.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForegroundResourceId(pub String);

/// A foreground inline child agent run (e.g. an `ask_advisor` dispatch) that
/// cancellation must reach via [`CancelPort::cancel_agent_run`].
#[derive(Debug, Clone)]
pub struct InlineAgentRunHandle {
    agent_run_id: AgentRunId,
}

/// Per-agent-run registry of foreground cancelable effects.
pub struct ForegroundExecutor {
    agent_run_id: AgentRunId,
    resources: Mutex<HashMap<ForegroundResourceId, std::sync::Arc<dyn CancelableResource>>>,
    inline_agent_runs: Mutex<HashMap<AgentRunId, InlineAgentRunHandle>>,
}

impl std::fmt::Debug for ForegroundExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForegroundExecutor")
            .field("agent_run_id", &self.agent_run_id)
            .finish_non_exhaustive()
    }
}

impl ForegroundExecutor {
    fn new(agent_run_id: AgentRunId) -> Self {
        Self {
            agent_run_id,
            resources: Mutex::new(HashMap::new()),
            inline_agent_runs: Mutex::new(HashMap::new()),
        }
    }

    /// Register a foreground cancelable resource under a stable id.
    pub fn register_resource(
        &self,
        id: ForegroundResourceId,
        resource: std::sync::Arc<dyn CancelableResource>,
    ) {
        self.resources
            .lock()
            .expect("foreground lock")
            .insert(id, resource);
    }

    /// Drop a foreground resource once its tool call has settled.
    pub fn unregister_resource(&self, id: &ForegroundResourceId) {
        self.resources.lock().expect("foreground lock").remove(id);
    }

    /// Record an inline child agent run so cancellation can recurse into it.
    pub fn register_inline_agent_run(&self, agent_run_id: AgentRunId) {
        self.inline_agent_runs
            .lock()
            .expect("foreground lock")
            .insert(agent_run_id.clone(), InlineAgentRunHandle { agent_run_id });
    }

    /// Forget an inline child agent run once it has been awaited.
    pub fn unregister_inline_agent_run(&self, agent_run_id: &AgentRunId) {
        self.inline_agent_runs
            .lock()
            .expect("foreground lock")
            .remove(agent_run_id);
    }

    /// Tear down every registered foreground effect: cancel inline child runs
    /// through the recursive [`CancelPort`], then tear down registered
    /// resources. Errors are logged and do not abort the sweep.
    pub async fn teardown(
        &self,
        cancel_port: &dyn CancelPort,
        reason: &str,
    ) -> Result<(), ToolError> {
        let inline: Vec<AgentRunId> = {
            let mut guard = self.inline_agent_runs.lock().expect("foreground lock");
            guard
                .drain()
                .map(|(_, handle)| handle.agent_run_id)
                .collect()
        };
        for agent_run_id in inline {
            if let Err(err) = cancel_port.cancel_agent_run(&agent_run_id, reason).await {
                tracing::warn!(
                    error = %err,
                    agent_run_id = agent_run_id.as_str(),
                    "foreground inline-run cancellation failed"
                );
            }
        }
        let resources: Vec<std::sync::Arc<dyn CancelableResource>> = {
            let mut guard = self.resources.lock().expect("foreground lock");
            guard.drain().map(|(_, resource)| resource).collect()
        };
        for resource in resources {
            if let Err(err) = resource.teardown(reason).await {
                tracing::warn!(error = %err, "foreground resource teardown failed");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::{Arc, Mutex as StdMutex};

    use async_trait::async_trait;
    use eos_types::TaskId;

    use super::*;

    #[derive(Debug, Default)]
    struct RecordingCancelPort {
        cancelled_runs: StdMutex<Vec<String>>,
    }

    impl RecordingCancelPort {
        fn cancelled_runs(&self) -> Vec<String> {
            self.cancelled_runs.lock().expect("lock").clone()
        }
    }

    #[async_trait]
    impl CancelPort for RecordingCancelPort {
        async fn cancel_task(&self, _task_id: &TaskId, _reason: &str) -> Result<(), ToolError> {
            Ok(())
        }

        async fn cancel_agent_run(
            &self,
            agent_run_id: &AgentRunId,
            _reason: &str,
        ) -> Result<(), ToolError> {
            self.cancelled_runs
                .lock()
                .expect("lock")
                .push(agent_run_id.as_str().to_owned());
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingResource {
        torn_down: StdMutex<bool>,
    }

    #[async_trait]
    impl CancelableResource for RecordingResource {
        async fn teardown(&self, _reason: &str) -> Result<(), ToolError> {
            *self.torn_down.lock().expect("lock") = true;
            Ok(())
        }
    }

    // §7.1/§17: `ask_advisor` registers an inline advisor run; cancellation reaches
    // it through `CancelPort::cancel_agent_run`. Registered `CancelableResource`s
    // are also torn down. After teardown the maps are drained (a second teardown is
    // a no-op), so cancellation cannot double-fire.
    #[tokio::test]
    async fn teardown_cancels_inline_advisor_run_and_resources() {
        let executor = ForegroundExecutorFactory.create("owner-run".parse().expect("agent run id"));
        let advisor_run: AgentRunId = "advisor-run".parse().expect("agent run id");
        executor.register_inline_agent_run(advisor_run.clone());
        let resource = Arc::new(RecordingResource::default());
        executor.register_resource(ForegroundResourceId("advisor".to_owned()), resource.clone());

        let cancel_port = RecordingCancelPort::default();
        executor
            .teardown(&cancel_port, "parent cancelled")
            .await
            .expect("teardown");

        assert_eq!(
            cancel_port.cancelled_runs(),
            vec!["advisor-run".to_owned()],
            "the inline advisor run is cancelled via cancel_agent_run"
        );
        assert!(
            *resource.torn_down.lock().expect("lock"),
            "registered foreground resources are torn down"
        );

        // A second teardown finds the drained maps and issues no further cancels.
        executor
            .teardown(&cancel_port, "again")
            .await
            .expect("teardown");
        assert_eq!(
            cancel_port.cancelled_runs().len(),
            1,
            "teardown is one-shot"
        );
    }
}
