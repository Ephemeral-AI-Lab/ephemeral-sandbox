//! [`EventBus`] replay/live tests.
#![allow(clippy::unwrap_used)]

use std::collections::VecDeque;
use std::time::Duration;

use tokio::sync::broadcast;

use eos_backend_types::{EventRecord, EVENT_STREAM_GAP};
use eos_engine::StreamEvent;
use eos_types::{RequestId, UtcDateTime};

use crate::test_support::{rid, temp_store};

use super::{milestone_kind, EventBus, EventSubscription};

fn milestone(seq: i64, request: &RequestId, kind: &str) -> EventRecord {
    EventRecord {
        request_id: request.clone(),
        seq,
        kind: kind.to_owned(),
        payload: serde_json::json!({ "type": kind, "seq": seq }),
        created_at: UtcDateTime::now(),
    }
}

fn system_event(text: &str) -> StreamEvent {
    serde_json::from_value(serde_json::json!({
        "type": "system_notification",
        "agent_name": "root",
        "agent_run_id": "run-event-bus",
        "text": text,
    }))
    .unwrap()
}

async fn await_persisted(
    store: &eos_backend_store::BackendStore,
    request: &RequestId,
    target: i64,
) {
    for _ in 0..500 {
        if store
            .event_log()
            .max_seq(request)
            .await
            .unwrap()
            .unwrap_or(0)
            >= target
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("event_log did not reach seq {target} in time");
}

#[test]
fn milestone_kind_drops_deltas_keeps_milestones_and_unknown() {
    for delta in [
        "reasoning_delta",
        "assistant_text_delta",
        "tool_execution_progress",
    ] {
        assert_eq!(milestone_kind(&serde_json::json!({ "type": delta })), None);
    }
    for kind in [
        "assistant_message_complete",
        "tool_execution_started",
        "tool_execution_completed",
        "tool_execution_cancelled",
        "system_notification",
    ] {
        assert_eq!(
            milestone_kind(&serde_json::json!({ "type": kind })),
            Some(kind)
        );
    }
    assert_eq!(
        milestone_kind(&serde_json::json!({ "type": "future_event" })),
        Some("future_event")
    );
    assert_eq!(milestone_kind(&serde_json::json!({ "nope": 1 })), None);
}

#[tokio::test]
async fn registered_sink_persists_before_broadcasting() {
    let (store, _tmp) = temp_store().await;
    let bus = EventBus::with_capacity(store.event_log().clone(), 64, 64);
    let request = rid("req-persist-before-broadcast");
    let sink = bus.register(&request);
    let mut sub = bus.subscribe(&request, 0).await.unwrap();

    sink(&system_event("ready"));

    let record = sub.recv().await.unwrap().unwrap();
    assert_eq!(record.seq, 1);
    let rows = store.event_log().list_since(&request, 0).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].seq, 1);
}

#[tokio::test]
async fn multiple_registered_sinks_share_one_request_sequencer() {
    let (store, _tmp) = temp_store().await;
    let bus = EventBus::with_capacity(store.event_log().clone(), 64, 64);
    let request = rid("req-shared-seq");
    let first = bus.register(&request);
    let second = bus.register(&request);

    first(&system_event("first"));
    second(&system_event("second"));
    await_persisted(&store, &request, 2).await;

    let rows = store.event_log().list_since(&request, 0).await.unwrap();
    let seqs: Vec<i64> = rows.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2]);
}

#[tokio::test]
async fn reconnect_replays_then_joins_live_with_no_gap() {
    let (store, _tmp) = temp_store().await;
    let bus = EventBus::with_capacity(store.event_log().clone(), 64, 64);
    let request = rid("req-reconnect");
    let sink = bus.register(&request);

    sink(&system_event("one"));
    sink(&system_event("two"));
    await_persisted(&store, &request, 2).await;

    let mut sub = bus.subscribe(&request, 0).await.unwrap();
    sink(&system_event("three"));

    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(sub.recv().await.unwrap().unwrap().seq);
    }
    assert_eq!(seen, vec![1, 2, 3]);
}

#[tokio::test]
async fn subscription_recovers_broadcast_lag_from_the_durable_log() {
    let (store, _tmp) = temp_store().await;
    let event_log = store.event_log().clone();
    let request = rid("req-lag");
    for seq in 1..=5 {
        event_log
            .append(&milestone(seq, &request, "tool_execution_completed"))
            .await
            .unwrap();
    }

    let (live, rx) = broadcast::channel::<EventRecord>(2);
    for seq in 1..=5 {
        let _ = live.send(milestone(seq, &request, "tool_execution_completed"));
    }
    drop(live);

    let mut sub = EventSubscription {
        request_id: request.clone(),
        event_log,
        replay: VecDeque::new(),
        live: Some(rx),
        last_seq: 0,
    };
    let mut seen = Vec::new();
    while let Some(record) = sub.recv().await.unwrap() {
        seen.push(record.seq);
    }
    assert_eq!(seen, vec![1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn bounded_queue_overflow_drops_and_emits_a_visible_gap_marker() {
    let (store, _tmp) = temp_store().await;
    let bus = EventBus::with_capacity(store.event_log().clone(), 1, 64);
    let request = rid("req-overflow");
    let sink = bus.register(&request);

    for i in 0..5 {
        sink(&system_event(&format!("event-{i}")));
    }
    drop(sink);

    let mut rows = Vec::new();
    for _ in 0..500 {
        rows = store.event_log().list_since(&request, 0).await.unwrap();
        if rows.iter().any(|r| r.kind == EVENT_STREAM_GAP) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    let gaps = rows.iter().filter(|r| r.kind == EVENT_STREAM_GAP).count();
    let reals = rows.iter().filter(|r| r.kind != EVENT_STREAM_GAP).count();
    assert_eq!(reals, 1);
    assert_eq!(gaps, 1);
    let gap = rows.iter().find(|r| r.kind == EVENT_STREAM_GAP).unwrap();
    assert_eq!(
        gap.payload
            .get("dropped")
            .and_then(serde_json::Value::as_u64),
        Some(4)
    );
}

#[tokio::test]
async fn live_tail_closes_after_last_registered_sink_drops() {
    let (store, _tmp) = temp_store().await;
    let bus = EventBus::with_capacity(store.event_log().clone(), 64, 64);
    let request = rid("req-close");
    let sink = bus.register(&request);
    let mut sub = bus.subscribe(&request, 0).await.unwrap();

    drop(sink);

    let next = tokio::time::timeout(Duration::from_secs(1), sub.recv())
        .await
        .expect("subscription closes")
        .unwrap();
    assert!(next.is_none());
}
