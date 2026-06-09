//! Engine event output fan-out.

use crate::records::AgentRunRecordWriter;

use super::event::StreamEvent;
use super::printer::EngineEventPrinter;
use super::sink::EngineEventSink;

/// Output aggregate for live observations, printing, and durable run records.
#[derive(Clone, Default)]
pub struct EngineEventOutputs {
    live_event_sink: Option<EngineEventSink>,
    event_printer: Option<EngineEventPrinter>,
    run_record_writer: Option<AgentRunRecordWriter>,
}

impl std::fmt::Debug for EngineEventOutputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineEventOutputs")
            .field("has_live_event_sink", &self.live_event_sink.is_some())
            .field("has_event_printer", &self.event_printer.is_some())
            .field("has_run_record_writer", &self.run_record_writer.is_some())
            .finish()
    }
}

impl EngineEventOutputs {
    /// Create an empty output aggregate.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach live event observation.
    #[must_use]
    pub fn with_live_event_sink(mut self, live_event_sink: Option<EngineEventSink>) -> Self {
        self.live_event_sink = live_event_sink;
        self
    }

    /// Attach human-readable event printing.
    #[must_use]
    pub fn with_event_printer(mut self, event_printer: Option<EngineEventPrinter>) -> Self {
        self.event_printer = event_printer;
        self
    }

    /// Attach durable agent-run record writing.
    #[must_use]
    pub fn with_run_record_writer(
        mut self,
        run_record_writer: Option<AgentRunRecordWriter>,
    ) -> Self {
        self.run_record_writer = run_record_writer;
        self
    }

    pub(crate) fn observe(&self, event: &StreamEvent) {
        if let Some(live_event_sink) = &self.live_event_sink {
            live_event_sink(event);
        }
        if let Some(event_printer) = &self.event_printer {
            event_printer.print(event);
        }
    }

    pub(crate) fn run_record_writer(&self) -> Option<&AgentRunRecordWriter> {
        self.run_record_writer.as_ref()
    }
}
