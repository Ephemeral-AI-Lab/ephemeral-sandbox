//! Runtime-owned agent record writer handle.

use eos_engine::records::AgentRunRecordWriter;

/// Optional file-backed agent-node record writer.
#[derive(Clone, Debug, Default)]
pub(crate) struct RecordWriterRuntime {
    pub(crate) run_record_writer: Option<AgentRunRecordWriter>,
}
