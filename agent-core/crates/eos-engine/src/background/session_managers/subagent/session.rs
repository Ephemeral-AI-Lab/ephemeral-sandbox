use eos_tools::ToolResult;
use eos_types::{AgentRunId, JsonObject, SubagentSessionId};
use tokio::task::AbortHandle;

use super::super::{BackgroundSession, BackgroundSessionStatus};

/// One tracked subagent run owned by an agent run's background session runtime.
#[derive(Debug, Clone)]
pub(in crate::background) struct SubagentSession {
    id: SubagentSessionId,
    #[allow(
        dead_code,
        reason = "Subagent sessions retain the child agent run id for diagnostics"
    )]
    agent_run_id: AgentRunId,
    driver_abort: AbortHandle,
    tool_input: JsonObject,
    status: BackgroundSessionStatus,
    result: Option<ToolResult>,
}

impl SubagentSession {
    pub(super) fn running(
        id: SubagentSessionId,
        agent_run_id: AgentRunId,
        driver_abort: AbortHandle,
        tool_input: JsonObject,
    ) -> Self {
        Self {
            id,
            agent_run_id,
            driver_abort,
            tool_input,
            status: BackgroundSessionStatus::Running,
            result: None,
        }
    }

    pub(super) fn tool_input(&self) -> &JsonObject {
        &self.tool_input
    }

    pub(super) const fn status(&self) -> BackgroundSessionStatus {
        self.status
    }

    pub(super) fn result(&self) -> Option<&ToolResult> {
        self.result.as_ref()
    }

    pub(super) fn cancel(&mut self, reason: &str) -> bool {
        if !matches!(self.status, BackgroundSessionStatus::Running) {
            return false;
        }
        self.status = BackgroundSessionStatus::Cancelled;
        self.result = Some(
            ToolResult::error(format!("Background subagent cancelled: {reason}"))
                .meta("subagent_cancelled", serde_json::json!(true)),
        );
        self.driver_abort.abort();
        true
    }

    pub(super) fn settle(
        &mut self,
        status: BackgroundSessionStatus,
        result: ToolResult,
    ) -> Option<ToolResult> {
        if status.precedence() > self.status.precedence() {
            self.status = status;
            self.result = Some(result);
        }
        self.result.clone()
    }
}

impl BackgroundSession for SubagentSession {
    type Id = SubagentSessionId;

    fn id(&self) -> &Self::Id {
        &self.id
    }
}
