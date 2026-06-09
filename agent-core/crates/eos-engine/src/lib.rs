//! `eos-engine` — one agent query loop, tool dispatch, background
//! session accounting, notifications, prompt reports, and the provider-stream seam.
#![forbid(unsafe_code)]

pub mod agent_loop;
pub mod background;
mod notifications;
pub mod query;
mod support;
mod telemetry;
pub mod tool_call;

pub use agent_loop::{
    AgentLoopToolRegistryBuildInput, AgentLoopToolRegistryFactory, BackgroundSessionInputs,
    ExecutionMetadataBuildInput, TokioAgentLoopLauncher, ToolCallHookStores,
    ToolExecutionMetadataReader,
};
pub use background::{
    BackgroundCompletion, BackgroundManagers, BackgroundNotificationEmitter,
    BackgroundSessionStatus, BackgroundSessionTeardown,
};
pub use eos_types::{
    AgentLoopCancellation, AgentLoopCancellationHandle, AgentLoopLauncher, AgentLoopMessage,
    AgentLoopOutcome, AgentLoopOutcomeFuture, AgentLoopOutcomeKind, StartAgentLoopRequest,
    StartedAgentLoop,
};
pub use notifications::{
    make_default_notification_rules, EngineNotificationQueue, NotificationRule,
    NotificationRuleContext,
};
pub use query::{
    EngineEventSink, EngineStream, LlmProviderStreamSource, ProviderStreamSource,
    ProviderStreamSourceFactory,
};
pub use support::EngineError;
pub use telemetry::{stamp_identity, AssistantMessageComplete, PromptReportRecorder, StreamEvent};
