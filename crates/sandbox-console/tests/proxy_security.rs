#![forbid(unsafe_code)]

mod support;

use http::header::{AUTHORIZATION, CONTENT_SECURITY_POLICY, COOKIE, ORIGIN, REFERER, SET_COOKIE};
use http::{Method, Request, StatusCode};

#[tokio::test]
async fn preview_proxy_strips_console_authority_and_enforces_the_opaque_response_policy() {
    let daemon = support::FakeDaemonHttp::spawn().await;
    let daemon_addr = daemon.addr;
    let gateway =
        support::FakeGateway::spawn(move |_| vec![support::record_line("eos-1", daemon_addr)])
            .await;
    let console = support::spawn_console_default(gateway.addr).await;

    let request = Request::builder()
        .method(Method::GET)
        .uri("/s/eos-1/shared/5173/")
        .header(http::header::HOST, "console.test")
        .header(COOKIE, "console_session=secret")
        .header(AUTHORIZATION, "Bearer secret")
        .header(ORIGIN, "https://console.test")
        .header(REFERER, "https://console.test/files")
        .header("x-forwarded-for", "198.51.100.7")
        .body(support::empty_body())
        .expect("build proxy request");
    let (response, _sender) = support::send_request(console, request).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[CONTENT_SECURITY_POLICY],
        "sandbox allow-scripts; frame-ancestors 'self'"
    );
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(response.headers().get(SET_COOKIE).is_none());

    let echo = support::body_json(response).await;
    for name in ["cookie", "authorization", "origin", "referer"] {
        assert!(
            echo["headers"].get(name).is_none(),
            "{name} reached Preview"
        );
    }
    assert_eq!(echo["headers"]["x-forwarded-for"], "127.0.0.1");
}

#[tokio::test]
async fn opaque_preview_origins_cannot_call_console_apis() {
    let gateway = support::FakeGateway::spawn(|_| Vec::new()).await;
    let console = support::spawn_console_default(gateway.addr).await;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/rpc")
        .header(ORIGIN, "null")
        .body(support::empty_body())
        .expect("build opaque API request");

    let (response, _sender) = support::send_request(console, request).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        support::body_text(response).await,
        "opaque origins cannot call console APIs"
    );
}
