//! `eos-engine` — one agent query loop, tool dispatch, background
//! supervision, notifications, prompt reports, and the event-source seam.
#![forbid(unsafe_code)]

pub mod agent;
pub mod background;
mod notifications;
pub mod prompt;
pub mod query;
mod runtime;
mod support;
mod telemetry;
pub mod tool_call;

pub use agent::{build_query_context, BuildQueryContextInput};
pub use background::{
    spawn_command_completion_heartbeat, BackgroundSupervisorHandle, BackgroundTaskStatus,
    BackgroundTaskSupervisor, CommandSessionRecord,
};
pub use notifications::{make_default_notification_rules, NotificationRule, NotificationService};
pub use query::{
    build_query_run_request, run_query, terminal_submission_failed, EngineStream, EventSource,
    ProviderEventSource, QueryContext, QueryExitReason, QueryRunRequest, QueryStream,
};
pub use runtime::{
    run_agent, AgentRunInput, AgentRunResult, EngineRunHandles, EventCallback, EventSourceFactory,
    ToolRegistryExtender,
};
pub use support::EngineError;
pub use telemetry::{stamp_identity, AssistantMessageComplete, PromptReportRecorder, StreamEvent};
