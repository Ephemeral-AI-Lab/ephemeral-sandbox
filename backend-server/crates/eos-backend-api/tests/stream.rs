//! Stream replay tests for both transports. The replay/live handoff correctness
//! is proven in `eos-backend-runtime`; here we prove the API layer forwards
//! persisted `event_log` rows from `last_seq`, over SSE and over a real
//! WebSocket. Events are seeded directly through the public `EventLogRepo`
//! (the bus's `register` is crate-private), exercising the replay-only path.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

use eos_backend_runtime::CancelOutcome;
use eos_backend_store::BackendStore;
use eos_backend_types::{BackendRunStatus, EventRecord, RunMeta, EVENT_STREAM_GAP};
use eos_types::{RequestId, UtcDateTime};

use support::{fake_reads, router, test_store, FakeRunControl, FakeSandboxRegistry};

async fn seed_run(store: &BackendStore, id: &RequestId) {
    store
        .run_meta()
        .insert(&RunMeta {
            request_id: id.clone(),
            status: BackendRunStatus::Done,
            label: None,
            client_meta: json!({}),
            created_at: UtcDateTime::now(),
            finished_at: Some(UtcDateTime::now()),
            cancel_reason: None,
        })
        .await
        .expect("seed run_meta");
}

/// Seed `count` milestone events (seq 1..=count) into `event_log`.
async fn seed_events(store: &BackendStore, id: &RequestId, count: i64) {
    for seq in 1..=count {
        store
            .event_log()
            .append(&EventRecord {
                request_id: id.clone(),
                seq,
                kind: "run_progress".to_owned(),
                payload: json!({ "n": seq }),
                created_at: UtcDateTime::now(),
            })
            .await
            .expect("seed event");
    }
}

fn app(store: &BackendStore) -> axum::Router {
    router(
        store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    )
}

#[tokio::test]
async fn sse_replays_persisted_events_in_order() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    seed_run(&store, &id).await;
    seed_events(&store, &id, 3).await;
    let app = app(&store);

    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/user-requests/{}/stream", id.as_str()))
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");

    // All three payloads replayed, in sequence order.
    let p1 = text.find(r#"{"n":1}"#).expect("event 1");
    let p2 = text.find(r#"{"n":2}"#).expect("event 2");
    let p3 = text.find(r#"{"n":3}"#).expect("event 3");
    assert!(p1 < p2 && p2 < p3, "events out of order: {text}");
}

#[tokio::test]
async fn sse_replay_resumes_after_last_seq() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    seed_run(&store, &id).await;
    seed_events(&store, &id, 3).await;
    let app = app(&store);

    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/user-requests/{}/stream?last_seq=2", id.as_str()))
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");

    assert!(!text.contains(r#"{"n":1}"#), "replayed an event before last_seq");
    assert!(!text.contains(r#"{"n":2}"#), "replayed last_seq itself");
    assert!(text.contains(r#"{"n":3}"#), "missed the event after last_seq");
}

#[tokio::test]
async fn stream_unknown_request_is_404() {
    let (store, _dir) = test_store().await;
    let app = app(&store);
    let request = Request::builder()
        .method("GET")
        .uri("/api/user-requests/missing/stream")
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn websocket_replays_persisted_events() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    seed_run(&store, &id).await;
    seed_events(&store, &id, 3).await;
    let app = app(&store);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });

    let url = format!(
        "ws://{addr}/api/user-requests/{}/stream?last_seq=0",
        id.as_str()
    );
    let (mut socket, _resp) = tokio_tungstenite::connect_async(url)
        .await
        .expect("ws connect");

    let mut seqs = Vec::new();
    while let Some(message) = socket.next().await {
        match message.expect("ws frame") {
            Message::Text(text) => {
                let record: Value = serde_json::from_str(&text).expect("event json");
                seqs.push(record["seq"].as_i64().expect("seq"));
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    let _ = socket.close(None).await;
    server.abort();

    assert_eq!(seqs, vec![1, 2, 3], "websocket replay sequence mismatch");
}

/// A persisted `event_stream_gap` marker (dropped-milestone loss) must replay to
/// the live stream, so milestone loss stays visible to clients and is never
/// silent (SPEC §Event Stream overflow policy). The handler does not filter the
/// gap kind out of the replay.
#[tokio::test]
async fn sse_replays_event_stream_gap_marker() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    seed_run(&store, &id).await;
    // One real milestone, then a gap marker recording dropped milestones.
    seed_events(&store, &id, 1).await;
    store
        .event_log()
        .append(&EventRecord {
            request_id: id.clone(),
            seq: 2,
            kind: EVENT_STREAM_GAP.to_owned(),
            payload: json!({ "dropped": 3 }),
            created_at: UtcDateTime::now(),
        })
        .await
        .expect("seed gap marker");
    let app = app(&store);

    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/user-requests/{}/stream", id.as_str()))
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");

    // The gap marker rides the stream as its own milestone kind (loss is visible).
    assert!(text.contains(EVENT_STREAM_GAP), "gap marker not replayed: {text}");
    assert!(text.contains(r#"{"dropped":3}"#), "gap payload missing: {text}");
}
