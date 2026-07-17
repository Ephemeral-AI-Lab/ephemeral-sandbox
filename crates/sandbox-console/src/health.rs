//! `/api/sandboxes/:id/health`: report manager-owned readiness without
//! contacting the sandbox daemon. The endpoint is retained for compatibility,
//! but an idle console health refresh is only a sandbox-record lookup.

use http::StatusCode;
use hyper::Response;
use serde_json::json;

use crate::endpoint;
use crate::response::{self, BoxBody};
use crate::state::AppState;

pub async fn handle(state: &AppState, sandbox_id: &str) -> Response<BoxBody> {
    match endpoint::resolve(state, sandbox_id).await {
        Ok(_) => response::json_value(
            StatusCode::OK,
            &json!({
                "status": "ok",
                "detail": "manager record is ready and has a daemon_http endpoint"
            }),
        ),
        Err(error) => return error.into_response(),
    }
}
