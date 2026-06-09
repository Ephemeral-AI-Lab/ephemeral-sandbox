//! Agent-run record and stream output surfaces.
//!
//! The record root is supplied by the backend composition root, but durable
//! message/event contents are started and finished by the engine loop where
//! provider-visible messages and tool events are observed in order. Live stream
//! observations share this owner because they are emitted by the same run.

mod error;
mod layout;
mod store;
mod stream;

pub use error::{AgentRunRecordError, Result};
pub use store::{
    AgentRunRecordEvent, AgentRunRecordFinishStatus, AgentRunRecordHandle, AgentRunRecordIdentity,
    AgentRunRecordStore, MessageAppendRange, MessageBytes,
};
pub use stream::{
    stamp_identity, AgentRunOutputs, AgentRunStreamEvent, AgentRunStreamSink,
    AgentRunStreamSinkFactory, AssistantMessageComplete,
};
