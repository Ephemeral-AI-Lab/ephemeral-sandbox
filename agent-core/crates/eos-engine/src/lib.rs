//! `eos-engine` — one ephemeral agent query loop, tool dispatch, background
//! supervision, notifications, prompt reports, and the event-source seam.
#![forbid(unsafe_code)]

mod advisor;
pub mod agent;
pub mod agent_loop;
pub mod audit;
pub mod background;
mod error;
mod events;
mod notifications;
pub mod prompt;
mod prompt_report;
pub mod query;
pub mod tool_call;

#[cfg(test)]
mod test_support;

pub use agent::{build_query_context, BuildQueryContextInput};
pub use agent_loop::{
    run_ephemeral_agent, EngineRunHandles, EphemeralRun, EphemeralRunInput, EventCallback,
    EventSourceFactory,
};
pub use background::{
    spawn_command_completion_heartbeat, BackgroundSupervisorHandle, BackgroundTaskStatus,
    BackgroundTaskSupervisor, CommandSessionRecord,
};
pub use error::EngineError;
pub use events::{stamp_identity, AssistantMessageComplete, StreamEvent};
pub use notifications::{make_default_notification_rules, NotificationRule, NotificationService};
pub use prompt_report::PromptReportRecorder;
pub use query::{
    build_query_run_request, run_query, terminal_submission_failed, EngineStream, EventSource,
    ProviderEventSource, QueryContext, QueryExitReason, QueryRunRequest, QueryStream,
};
