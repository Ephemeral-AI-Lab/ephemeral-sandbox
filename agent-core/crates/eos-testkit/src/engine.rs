//! Layer-A stepping: pull an engine event stream to a chosen checkpoint.

use eos_engine::{AgentRunStreamEvent, EngineError};
use futures::Stream;
use futures::StreamExt;

/// Pull `stream` forward, collecting each streamed [`AgentRunStreamEvent`] until `stop`
/// returns `true` for one (inclusive) or the stream ends, then return the
/// collected events.
///
/// # Panics
/// Panics if the stream yields an `EngineError` item (a test-harness assertion).
pub async fn run_until<S, F>(stream: &mut S, mut stop: F) -> Vec<AgentRunStreamEvent>
where
    S: Stream<Item = Result<AgentRunStreamEvent, EngineError>> + Unpin,
    F: FnMut(&AgentRunStreamEvent) -> bool,
{
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        let event = item.expect("engine stream item");
        let reached = stop(&event);
        events.push(event);
        if reached {
            break;
        }
    }
    events
}
