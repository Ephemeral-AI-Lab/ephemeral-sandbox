//! Agent-run and agent-loop contracts shared by runner, engine, and tools.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod agent_loop;
mod agent_name;
mod agent_run_api;
mod agent_run_outcome;
mod agent_state;
mod error;
mod metadata_service;
mod spawn_agent_request;

pub use agent_loop::{
    agent_loop_cancel_pair, AgentLoopCancelHandle, AgentLoopCancelSignal, AgentLoopLauncher,
    AgentLoopMessage, AgentLoopOutcome, AgentLoopOutcomeKind, StartAgentLoopRequest,
    StartedAgentLoop,
};
pub use agent_name::{AgentName, AgentNameError};
pub use agent_run_api::AgentRunApi;
pub use agent_run_outcome::{AgentRunOutcome, AgentRunStatus};
pub use agent_state::AgentState;
pub use error::{AgentPortError, AgentRunError};
pub use metadata_service::{
    AgentExecutionMetadataService, AuditNodeBuildInput, ExecutionMetadataBuildInput,
};
pub use spawn_agent_request::{
    AgentRunMessageRecordKind, AgentRunRecordKind, SpawnAgentRequest, WorkflowTaskRole,
};
