//! `eos-engine` — one agent query loop, tool dispatch, background
//! session accounting, notifications, prompt reports, and the event-source seam.
#![forbid(unsafe_code)]

pub mod agent_loop;
pub mod background;
mod notifications;
pub mod query;
pub mod records;
mod support;
mod telemetry;
pub mod tool_call;

pub use agent_loop::{
    start_agent_loop, AgentExecutionMetadataService, AgentLoopBackgroundDependencies,
    AgentLoopCancelHandle, AgentLoopHookDependencies, AgentLoopLauncher, AgentLoopMessage,
    AgentLoopOutcome, AgentLoopOutcomeKind, AgentLoopToolRegistryBuildInput,
    AgentLoopToolRegistryFactory, ExecutionMetadataBuildInput, StartAgentLoopRequest,
    StartedAgentLoop, TokioAgentLoopLauncher,
};
pub use background::{
    BackgroundCompletion, BackgroundManagers, BackgroundNotificationEmitter,
    BackgroundSessionStatus, BackgroundTeardownService,
};
pub use notifications::{
    make_default_notification_rules, NotificationRule, NotificationRuleContext, NotificationService,
};
pub use query::{
    EngineStream, EventCallback, EventSource, EventSourceFactory, ProviderEventSource,
};
pub use support::EngineError;
pub use telemetry::{stamp_identity, AssistantMessageComplete, PromptReportRecorder, StreamEvent};
