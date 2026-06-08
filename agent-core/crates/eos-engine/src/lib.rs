//! `eos-engine` — one agent query loop, tool dispatch, background
//! session accounting, notifications, prompt reports, and the event-source seam.
#![forbid(unsafe_code)]

pub mod agent_loop;
pub mod background;
mod notifications;
pub mod query;
mod runtime;
mod support;
mod telemetry;
pub mod tool_call;

pub use agent_loop::{
    start_agent_loop, AgentLoopToolRegistryBuildInput, AgentLoopToolRegistryFactory,
    TokioAgentLoopLauncher,
};
pub use background::{
    BackgroundCompletion, BackgroundManagers, BackgroundNotificationEmitter,
    BackgroundSessionStatus, BackgroundTeardownService,
};
pub use notifications::{make_default_notification_rules, NotificationRule, NotificationService};
pub use query::{
    build_query_run_request, run_query, terminal_submission_failed, EngineStream, EventCallback,
    EventSource, EventSourceFactory, ProviderEventSource, QueryContext, QueryExitReason,
    QueryRunRequest, QueryStream,
};
pub use runtime::{
    AgentRunCancellation, AgentRunControl, AgentRunFinalization, AgentRunRegistry,
    EngineCancelPort, ForegroundExecutor, ForegroundExecutorFactory, ForegroundResourceId,
};
pub use support::EngineError;
pub use telemetry::{stamp_identity, AssistantMessageComplete, PromptReportRecorder, StreamEvent};
