//! [`BackgroundNotificationEmitter`] (spec §8.4) — the centralized renderer and
//! delivery adapter for model-visible background completion messages.
//!
//! Subagent, workflow, and command-session terminal transitions all produce one
//! [`BackgroundCompletion`], which the emitter renders to a `[BACKGROUND
//! COMPLETED]` message and enqueues into the agent run's own
//! [`NotificationService`]. The emitter wraps the exact service owned by the
//! run's `AgentRunControl`, so a completion reaches the run that owns the work
//! and never another run's queue (spec §13.1). Callers must clone the terminal
//! data out from under any manager lock and drop the lock *before* awaiting `emit`.

use eos_tools::SystemNotification as ToolNotification;
use eos_tools::ToolError;
use eos_tools::ToolResult;
use eos_types::{CommandSessionId, SandboxId, SubagentSessionId, WorkflowId, WorkflowSessionId};
use serde_json::Value;

use super::session_managers::BackgroundSessionStatus;
use crate::notifications::NotificationService;

/// A terminal background transition to surface to the owning agent run.
#[derive(Debug, Clone)]
pub enum BackgroundCompletion {
    /// A subagent run settled (the parent run is the notification target).
    Subagent {
        /// Agent-facing subagent handle id.
        subagent_session_id: SubagentSessionId,
        /// Terminal status.
        status: BackgroundSessionStatus,
        /// The subagent's terminal result.
        result: ToolResult,
    },
    /// A delegated workflow reached a terminal state.
    Workflow {
        /// Agent-facing workflow handle id.
        workflow_task_id: WorkflowSessionId,
        /// The persisted workflow id.
        workflow_id: WorkflowId,
        /// Terminal status.
        status: BackgroundSessionStatus,
    },
    /// A background command session completed (the owner run is the target).
    CommandSession {
        /// Daemon-minted command-session id.
        command_session_id: CommandSessionId,
        /// Owning sandbox.
        sandbox_id: SandboxId,
        /// Terminal status.
        status: BackgroundSessionStatus,
        /// The daemon completion `result` payload.
        result: Value,
    },
}

impl BackgroundCompletion {
    /// The notification dedup/event key (the typed handle id).
    fn event_key(&self) -> String {
        match self {
            Self::Subagent {
                subagent_session_id,
                ..
            } => subagent_session_id.as_str().to_owned(),
            Self::Workflow {
                workflow_task_id, ..
            } => workflow_task_id.as_str().to_owned(),
            Self::CommandSession {
                command_session_id, ..
            } => command_session_id.as_str().to_owned(),
        }
    }

    /// Render the model-visible `[BACKGROUND COMPLETED]` body. The payload names
    /// the background kind and its typed handle id so the model can call the
    /// matching progress/check tool for details.
    fn render(&self) -> String {
        match self {
            Self::Subagent {
                subagent_session_id,
                status,
                result,
            } => format!(
                "[BACKGROUND COMPLETED] subagent_session_id={} status={}\n{}",
                subagent_session_id.as_str(),
                status_token(*status),
                result.output,
            ),
            Self::Workflow {
                workflow_task_id,
                workflow_id,
                status,
            } => format!(
                "[BACKGROUND COMPLETED] workflow_task_id={} workflow_id={} status={}",
                workflow_task_id.as_str(),
                workflow_id.as_str(),
                status_token(*status),
            ),
            Self::CommandSession {
                command_session_id,
                status,
                result,
                ..
            } => {
                let exit = result
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .map_or_else(|| "none".to_owned(), |code| code.to_string());
                let stdout = result
                    .get("output")
                    .and_then(|output| output.get("stdout"))
                    .or_else(|| result.get("stdout"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                format!(
                    "[BACKGROUND COMPLETED] command_session_id={} status={} exit_code={exit}\nstdout: {stdout}",
                    command_session_id.as_str(),
                    status_token(*status),
                )
            }
        }
    }
}

fn status_token(status: BackgroundSessionStatus) -> &'static str {
    match status {
        BackgroundSessionStatus::Running => "running",
        BackgroundSessionStatus::Completed => "completed",
        BackgroundSessionStatus::Failed => "failed",
        BackgroundSessionStatus::Cancelled => "cancelled",
        BackgroundSessionStatus::Delivered => "delivered",
    }
}

/// Centralized renderer + delivery adapter wrapping one agent run's notifier.
#[derive(Clone, Debug, Default)]
pub struct BackgroundNotificationEmitter {
    notifications: NotificationService,
}

impl BackgroundNotificationEmitter {
    /// Wrap the agent run's notification service.
    #[must_use]
    pub fn new(notifications: NotificationService) -> Self {
        Self { notifications }
    }

    /// The wrapped notification service (the exact run-local queue).
    #[must_use]
    pub fn notifications(&self) -> NotificationService {
        self.notifications.clone()
    }

    /// Render and enqueue one background completion into the run's notifier.
    pub async fn emit(&self, completion: BackgroundCompletion) -> Result<(), ToolError> {
        use eos_tools::NotificationSink;
        self.notifications
            .notify_system(ToolNotification {
                event: completion.event_key(),
                message: completion.render(),
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    /// Spec §8.4/§9.1: a subagent completion renders the `[BACKGROUND COMPLETED]`
    /// body with the typed handle id and lands in the wrapped notifier; a second
    /// notifier never sees it (instance isolation, §13.1).
    #[tokio::test]
    async fn emits_subagent_completion_into_its_own_notifier() {
        let notifier = NotificationService::new();
        let other = NotificationService::new();
        let emitter = BackgroundNotificationEmitter::new(notifier.clone());

        emitter
            .emit(BackgroundCompletion::Subagent {
                subagent_session_id: "subagent_1".parse().expect("id"),
                status: BackgroundSessionStatus::Completed,
                result: ToolResult::ok("did the work"),
            })
            .await
            .expect("emit");

        assert!(other.drain().await.is_empty(), "isolated from other runs");
        let drained = notifier.drain().await;
        assert_eq!(drained.len(), 1, "exactly one completion notification");
        assert_eq!(drained[0].event, "subagent_1");
        assert!(drained[0]
            .message
            .starts_with("[BACKGROUND COMPLETED] subagent_session_id=subagent_1"));
        assert!(drained[0].message.contains("status=completed"));
        assert!(drained[0].message.contains("did the work"));
    }
}
