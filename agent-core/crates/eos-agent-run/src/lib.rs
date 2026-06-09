//! Agent-run lifecycle adapter.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod active_agent_runs;
mod cancellation;
mod completion;
mod persistence;
mod service;
mod spawn;

pub use active_agent_runs::ActiveAgentRunRegistry;
pub use eos_types::{
    AgentRunApi, AgentRunError, AgentRunOutcome, AgentRunStatus, SpawnAgentRequest,
    TaskAgentRunKind, WorkflowTaskRole,
};
pub use service::AgentRunService;
