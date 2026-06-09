//! Engine event data, observation, and rendering.

// Phase 04 intentionally keeps event data at `event/event.rs` so data,
// observation, and rendering stay as sibling files under the event owner.
#[allow(clippy::module_inception)]
mod event;
mod outputs;
mod printer;
mod sink;

pub use event::{stamp_identity, AssistantMessageComplete, StreamEvent};
pub use outputs::EngineEventOutputs;
pub use printer::EngineEventPrinter;
pub use sink::EngineEventSink;
