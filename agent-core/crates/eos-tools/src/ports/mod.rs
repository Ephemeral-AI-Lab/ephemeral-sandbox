//! Compatibility re-exports for shared transition contracts.

pub use eos_tool_core::{
    AttemptSubmissionPort, BackgroundSessionCounts, CancelPort, CancelableResource,
    CancelledSubagent, CommandServicePort, CommandSessionPort, NotificationSink,
    OutstandingWorkflow, PlanReducer, PlanTask, PlannerPlan, Sealed, StartWorkflowRequest,
    StartedWorkflow, SubagentLaunchRejection, SubagentProgress, SubagentSessionPort,
    SubagentSessionStatus, SubmissionAck, SystemNotification, TerminalWorkflow,
    WorkflowServicePort, WorkflowSessionPort,
};
