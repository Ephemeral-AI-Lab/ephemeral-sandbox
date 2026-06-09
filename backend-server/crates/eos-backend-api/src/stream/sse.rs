//! Server-Sent Events transport for the milestone stream.

use std::convert::Infallible;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream;

use eos_backend_types::EventRecord;
use eos_types::RequestId;

use crate::error::ApiError;
use crate::router::AppState;

/// Build the SSE response: replay records after `last_seq`, and map
/// each delivered [`EventRecord`] to an SSE event whose `id` is the sequence,
/// `event` is the milestone kind, and `data` is the JSON payload. A store error
/// mid-stream ends the stream rather than surfacing internals to the client.
pub async fn response(
    state: AppState,
    request_id: RequestId,
    last_seq: i64,
) -> Result<Response, ApiError> {
    let subscription = state.event_bus.subscribe(&request_id, last_seq).await?;
    let events = stream::unfold(subscription, |mut subscription| async move {
        match subscription.recv().await {
            Ok(Some(record)) => Some((Ok::<Event, Infallible>(to_event(&record)), subscription)),
            Ok(None) => None,
            Err(err) => {
                tracing::error!(error = %err, "sse replay refill failed; ending stream");
                None
            }
        }
    });
    Ok(Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// Map one record to an SSE event (id = seq, event = kind, data = JSON payload).
fn to_event(record: &EventRecord) -> Event {
    Event::default()
        .id(record.seq.to_string())
        .event(&record.kind)
        .data(record.payload.to_string())
}
