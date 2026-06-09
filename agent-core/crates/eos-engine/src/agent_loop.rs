//! Public non-blocking agent-loop API and internal loop executor.

mod contracts;
mod executor;
mod launcher;
mod state;

pub use contracts::{
    AgentLoopToolRegistryBuildInput, AgentLoopToolRegistryFactory, BackgroundSessionRuntimeFactory,
    ExecutionMetadataBuildInput, ToolCallHookStores, ToolExecutionMetadataReader,
};
pub(crate) use eos_types::{
    AgentLoopMessage, AgentLoopOutcome, AgentLoopOutcomeKind, StartAgentLoopRequest,
};
pub use launcher::TokioAgentLoopLauncher;

pub(crate) use contracts::tool_result_payload;
pub(crate) use executor::{AgentLoopExecutor, AgentLoopExecutorInput};
pub(crate) use launcher::AgentLoopCancelSignal;
pub(crate) use launcher::AgentLoopProviderStream;
pub(crate) use state::{AgentLoopRunServices, AgentLoopState};
