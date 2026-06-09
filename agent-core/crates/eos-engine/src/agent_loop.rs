//! Public non-blocking agent-loop API and internal loop executor.

mod agent_loop_executor;
mod agent_loop_state;
mod contracts;
mod launcher;
mod loop_hooks;

pub use contracts::{
    AgentLoopToolRegistryBuildInput, AgentLoopToolRegistryFactory, BackgroundSessionInputs,
    ExecutionMetadataBuildInput, ToolCallHookStores, ToolExecutionMetadataReader,
};
pub use eos_types::{
    AgentLoopCancellation, AgentLoopCancellationHandle, AgentLoopLauncher, AgentLoopMessage,
    AgentLoopOutcome, AgentLoopOutcomeFuture, AgentLoopOutcomeKind, StartAgentLoopRequest,
    StartedAgentLoop,
};
pub use launcher::TokioAgentLoopLauncher;

pub(crate) use agent_loop_executor::AgentLoopExecutor;
pub(crate) use agent_loop_state::{AgentLoopRunServices, AgentLoopState};
pub(crate) use contracts::tool_result_payload;
pub(crate) use launcher::AgentLoopCancelSignal;
pub(crate) use launcher::AgentLoopProviderStream;
pub(crate) use loop_hooks::{AgentLoopHooks, NoopAgentLoopHooks};
