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

use eos_backend_runtime::{CancelOutcome, DeleteRejection, SandboxManagerError};
use eos_backend_store::BackendStore;
use eos_backend_types::{BackendRunStatus, RunMeta, SandboxState};
use eos_state::RequestStatus;
use eos_types::{RequestId, SandboxId, TaskId, UtcDateTime};

use support::{
    fake_reads, make_agent_run, make_sandbox_view, make_task, router, test_store, FakeRunControl,
    FakeSandboxRegistry,
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

fn post_json(uri: &str, body: Value) -> Request<Body> {
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

// --- create / cancel -------------------------------------------------------

#[tokio::test]
async fn post_create_returns_202_with_request_id() {
    let (store, _dir) = test_store().await;
    let runs = Arc::new(FakeRunControl::new(CancelOutcome::Requested));
    let expected = runs.launch_id.clone();
    let app = router(
        &store,
        runs.clone(),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );

    let (status, body) = send(&app, post_json("/api/user-requests", json!({ "prompt": "hi" }))).await;

    assert_eq!(status, StatusCode::ACCEPTED);
    let body = json_of(&body);
    assert_eq!(body["request_id"], json!(expected.as_str()));
    assert_eq!(runs.launched.lock().expect("poisoned").len(), 1);
}

#[tokio::test]
async fn post_rejects_unsupported_sandbox_override() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );

    // `image` is a v1-deferred override; `deny_unknown_fields` rejects it.
    let (status, _) = send(
        &app,
        post_json(
            "/api/user-requests",
            json!({ "prompt": "hi", "sandbox_args": { "image": "ubuntu" } }),
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );

    let (status, _) = send(
        &app,
        post_json(
            "/api/user-requests",
            json!({ "prompt": "hi", "sandbox_args": { "sandbox_id": "sbx-1" } }),
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );

    let (status, _) = send(&app, delete(&format!("/api/user-requests/{}", id.as_str()))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn cancel_unknown_is_404() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );
    let (status, _) = send(&app, delete("/api/user-requests/does-not-exist")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancel_already_finished_is_409() {
    let (store, _dir) = test_store().await;
    let id = RequestId::new_v4();
    seed_run(&store, &id, BackendRunStatus::Done).await;
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::NotFound)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );
    let (status, _) = send(&app, delete(&format!("/api/user-requests/{}", id.as_str()))).await;
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );

    let (status, body) = send(&app, get("/api/user-requests?limit=10")).await;
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(Some(RequestStatus::Done), vec![], None),
    );

    let (status, body) = send(&app, get(&format!("/api/user-requests/{}", id.as_str()))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body)["status"], json!("done"));

    // The write-back persisted the terminal status + finished_at (CAS guard).
    let persisted = store.run_meta().get(&id).await.expect("get").expect("row");
    assert_eq!(persisted.status, BackendRunStatus::Done);
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(Some(RequestStatus::Done), vec![], None),
    );

    let (status, body) = send(&app, get(&format!("/api/user-requests/{}", id.as_str()))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_of(&body)["status"], json!("cancelled"));
    assert_eq!(
        store.run_meta().get(&id).await.expect("get").expect("row").status,
        BackendRunStatus::Cancelled
    );
}

#[tokio::test]
async fn detail_unknown_is_404() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );
    let (status, _) = send(&app, get("/api/user-requests/missing")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// --- sandboxes -------------------------------------------------------------

#[tokio::test]
async fn sandbox_list_is_sanitized() {
    let (store, _dir) = test_store().await;
    let sandbox_id: SandboxId = "sbx-1".parse().expect("id");
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![make_sandbox_view(
            &sandbox_id,
            SandboxState::Ready,
        )])),
        fake_reads(None, vec![], None),
    );

    let (status, body) = send(&app, get("/api/sandboxes")).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8(body).expect("utf8");
    assert!(text.contains("sbx-1"));
    // No daemon connection material or credentials in the serialized view (AC4).
    for denied in ["host", "port", "auth", "token", "daemon"] {
        assert!(!text.contains(denied), "sandbox response leaked {denied:?}: {text}");
    }
}

#[tokio::test]
async fn sandbox_detail_unknown_is_404() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(registry),
        fake_reads(None, vec![], None),
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![make_sandbox_view(
            &sandbox_id,
            SandboxState::Ready,
        )])),
        fake_reads(None, vec![], None),
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(registry),
        fake_reads(None, vec![], None),
    );
    let (status, body) = send(&app, delete("/api/sandboxes/sbx-1")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let text = String::from_utf8(body).expect("utf8");
    assert!(!text.contains("daemon-secret-xyz"), "leaked internal detail: {text}");
}

// --- stats -----------------------------------------------------------------

#[tokio::test]
async fn stats_routes_serve_on_empty_store() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![task], None),
    );
    let (status, body) = send(
        &app,
        get(&format!("/api/user-requests/{}/tasks", request_id.as_str())),
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![task], Some(run)),
    );
    let (status, body) = send(&app, get(&format!("/api/tasks/{}", task_id.as_str()))).await;
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
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );
    let (status, _) = send(&app, get("/api/tasks/missing")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn transcript_returns_messages() {
    let (store, _dir) = test_store().await;
    let task_id = TaskId::new_v4();
    let task = make_task(&task_id, &RequestId::new_v4());
    let mut message = serde_json::Map::new();
    message.insert("role".to_owned(), json!("assistant"));
    let run = make_agent_run(&task_id, vec![message]);
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![task], Some(run)),
    );
    let (status, body) = send(
        &app,
        get(&format!("/api/tasks/{}/transcript", task_id.as_str())),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json_of(&body);
    assert_eq!(body["messages"].as_array().expect("messages").len(), 1);
    assert_eq!(body["messages"][0]["role"], json!("assistant"));
}

// --- conventions / openapi -------------------------------------------------

#[tokio::test]
async fn legacy_equals_style_path_is_404() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );
    let (status, _) = send(&app, get("/api/user-request=abc")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openapi_pins_paths_and_schemas() {
    let (store, _dir) = test_store().await;
    let app = router(
        &store,
        Arc::new(FakeRunControl::new(CancelOutcome::Requested)),
        Arc::new(FakeSandboxRegistry::new(vec![])),
        fake_reads(None, vec![], None),
    );
    let (status, body) = send(&app, get("/openapi.json")).await;
    assert_eq!(status, StatusCode::OK);
    let doc = json_of(&body);

    for path in [
        "/api/user-requests",
        "/api/user-requests/{request_id}",
        "/api/user-requests/{request_id}/events",
        "/api/user-requests/{request_id}/stream",
        "/api/user-requests/{request_id}/tasks",
        "/api/tasks/{task_id}",
        "/api/tasks/{task_id}/transcript",
        "/api/stats/performance",
        "/api/stats/correctness",
        "/api/stats/agent-runs",
        "/api/stats/events",
        "/api/sandboxes",
        "/api/sandboxes/{sandbox_id}",
    ] {
        assert!(doc["paths"].get(path).is_some(), "openapi missing path {path}");
    }
    for schema in ["CreateUserRequest", "UserRequestDetail", "SandboxView", "PerformanceStats"] {
        assert!(
            doc["components"]["schemas"].get(schema).is_some(),
            "openapi missing schema {schema}"
        );
    }
}
