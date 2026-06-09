//! File-backed agent-run message records.
//!
//! The message-record root is supplied by the backend composition root, but the
//! message/event contents are started and finished by the agent-run lifecycle
//! owner where request, task, agent-run, and provider-visible message facts are
//! available.

mod error;
mod handle;
mod io;
mod kind;
mod layout;
mod record;
mod service;

pub use error::{MessageRecordError, Result};
pub use handle::{AgentRunRecordHandle, NodeFinishStatus};
pub use kind::{AgentRunRecordKind, AgentRunRecordStart, WorkflowTaskRole};
pub use record::{MessageAppendRange, NodeEvent, RecordBytes};
pub use service::AgentMessageRecords;
