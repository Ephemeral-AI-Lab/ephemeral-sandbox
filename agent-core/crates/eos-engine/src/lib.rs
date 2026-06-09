//! `eos-engine` — one agent query loop, tool dispatch, background
//! session accounting, notifications, prompt reports, and the provider-stream seam.
#![forbid(unsafe_code)]

pub mod agent_loop;
pub mod background;
pub mod event;
mod notifications;
pub mod provider_stream;
pub mod records;
mod support;
mod telemetry;
pub mod tool_call;

pub use agent_loop::{
    AgentLoopToolRegistryBuildInput, AgentLoopToolRegistryFactory, BackgroundSessionRuntimeFactory,
    ExecutionMetadataBuildInput, TokioAgentLoopLauncher, ToolCallHookStores,
    ToolExecutionMetadataReader,
};
pub use background::{
    BackgroundCompletion, BackgroundNotificationEmitter, BackgroundSessionRuntime,
    BackgroundSessionStatus, BackgroundSessionTeardown,
};
pub use event::{
    stamp_identity, AssistantMessageComplete, EngineEventOutputs, EngineEventPrinter,
    EngineEventSink, StreamEvent,
};
pub use notifications::{
    make_default_notification_rules, EngineNotificationQueue, NotificationRule,
    NotificationRuleContext,
};
pub use provider_stream::{
    EngineStream, LlmProviderStreamSource, ProviderStreamSource, ProviderStreamSourceFactory,
};
pub use support::EngineError;
pub use telemetry::PromptReportRecorder;
