//! probe
use axum::extract::ws::WebSocketUpgrade;
use axum::http::HeaderMap;

#[allow(dead_code)]
async fn probe(up: Option<WebSocketUpgrade>, _h: HeaderMap) -> axum::response::Response {
    match up {
        Some(u) => u.on_upgrade(|_socket| async {}),
        None => axum::response::IntoResponse::into_response("sse"),
    }
}
