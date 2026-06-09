//! Agent-run node message-record routes.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Response};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use futures::stream;
use serde::Deserialize;

use eos_engine::records::{AgentRunRecordWriter as AgentMessageRecords, NodeEvent};
use eos_types::{format_record_dir, AgentRunId, AgentRunRecordDir};

use super::parse_id;
use crate::error::ApiError;
use crate::router::AppState;

/// `?after_byte=` query for raw message JSONL tailing.
#[derive(Debug, Deserialize)]
pub struct MessagesQuery {
    after_byte: Option<u64>,
}

/// `?after_seq=` query for event replay.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    after_seq: Option<u64>,
}

/// `?last_seq=` query for SSE replay.
#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    last_seq: Option<u64>,
}

/// `GET /api/agent-core/agent-runs/{agent_run_id}/messages`.
pub async fn messages(
    State(state): State<AppState>,
    Path(agent_run_id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> Result<Response<Body>, ApiError> {
    let agent_run_id: AgentRunId = parse_id(&agent_run_id, "agent run")?;
    let record_dir = record_dir_for_agent_run(&state, &agent_run_id).await?;
    let bytes = state
        .message_records
        .read_messages_at(&record_dir, query.after_byte.unwrap_or(0))
        .await?;
    Response::builder()
        .header("content-type", "application/x-ndjson")
        .header("x-next-byte-offset", bytes.next_byte_offset.to_string())
        .body(Body::from(bytes.bytes))
        .map_err(|err| {
            tracing::error!(error = %err, "failed to build messages response");
            ApiError::Internal
        })
}

/// `GET /api/agent-core/agent-runs/{agent_run_id}/events`.
pub async fn events(
    State(state): State<AppState>,
    Path(agent_run_id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<NodeEvent>>, ApiError> {
    let agent_run_id: AgentRunId = parse_id(&agent_run_id, "agent run")?;
    let record_dir = record_dir_for_agent_run(&state, &agent_run_id).await?;
    Ok(Json(
        state
            .message_records
            .read_events_at(&record_dir, query.after_seq.unwrap_or(0))
            .await?,
    ))
}

/// `GET /api/agent-core/agent-runs/{agent_run_id}/stream`.
pub async fn stream(
    State(state): State<AppState>,
    Path(agent_run_id): Path<String>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let agent_run_id: AgentRunId = parse_id(&agent_run_id, "agent run")?;
    let record_dir = record_dir_for_agent_run(&state, &agent_run_id).await?;
    let last_seq = last_event_id(&headers).or(query.last_seq).unwrap_or(0);
    let initial = state
        .message_records
        .read_events_at(&record_dir, last_seq)
        .await?;
    let tail = TailState::new(
        state.message_records,
        record_dir,
        last_seq,
        VecDeque::from(initial),
    );
    let events = stream::unfold(tail, |mut tail| async move {
        loop {
            if let Some(event) = tail.pending.pop_front() {
                tail.next_seq = tail.next_seq.max(event.seq);
                if event.kind == "node_finished" {
                    tail.finished = true;
                }
                return Some((Ok::<Event, Infallible>(to_sse_event(&event)), tail));
            }
            if tail.finished {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            match tail
                .message_records
                .read_events_at(&tail.record_dir, tail.next_seq)
                .await
            {
                Ok(events) => {
                    tail.pending = VecDeque::from(events);
                }
                Err(err) => {
                    tracing::error!(error = %err, "agent-run SSE message-record tail failed");
                    return None;
                }
            }
        }
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

#[derive(Debug)]
struct TailState {
    message_records: AgentMessageRecords,
    record_dir: AgentRunRecordDir,
    next_seq: u64,
    pending: VecDeque<NodeEvent>,
    finished: bool,
}

impl TailState {
    fn new(
        message_records: AgentMessageRecords,
        record_dir: AgentRunRecordDir,
        last_seq: u64,
        pending: VecDeque<NodeEvent>,
    ) -> Self {
        Self {
            message_records,
            record_dir,
            next_seq: last_seq,
            pending,
            finished: false,
        }
    }
}

fn to_sse_event(event: &NodeEvent) -> Event {
    let payload = serde_json::to_string(&event.payload).unwrap_or_else(|err| {
        tracing::error!(error = %err, "failed to encode SSE event payload");
        "{}".to_owned()
    });
    Event::default()
        .id(event.seq.to_string())
        .event(&event.kind)
        .data(payload)
}

fn last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers.get("last-event-id")?.to_str().ok()?.parse().ok()
}

async fn record_dir_for_agent_run(
    state: &AppState,
    agent_run_id: &AgentRunId,
) -> Result<AgentRunRecordDir, ApiError> {
    let index = state
        .task_agent_run_store
        .record_index_for_agent_run(agent_run_id)
        .await?
        .ok_or(ApiError::NotFound("agent run"))?;
    Ok(format_record_dir(&index))
}
