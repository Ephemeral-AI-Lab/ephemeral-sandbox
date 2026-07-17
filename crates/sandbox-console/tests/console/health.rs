use http::StatusCode;

use crate::support;

#[tokio::test]
async fn registered_daemon_answers_ok_without_connecting_to_it() {
    let closed = support::closed_port().await;
    let gateway =
        support::FakeGateway::spawn(move |_| vec![support::record_line("eos-1", closed)]).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::get(console, "/api/sandboxes/eos-1/health").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = support::body_json(response).await;
    assert_eq!(body["status"], "ok");
    assert!(body["detail"]
        .as_str()
        .is_some_and(|detail| detail.contains("manager record")));
    assert_eq!(gateway.request_count(), 1);
}

#[tokio::test]
async fn unknown_sandbox_is_404() {
    let gateway =
        support::FakeGateway::spawn(|_| vec![support::not_found_line("eos-missing")]).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::get(console, "/api/sandboxes/eos-missing/health").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = support::body_json(response).await;
    assert_eq!(body["error"]["kind"], "unknown_sandbox");
}

#[tokio::test]
async fn sandbox_without_endpoint_is_503() {
    let gateway = support::FakeGateway::spawn(|_| vec![support::no_endpoint_line("eos-new")]).await;
    let console = support::spawn_console_default(gateway.addr).await;

    let response = support::get(console, "/api/sandboxes/eos-new/health").await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = support::body_json(response).await;
    assert_eq!(body["error"]["kind"], "sandbox_not_ready");
}
