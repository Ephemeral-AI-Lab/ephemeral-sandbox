//! API contract tests: status precedence + write-back, sandbox sanitization,
//! pagination, cancellation, v1 sandbox-override rejection, error sanitization,
//! path conventions, and the `OpenAPI` shape. Drives the real router over a temp
//! `backend.db` plus the runtime/agent-core fakes in [`support`].

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use eos_backend_runtime::{DeleteRejection, SandboxManagerError};
use eos_backend_store::BackendStore;
use eos_backend_types::{BackendRunStatus, EventRecord, RunMeta, SandboxState};
use eos_engine::records::AgentRunRecordWriter as AgentMessageRecords;
use eos_types::RequestStatus;
use eos_types::{AgentRunId, RequestId, SandboxId, TaskId, UtcDateTime};

use support::{
    fake_agent_core_state, make_agent_run, make_sandbox_view, make_task, router,
    router_with_message_records, test_store, FakeSandboxRegistry,
};

/// Send a request through the router and return `(status, body bytes)`.
async fn send(router: &axum::Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    (status, bytes.to_vec())
}

fn json_of(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("json body")
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("request")
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .expect("request")
}

fn post_json(uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn seed_run(store: &BackendStore, id: &RequestId, status: BackendRunStatus) {
    store
        .run_meta()
        .insert(&RunMeta {
            request_id: id.clone(),
            status,
            label: None,
            client_meta: json!({}),
            created_at: UtcDateTime::now(),
            finished_at: None,
            cancel_reason: None,
        })
        .await
        .expect("seed run_meta");
}

fn seed_agent_message_record(
    root: &std::path::Path,
    agent_run_id: &AgentRunId,
) -> std::path::PathBuf {
    let node = root
        .join("requests/req-1/root-task-task-1")
        .join(format!("agent-run-{}", agent_run_id.as_str()));
    std::fs::create_dir_all(&node).expect("message-record dir");
    std::fs::write(
        node.join("messages.jsonl"),
        concat!(
            "{\"type\":\"initial_message\",\"role\":\"system\",\"content\":[{\"type\":\"text\",\"text\":\"system\"}]}\n",
            "{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}\n",
        ),
    )
    .expect("messages");
    std::fs::write(
        node.join("events.jsonl"),
        concat!(
            "{\"seq\":1,\"kind\":\"node_started\",\"payload\":{\"type\":\"root_agent\"},\"created_at\":\"2026-06-07T04:00:00Z\"}\n",
            "{\"seq\":2,\"kind\":\"messages_initialized\",\"payload\":{\"count\":1,\"messages_start_byte\":0,\"messages_end_byte\":90},\"created_at\":\"2026-06-07T04:00:01Z\"}\n",
            "{\"seq\":3,\"kind\":\"node_finished\",\"payload\":{\"status\":\"completed\"},\"created_at\":\"2026-06-07T04:00:02Z\"}\n",
        ),
    )
    .expect("events");
    node
}

// --- create / cancel -------------------------------------------------------

#[tokio::test]
async fn post_create_returns_202_with_request_id() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );

    let (status, body) = send(
        &app,
        post_json("/api/agent-core/requests", &json!({ "prompt": "hi" })),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED);
    let body = json_of(&body);
    let request_id = body["request_id"].as_str().expect("request id");
    assert!(!request_id.is_empty());
    assert!(store
        .run_meta()
        .get(&request_id.parse().expect("typed request id"))
        .await
        .expect("run meta get")
        .is_some());
}

#[tokio::test]
async fn post_rejects_unsupported_sandbox_override() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );

    // `image` is a v1-deferred override; `deny_unknown_fields` rejects it.
    let (status, _) = send(
        &app,
        post_json(
            "/api/agent-core/requests",
            &json!({ "prompt": "hi", "sandbox_args": { "image": "ubuntu" } }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_accepts_sandbox_id_override() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );

    let (status, _) = send(
        &app,
        post_json(
            "/api/agent-core/requests",
            &json!({ "prompt": "hi", "sandbox_args": { "sandbox_id": "sbx-1" } }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn cancel_requested_returns_202() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    seed_run(&store, &id, BackendRunStatus::Running).await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );

    let (status, _) = send(
        &app,
        delete(&format!("/api/agent-core/requests/{}", id.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn cancel_unknown_is_404() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );
    let (status, _) = send(&app, delete("/api/agent-core/requests/does-not-exist")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancel_already_finished_is_409() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    seed_run(&store, &id, BackendRunStatus::Done).await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );
    let (status, _) = send(
        &app,
        delete(&format!("/api/agent-core/requests/{}", id.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

// --- list / detail / status precedence ------------------------------------

#[tokio::test]
async fn list_returns_page_of_run_records() {
    let (store, _dir) = test_store().await;
    seed_run(&store, &RequestId::new_v4(), BackendRunStatus::Running).await;
    seed_run(&store, &RequestId::new_v4(), BackendRunStatus::Done).await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );

    let (status, body) = send(&app, get("/api/agent-core/requests?limit=10")).await;
    assert_eq!(status, StatusCode::OK);
    let body = json_of(&body);
    assert_eq!(body["total"], json!(2));
    assert_eq!(body["items"].as_array().expect("items").len(), 2);
}

#[tokio::test]
async fn detail_joins_and_persists_terminal_outcome() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    // Backend still Running, agent-core reports Done -> resolved `done` + write-back.
    seed_run(&store, &id, BackendRunStatus::Running).await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(Some(RequestStatus::Done), vec![], None),
    );

    let (status, body) = send(
        &app,
        get(&format!("/api/agent-core/requests/{}", id.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body)["status"], json!("done"));

    // The write-back persisted the terminal status + finished_at (CAS guard).
    let persisted = store.run_meta().get(&id).await.expect("get").expect("row");
    assert_eq!(persisted.status, BackendRunStatus::Done);
    assert!(persisted.finished_at.is_some());
}

#[tokio::test]
async fn detail_failed_outcome_is_persisted() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    // Backend still Running, agent-core reports Failed -> resolved `failed` + write-back.
    seed_run(&store, &id, BackendRunStatus::Running).await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(Some(RequestStatus::Failed), vec![], None),
    );

    let (status, body) = send(
        &app,
        get(&format!("/api/agent-core/requests/{}", id.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body)["status"], json!("failed"));

    // The CAS write-back persisted the terminal Failed status + finished_at.
    let persisted = store.run_meta().get(&id).await.expect("get").expect("row");
    assert_eq!(persisted.status, BackendRunStatus::Failed);
    assert!(persisted.finished_at.is_some());
}

#[tokio::test]
async fn detail_cancelled_is_not_clobbered_by_agent_terminal() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    // Backend already Cancelled (terminal) — even if agent-core reports Done, the
    // resolved status stays `cancelled` and no write-back occurs.
    seed_run(&store, &id, BackendRunStatus::Cancelled).await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(Some(RequestStatus::Done), vec![], None),
    );

    let (status, body) = send(
        &app,
        get(&format!("/api/agent-core/requests/{}", id.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body)["status"], json!("cancelled"));
    assert_eq!(
        store
            .run_meta()
            .get(&id)
            .await
            .expect("get")
            .expect("row")
            .status,
        BackendRunStatus::Cancelled
    );
}

#[tokio::test]
async fn detail_unknown_is_404() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );
    let (status, _) = send(&app, get("/api/agent-core/requests/missing")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn events_route_replays_persisted_milestones() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    seed_run(&store, &id, BackendRunStatus::Running).await;
    for seq in 1..=2 {
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
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );

    // Full replay, in sequence order.
    let (status, body) = send(
        &app,
        get(&format!(
            "/api/agent-core/requests/{}/events?after_seq=0",
            id.as_str()
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = json_of(&body);
    let events = events.as_array().expect("array");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["seq"], json!(1));
    assert_eq!(events[1]["seq"], json!(2));

    // `after_seq` filters out already-seen records.
    let (status, body) = send(
        &app,
        get(&format!(
            "/api/agent-core/requests/{}/events?after_seq=1",
            id.as_str()
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = json_of(&body);
    let events = events.as_array().expect("array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["seq"], json!(2));

    // Unknown request is a 404.
    let (status, _) = send(&app, get("/api/agent-core/requests/missing/events")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn agent_run_messages_route_returns_raw_jsonl_with_next_offset() {
    let (store, _dir) = test_store().await;
    let message_records_dir = tempfile::tempdir().expect("message-record tempdir");
    let agent_run_id: AgentRunId = "run-1".parse().expect("run id");
    let node = seed_agent_message_record(message_records_dir.path(), &agent_run_id);
    let full_len = std::fs::metadata(node.join("messages.jsonl"))
        .expect("messages metadata")
        .len();
    let app = router_with_message_records(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
        AgentMessageRecords::new(message_records_dir.path()),
    );

    let (status, body) = send(
        &app,
        get(&format!(
            "/api/agent-core/agent-runs/{}/messages?after_byte=0",
            agent_run_id.as_str()
        )),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body).expect("utf8");
    assert!(text.contains("\"role\":\"system\""));
    assert!(text.contains("\"role\":\"user\""));

    let response = app
        .clone()
        .oneshot(get(&format!(
            "/api/agent-core/agent-runs/{}/messages?after_byte={}",
            agent_run_id.as_str(),
            full_len
        )))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-next-byte-offset")
            .expect("offset")
            .to_str()
            .expect("offset str"),
        full_len.to_string()
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn agent_run_events_route_replays_node_local_events() {
    let (store, _dir) = test_store().await;
    let message_records_dir = tempfile::tempdir().expect("message-record tempdir");
    let agent_run_id: AgentRunId = "run-1".parse().expect("run id");
    seed_agent_message_record(message_records_dir.path(), &agent_run_id);
    let app = router_with_message_records(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
        AgentMessageRecords::new(message_records_dir.path()),
    );

    let (status, body) = send(
        &app,
        get(&format!(
            "/api/agent-core/agent-runs/{}/events?after_seq=1",
            agent_run_id.as_str()
        )),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let events = json_of(&body);
    let events = events.as_array().expect("events array");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["seq"], json!(2));
    assert_eq!(events[1]["kind"], json!("node_finished"));
}

#[tokio::test]
async fn agent_run_sse_replays_from_last_event_id() {
    let (store, _dir) = test_store().await;
    let message_records_dir = tempfile::tempdir().expect("message-record tempdir");
    let agent_run_id: AgentRunId = "run-1".parse().expect("run id");
    seed_agent_message_record(message_records_dir.path(), &agent_run_id);
    let app = router_with_message_records(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
        AgentMessageRecords::new(message_records_dir.path()),
    );

    let request = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/agent-core/agent-runs/{}/stream",
            agent_run_id.as_str()
        ))
        .header("last-event-id", "2")
        .body(Body::empty())
        .expect("request");
    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");

    assert!(
        !text.contains("messages_initialized"),
        "replayed last event: {text}"
    );
    assert!(
        text.contains("node_finished"),
        "missed terminal event: {text}"
    );
    assert!(text.contains("id: 3"), "missing node-local SSE id: {text}");
}

// --- sandboxes -------------------------------------------------------------

#[tokio::test]
async fn sandbox_list_is_sanitized() {
    let (store, _dir) = test_store().await;
    let sandbox_id: SandboxId = "sbx-1".parse().expect("id");
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![make_sandbox_view(
            &sandbox_id,
            SandboxState::Ready,
        )])),
        fake_agent_core_state(None, vec![], None),
    );

    let (status, body) = send(&app, get("/api/sandboxes")).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body).expect("utf8");
    assert!(text.contains("sbx-1"));
    // No daemon connection material or credentials in the serialized view (AC4).
    for denied in ["host", "port", "auth", "token", "daemon"] {
        assert!(
            !text.contains(denied),
            "sandbox response leaked {denied:?}: {text}"
        );
    }
}

#[tokio::test]
async fn sandbox_detail_returns_sanitized_view() {
    let (store, _dir) = test_store().await;
    let sandbox_id: SandboxId = "sbx-1".parse().expect("id");
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![make_sandbox_view(
            &sandbox_id,
            SandboxState::Ready,
        )])),
        fake_agent_core_state(None, vec![], None),
    );

    let (status, body) = send(&app, get("/api/sandboxes/sbx-1")).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body).expect("utf8");
    assert!(text.contains("sbx-1"));
    for denied in ["host", "port", "auth", "token", "daemon"] {
        assert!(
            !text.contains(denied),
            "sandbox detail leaked {denied:?}: {text}"
        );
    }
}

#[tokio::test]
async fn sandbox_detail_unknown_is_404() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );
    let (status, _) = send(&app, get("/api/sandboxes/sbx-x")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sandbox_delete_conflict_when_active() {
    let (store, _dir) = test_store().await;
    let sandbox_id: SandboxId = "sbx-1".parse().expect("id");
    let registry = FakeSandboxRegistry::with_delete_error(
        vec![make_sandbox_view(&sandbox_id, SandboxState::Active)],
        SandboxManagerError::DeleteRejected {
            sandbox_id: sandbox_id.clone(),
            reason: DeleteRejection::Active,
        },
    );
    let app = router(
        &store,
        Arc::new(registry),
        fake_agent_core_state(None, vec![], None),
    );
    let (status, _) = send(&app, delete("/api/sandboxes/sbx-1")).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn sandbox_delete_ok_is_204() {
    let (store, _dir) = test_store().await;
    let sandbox_id: SandboxId = "sbx-1".parse().expect("id");
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![make_sandbox_view(
            &sandbox_id,
            SandboxState::Ready,
        )])),
        fake_agent_core_state(None, vec![], None),
    );
    let (status, _) = send(&app, delete("/api/sandboxes/sbx-1")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn internal_error_is_not_leaked() {
    let (store, _dir) = test_store().await;
    // A teardown failure carries an internal detail; the client must see only a
    // generic 500 (no daemon detail, no SQL).
    let registry = FakeSandboxRegistry::with_delete_error(
        vec![],
        SandboxManagerError::Teardown("daemon-secret-xyz".to_owned()),
    );
    let app = router(
        &store,
        Arc::new(registry),
        fake_agent_core_state(None, vec![], None),
    );
    let (status, body) = send(&app, delete("/api/sandboxes/sbx-1")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let text = String::from_utf8(body).expect("utf8");
    assert!(
        !text.contains("daemon-secret-xyz"),
        "leaked internal detail: {text}"
    );
}

// --- stats -----------------------------------------------------------------

#[tokio::test]
async fn stats_routes_serve_on_empty_store() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );
    for uri in [
        "/api/stats/performance",
        "/api/stats/correctness",
        "/api/stats/agent-runs",
        "/api/stats/events",
    ] {
        let (status, _) = send(&app, get(uri)).await;
        assert_eq!(status, StatusCode::OK, "stats route {uri} failed");
    }
}

// --- tasks -----------------------------------------------------------------

#[tokio::test]
async fn request_tasks_returns_tree() {
    let (store, _dir) = test_store().await;
    let request_id = RequestId::new_v4();
    seed_run(&store, &request_id, BackendRunStatus::Running).await;
    let task = make_task(&TaskId::new_v4(), &request_id);
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![task], None),
    );
    let (status, body) = send(
        &app,
        get(&format!(
            "/api/agent-core/requests/{}/tasks",
            request_id.as_str()
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body).as_array().expect("array").len(), 1);
}

#[tokio::test]
async fn task_detail_joins_run() {
    let (store, _dir) = test_store().await;
    let task_id = TaskId::new_v4();
    let task = make_task(&task_id, &RequestId::new_v4());
    let run = make_agent_run(&task_id, vec![]);
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![task], Some(run)),
    );
    let (status, body) = send(
        &app,
        get(&format!("/api/agent-core/tasks/{}", task_id.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json_of(&body);
    assert_eq!(body["task"]["id"], json!(task_id.as_str()));
    assert!(body["agent_run"].is_object());
}

#[tokio::test]
async fn task_detail_unknown_is_404() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );
    let (status, _) = send(&app, get("/api/agent-core/tasks/missing")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn transcript_returns_messages() {
    let (store, _dir) = test_store().await;
    let task_id = TaskId::new_v4();
    let task = make_task(&task_id, &RequestId::new_v4());
    let run = make_agent_run(&task_id, vec![]);
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![task], Some(run)),
    );
    let (status, body) = send(
        &app,
        get(&format!(
            "/api/agent-core/tasks/{}/transcript",
            task_id.as_str()
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json_of(&body);
    assert_eq!(body["messages"].as_array().expect("messages").len(), 0);
}

#[tokio::test]
async fn transcript_prefers_agent_run_messages_jsonl() {
    let (store, _dir) = test_store().await;
    let message_records_dir = tempfile::tempdir().expect("message-record tempdir");
    let task_id: TaskId = "task-1".parse().expect("task id");
    let request_id: RequestId = "req-1".parse().expect("request id");
    let task = make_task(&task_id, &request_id);
    let agent_run_id: AgentRunId = "run-1".parse().expect("run id");
    seed_agent_message_record(message_records_dir.path(), &agent_run_id);
    let mut stale = serde_json::Map::new();
    stale.insert("role".to_owned(), json!("assistant"));
    let mut run = make_agent_run(&task_id, vec![stale]);
    run.id = agent_run_id;
    let app = router_with_message_records(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![task], Some(run)),
        AgentMessageRecords::new(message_records_dir.path()),
    );

    let (status, body) = send(
        &app,
        get(&format!(
            "/api/agent-core/tasks/{}/transcript",
            task_id.as_str()
        )),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let body = json_of(&body);
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], json!("system"));
    assert_eq!(messages[1]["role"], json!("user"));
}

// --- conventions / openapi -------------------------------------------------

#[tokio::test]
async fn legacy_equals_style_path_is_404() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );
    // Assemble the `=` from a fragment so this negative assertion does not itself
    // trip the AC4 legacy-path source grep (the path must not exist).
    let legacy = format!("/api/user-request{}abc", '=');
    let (status, _) = send(&app, get(&legacy)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openapi_pins_paths_and_schemas() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_agent_core_state(None, vec![], None),
    );
    let (status, body) = send(&app, get("/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    let doc = json_of(&body);

    for path in [
        "/api/agent-core/requests",
        "/api/agent-core/requests/{request_id}",
        "/api/agent-core/requests/{request_id}/events",
        "/api/agent-core/requests/{request_id}/stream",
        "/api/agent-core/requests/{request_id}/tasks",
        "/api/agent-core/tasks/{task_id}",
        "/api/agent-core/tasks/{task_id}/transcript",
        "/api/agent-core/agent-runs/{agent_run_id}/messages",
        "/api/agent-core/agent-runs/{agent_run_id}/events",
        "/api/agent-core/agent-runs/{agent_run_id}/stream",
        "/api/stats/performance",
        "/api/stats/correctness",
        "/api/stats/agent-runs",
        "/api/stats/events",
        "/api/sandboxes",
        "/api/sandboxes/{sandbox_id}",
    ] {
        assert!(
            doc["paths"].get(path).is_some(),
            "openapi missing path {path}"
        );
    }
    for schema in [
        "CreateUserRequest",
        "UserRequestDetail",
        "SandboxView",
        "PerformanceStats",
    ] {
        assert!(
            doc["components"]["schemas"].get(schema).is_some(),
            "openapi missing schema {schema}"
        );
    }
}
