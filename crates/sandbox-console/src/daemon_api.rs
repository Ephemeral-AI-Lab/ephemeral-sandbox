use std::sync::Arc;

use http::{Request, Response};
use hyper::body::Incoming;

use crate::endpoint;
use crate::proxy;
use crate::response::BoxBody;
use crate::state::AppState;

pub async fn handle(
    state: &Arc<AppState>,
    route: Route,
    req: Request<Incoming>,
) -> Response<BoxBody> {
    let endpoint = match endpoint::resolve(state, &route.sandbox_id).await {
        Ok(endpoint) => endpoint,
        Err(error) => return error.into_response(),
    };
    proxy::forward_to_endpoint(
        &endpoint,
        "/files/list",
        req,
        None,
        state.config.proxy_connect_timeout,
        state.config.proxy_response_timeout,
    )
    .await
}

pub struct Route {
    sandbox_id: String,
}

pub fn route(path: &str) -> Option<Route> {
    let rest = path.strip_prefix("/api/sandboxes/")?;
    let sandbox_id = rest.strip_suffix("/files/list")?;
    if sandbox_id.is_empty() || sandbox_id.contains('/') {
        return None;
    }
    Some(Route {
        sandbox_id: sandbox_id.to_owned(),
    })
}
