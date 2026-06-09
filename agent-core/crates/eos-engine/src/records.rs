//! File-backed agent-run message records.
//!
//! The message-record root is supplied by the backend composition root, but the
//! message/event contents are started and finished by the engine loop where
//! provider-visible messages and tool events are observed in order.

mod error;
mod handle;
mod io;
mod kind;
mod layout;
mod record;
mod writer;

pub use eos_types::WorkflowTaskRole;
pub use error::{AgentRunRecordError, Result};
pub use handle::{AgentRunRecordHandle, NodeFinishStatus};
pub use kind::{AgentRunRecordKind, AgentRunRecordStart};
pub use record::{MessageAppendRange, NodeEvent, RecordBytes};
pub use writer::AgentRunRecordWriter;
