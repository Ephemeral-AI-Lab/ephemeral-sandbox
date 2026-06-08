use async_trait::async_trait;
use eos_types::{AgentRunId, SubagentSessionId};
use serde::Serialize;

use crate::{Sealed, ToolResult};

/// Typed launch rejection facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentLaunchRejection {
    /// The caller is already a subagent.
    Recursive,
    /// The requested agent name is not registered.
    NotRegistered {
        /// Requested agent name.
        agent_name: String,
    },
    /// The requested agent exists but is not subagent-typed.
    NotSubagent {
        /// Requested agent name.
        agent_name: String,
        /// Registered agent type string.
        agent_type: String,
    },
}

/// Background-session status facts returned for subagent control tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentSessionStatus {
    /// The subagent is still running.
    Running,
    /// The subagent called its terminal tool.
    Completed,
    /// The subagent crashed or exited without terminal output.
    Failed,
    /// The subagent was cancelled.
    Cancelled,
    /// The subagent result was already delivered.
    Delivered,
}

/// Result of looking up a tracked subagent session.
#[derive(Debug, Clone)]
pub enum SubagentProgress {
    /// The session exists.
    Found {
        /// Agent-facing subagent session id.
        subagent_session_id: SubagentSessionId,
        /// Current tracked status.
        status: SubagentSessionStatus,
        /// Registered subagent name.
        agent_name: String,
        /// Terminal result, when available.
        result: Option<ToolResult>,
    },
    /// The session id is unknown to the owning run.
    Missing {
        /// Agent-facing subagent session id that was requested.
        subagent_session_id: SubagentSessionId,
    },
}

/// Result of a `cancel_subagent` request.
#[derive(Debug, Clone)]
pub enum CancelledSubagent {
    /// A running subagent was cancelled.
    Cancelled {
        /// Agent-facing subagent session id.
        subagent_session_id: SubagentSessionId,
        /// User/tool supplied cancellation reason.
        reason: String,
    },
    /// The session id is unknown or already terminal.
    MissingOrSettled {
        /// Agent-facing subagent session id that could not be cancelled.
        subagent_session_id: SubagentSessionId,
    },
}

/// Per-kind in-flight background-session count for one agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BackgroundSessionCounts {
    /// `subagents + workflows + command_sessions`.
    pub total: usize,
    /// In-flight subagent runs for this agent run.
    pub subagents: usize,
    /// Outstanding delegated workflows for this agent run.
    pub workflows: usize,
    /// In-flight background-tracked command sessions for this agent run.
    pub command_sessions: usize,
}

/// Subagent background-session registry for one owning agent run.
#[async_trait]
pub trait SubagentSessionPort: Sealed + Send + Sync {
    /// Register a started child agent run as a background session.
    async fn register_background_session(
        &self,
        agent_run_id: &AgentRunId,
        agent_name: &str,
    ) -> SubagentSessionId;

    /// Snapshot one tracked subagent session for model-facing rendering.
    async fn subagent_session_snapshot(
        &self,
        subagent_session_id: &SubagentSessionId,
    ) -> Option<SubagentProgress>;

    /// Cancel one tracked subagent session.
    async fn cancel_background_session(
        &self,
        subagent_session_id: &SubagentSessionId,
        reason: &str,
    ) -> CancelledSubagent;

    /// Cancel one tracked subagent by its natural child agent-run id.
    async fn cancel_background_agent_run(
        &self,
        agent_run_id: &AgentRunId,
        reason: &str,
    ) -> bool;

    /// Count running background sessions for this run.
    async fn count_background_sessions(&self) -> usize;

    /// Cancel all running background sessions for this run.
    async fn cancel_all_background_sessions(&self, reason: &str);

    /// Poll terminal child runs and push notifications.
    async fn poll_complete_background_sessions(&self) -> usize;
}
