//! [`EventBus`] — replay-safe live milestone streaming.
//!
//! The engine observes events through a synchronous callback. The backend callback
//! only serializes/classifies the event and `try_send`s an owned milestone into a
//! bounded queue. A request-scoped async drainer is the sole sequencer: it stamps a
//! monotonic `seq`, persists to `event_log`, then broadcasts to live SSE
//! subscribers. Subscriptions join live before replay, dedup by high-water `seq`,
//! and recover broadcast lag from the durable log.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc};

use eos_backend_store::{EventLogRepo, StoreError};
use eos_backend_types::{EventRecord, EVENT_STREAM_GAP};
use eos_engine::{EngineEventSink, EngineEventSinkFactory, StreamEvent};
use eos_types::{RequestId, UtcDateTime};

const DEFAULT_QUEUE_CAPACITY: usize = 1024;
const DEFAULT_LIVE_CAPACITY: usize = 1024;

#[derive(Debug)]
struct PendingMilestone {
    kind: String,
    payload: serde_json::Value,
    created_at: UtcDateTime,
}

#[derive(Debug)]
struct RequestStream {
    gap_pending: AtomicBool,
    dropped: AtomicU64,
    live: broadcast::Sender<EventRecord>,
}

impl RequestStream {
    fn note_drop(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        self.gap_pending.store(true, Ordering::Release);
    }
}

#[derive(Debug)]
struct RequestStreamHandle {
    tx: mpsc::Sender<PendingMilestone>,
    stream: Arc<RequestStream>,
    active_sinks: AtomicUsize,
}

#[derive(Debug)]
struct SinkGuard {
    request_id: RequestId,
    handle: Arc<RequestStreamHandle>,
    streams: Arc<Mutex<HashMap<RequestId, Arc<RequestStreamHandle>>>>,
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        if self.handle.active_sinks.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        let mut streams = self.streams.lock();
        if streams
            .get(&self.request_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.handle))
        {
            streams.remove(&self.request_id);
        }
    }
}

/// Backend event bus over the durable `event_log` plus request-scoped live tails.
#[derive(Debug)]
pub struct EventBus {
    streams: Arc<Mutex<HashMap<RequestId, Arc<RequestStreamHandle>>>>,
    event_log: EventLogRepo,
    queue_capacity: usize,
    live_capacity: usize,
}

impl EventBus {
    /// Build a bus over the durable `event_log` repository.
    #[must_use]
    pub fn new(event_log: EventLogRepo) -> Self {
        Self::with_capacity(event_log, DEFAULT_QUEUE_CAPACITY, DEFAULT_LIVE_CAPACITY)
    }

    fn with_capacity(event_log: EventLogRepo, queue_capacity: usize, live_capacity: usize) -> Self {
        Self {
            streams: Arc::new(Mutex::new(HashMap::new())),
            event_log,
            queue_capacity: queue_capacity.max(1),
            live_capacity: live_capacity.max(1),
        }
    }

    /// Build the per-loop live sink factory consumed by `TokioAgentLoopLauncher`.
    #[must_use]
    pub fn live_event_sink_factory(self: &Arc<Self>) -> EngineEventSinkFactory {
        let bus = Arc::clone(self);
        Arc::new(move |request| Some(bus.register(&request.record_target.request_id)))
    }

    /// Register one agent loop under its owning request and return its sync event
    /// sink. The returned sink must be dropped when the loop finishes; dropping the
    /// last sink for a request closes its live tail after the drainer flushes.
    #[must_use]
    pub fn register(&self, request_id: &RequestId) -> EngineEventSink {
        let handle = self.stream_for_request(request_id);
        handle.active_sinks.fetch_add(1, Ordering::AcqRel);
        let guard = SinkGuard {
            request_id: request_id.clone(),
            handle: handle.clone(),
            streams: Arc::clone(&self.streams),
        };
        let tx = handle.tx.clone();
        let stream = handle.stream.clone();
        Arc::new(move |event: &StreamEvent| {
            let _keep_alive = &guard;
            let Ok(payload) = serde_json::to_value(event) else {
                return;
            };
            classify_and_enqueue(&tx, &stream, payload);
        })
    }

    fn stream_for_request(&self, request_id: &RequestId) -> Arc<RequestStreamHandle> {
        let mut streams = self.streams.lock();
        if let Some(handle) = streams.get(request_id) {
            return handle.clone();
        }

        let (tx, rx) = mpsc::channel::<PendingMilestone>(self.queue_capacity);
        let (live, _) = broadcast::channel::<EventRecord>(self.live_capacity);
        let stream = Arc::new(RequestStream {
            gap_pending: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            live,
        });
        let handle = Arc::new(RequestStreamHandle {
            tx,
            stream: stream.clone(),
            active_sinks: AtomicUsize::new(0),
        });
        streams.insert(request_id.clone(), handle.clone());
        tokio::spawn(drain(
            rx,
            stream,
            self.event_log.clone(),
            request_id.clone(),
        ));
        handle
    }

    /// Subscribe to a request's stream, replaying persisted rows with
    /// `seq > after_seq` and then tailing the live stream when one is active.
    ///
    /// # Errors
    /// [`StoreError`] if the initial replay read fails.
    pub async fn subscribe(
        &self,
        request_id: &RequestId,
        after_seq: i64,
    ) -> Result<EventSubscription, StoreError> {
        let live = self
            .streams
            .lock()
            .get(request_id)
            .map(|handle| handle.stream.live.subscribe());
        let replay = self.event_log.list_since(request_id, after_seq).await?;
        Ok(EventSubscription {
            request_id: request_id.clone(),
            event_log: self.event_log.clone(),
            replay: replay.into(),
            live,
            last_seq: after_seq,
        })
    }
}

fn classify_and_enqueue(
    tx: &mpsc::Sender<PendingMilestone>,
    stream: &RequestStream,
    payload: serde_json::Value,
) {
    let Some(kind) = milestone_kind(&payload).map(str::to_owned) else {
        return;
    };
    let pending = PendingMilestone {
        kind,
        payload,
        created_at: UtcDateTime::now(),
    };
    if tx.try_send(pending).is_err() {
        stream.note_drop();
    }
}

fn milestone_kind(payload: &serde_json::Value) -> Option<&str> {
    let kind = payload.get("type").and_then(serde_json::Value::as_str)?;
    (!is_delta(kind)).then_some(kind)
}

fn is_delta(kind: &str) -> bool {
    matches!(
        kind,
        "reasoning_delta" | "assistant_text_delta" | "tool_execution_progress"
    )
}

async fn drain(
    mut rx: mpsc::Receiver<PendingMilestone>,
    stream: Arc<RequestStream>,
    event_log: EventLogRepo,
    request_id: RequestId,
) {
    let mut seq = match event_log.max_seq(&request_id).await {
        Ok(Some(seq)) => seq,
        Ok(None) => 0,
        Err(err) => {
            tracing::warn!(error = %err, "failed to read event_log high-water seq");
            0
        }
    };
    while let Some(pending) = rx.recv().await {
        seq = persist_and_broadcast(&event_log, &stream, &request_id, seq, pending).await;
        if stream.gap_pending.swap(false, Ordering::AcqRel) {
            seq = emit_gap(&event_log, &stream, &request_id, seq).await;
        }
    }
    if stream.gap_pending.swap(false, Ordering::AcqRel) {
        emit_gap(&event_log, &stream, &request_id, seq).await;
    }
}

async fn persist_and_broadcast(
    event_log: &EventLogRepo,
    stream: &RequestStream,
    request_id: &RequestId,
    seq: i64,
    pending: PendingMilestone,
) -> i64 {
    let record = EventRecord {
        request_id: request_id.clone(),
        seq: seq + 1,
        kind: pending.kind,
        payload: pending.payload,
        created_at: pending.created_at,
    };
    match event_log.append(&record).await {
        Ok(()) => {
            let stamped = record.seq;
            let _ = stream.live.send(record);
            stamped
        }
        Err(err) => {
            tracing::warn!(
                seq = record.seq,
                error = %err,
                "event_log append failed; dropping record and marking a stream gap"
            );
            stream.note_drop();
            seq
        }
    }
}

async fn emit_gap(
    event_log: &EventLogRepo,
    stream: &RequestStream,
    request_id: &RequestId,
    seq: i64,
) -> i64 {
    let gap = EventRecord {
        request_id: request_id.clone(),
        seq: seq + 1,
        kind: EVENT_STREAM_GAP.to_owned(),
        payload: serde_json::json!({ "dropped": stream.dropped.load(Ordering::Relaxed) }),
        created_at: UtcDateTime::now(),
    };
    match event_log.append(&gap).await {
        Ok(()) => {
            let _ = stream.live.send(gap);
            seq + 1
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to persist event_stream_gap marker");
            seq
        }
    }
}

/// Replay-then-live subscription for one request.
#[derive(Debug)]
pub struct EventSubscription {
    request_id: RequestId,
    event_log: EventLogRepo,
    replay: VecDeque<EventRecord>,
    live: Option<broadcast::Receiver<EventRecord>>,
    /// Highest `seq` delivered so far.
    last_seq: i64,
}

impl EventSubscription {
    /// Return the next event record, recovering live broadcast lag from
    /// `event_log`.
    ///
    /// # Errors
    /// [`StoreError`] if a durable refill read fails.
    pub async fn recv(&mut self) -> Result<Option<EventRecord>, StoreError> {
        loop {
            if let Some(record) = self.replay.pop_front() {
                if record.seq > self.last_seq {
                    self.last_seq = record.seq;
                    return Ok(Some(record));
                }
                continue;
            }

            let Some(live) = self.live.as_mut() else {
                return Ok(None);
            };
            match live.recv().await {
                Ok(record) => {
                    if record.seq > self.last_seq {
                        self.last_seq = record.seq;
                        return Ok(Some(record));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let refill = self
                        .event_log
                        .list_since(&self.request_id, self.last_seq)
                        .await?;
                    self.replay.extend(refill);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    let refill = self
                        .event_log
                        .list_since(&self.request_id, self.last_seq)
                        .await?;
                    if refill.is_empty() {
                        self.live = None;
                        return Ok(None);
                    }
                    self.replay.extend(refill);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/event_bus/mod.rs"]
mod tests;
