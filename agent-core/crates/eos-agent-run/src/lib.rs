//! Agent-run lifecycle adapter.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod active_agent_runs;
mod agent_loop_request;
mod agent_run_persistence;
mod agent_run_records;
mod agent_run_service;

pub use active_agent_runs::ActiveAgentRuns;
pub use agent_run_records::to_message_record_kind;
pub use agent_run_service::AgentRunService;
pub use eos_engine::records::{
    AgentMessageRecords, AgentRunRecordHandle, AgentRunRecordStart, MessageRecordError,
    NodeFinishStatus,
};
pub use eos_types::{
    AgentRunApi, AgentRunError, AgentRunMessageRecordKind, AgentRunOutcome, AgentRunStatus,
    SpawnAgentRequest, WorkflowTaskRole,
};
