//! `/s/:id/...` preview proxy: one prefix swap onto the sandbox's
//! `daemon_http` `/forward/...`, preserving method, query, and streamed
//! bodies while enforcing the Preview browser boundary. It tunnels
//! WebSocket/HTTP upgrades and appends a server-controlled `X-Forwarded-For`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use http::header::{
    HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONNECTION, CONTENT_SECURITY_POLICY, COOKIE,
    LOCATION, ORIGIN, PROXY_AUTHORIZATION, REFERER, SET_COOKIE, UPGRADE,
};
use http::{Request, Response, StatusCode, Uri};
use http_body_util::BodyExt as _;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::endpoint::{self, HttpEndpoint};
use crate::response::{self, BoxBody};
use crate::state::AppState;

const HOP_BY_HOP: [&str; 7] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
];

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const CLEAR_SITE_DATA: HeaderName = HeaderName::from_static("clear-site-data");
const PERMISSIONS_POLICY: HeaderName = HeaderName::from_static("permissions-policy");
const SERVICE_WORKER_ALLOWED: HeaderName = HeaderName::from_static("service-worker-allowed");

const PREVIEW_CSP: &str = "sandbox allow-scripts; frame-ancestors 'self'";
const PREVIEW_PERMISSIONS_POLICY: &str = "accelerometer=(), autoplay=(), camera=(), clipboard-read=(), clipboard-write=(), display-capture=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), payment=(), picture-in-picture=(), usb=(), web-share=()";

pub async fn handle(state: &Arc<AppState>, req: Request<Incoming>) -> Response<BoxBody> {
    let route = match PreviewRoute::parse(req.uri()) {
        Ok(route) => route,
        Err(message) => return response::text(StatusCode::BAD_REQUEST, message),
    };
    let endpoint = match endpoint::resolve(state, &route.sandbox_id).await {
        Ok(endpoint) => endpoint,
        Err(error) => return error.into_response(),
    };
    forward_to_endpoint(
        &endpoint,
        &route.forward_target,
        req,
        Some(&route.policy),
        state.config.proxy_connect_timeout,
        state.config.proxy_response_timeout,
    )
    .await
}

/// A parsed `/s/<id>/...` route: the sandbox id plus the daemon-side
/// `/forward/...` request target after the prefix swap.
struct PreviewRoute {
    sandbox_id: String,
    forward_target: String,
    policy: PreviewPolicy,
}

pub(crate) struct PreviewPolicy {
    public_prefix: String,
    public_path: String,
}

impl PreviewRoute {
    fn parse(uri: &Uri) -> Result<Self, &'static str> {
        let rest = uri
            .path()
            .strip_prefix("/s/")
            .ok_or("invalid preview route")?;
        let (sandbox_id, scope_rest) = rest.split_once('/').ok_or("invalid preview route")?;
        if sandbox_id.is_empty() {
            return Err("invalid preview route");
        }
        validate_scope(scope_rest)?;
        let mut segments = scope_rest.split('/');
        let scope = segments.next().ok_or("invalid preview route")?;
        let port = segments.next().ok_or("invalid preview route")?;
        let mut forward_target = String::with_capacity(9 + scope_rest.len());
        forward_target.push_str("/forward/");
        forward_target.push_str(scope_rest);
        if let Some(query) = uri.query() {
            forward_target.push('?');
            forward_target.push_str(query);
        }
        Ok(Self {
            sandbox_id: sandbox_id.to_owned(),
            forward_target,
            policy: PreviewPolicy {
                public_prefix: format!("/s/{sandbox_id}/{scope}/{port}"),
                public_path: uri.path().to_owned(),
            },
        })
    }
}

fn validate_scope(scope_rest: &str) -> Result<(), &'static str> {
    let port_segment = if let Some(shared) = scope_rest.strip_prefix("shared/") {
        shared
    } else if let Some(isolated) = scope_rest.strip_prefix("isolated=") {
        let (workspace_id, after) = isolated.split_once('/').ok_or("invalid preview route")?;
        if workspace_id.is_empty() {
            return Err("invalid preview route");
        }
        after
    } else {
        return Err("invalid preview route");
    };
    let port = port_segment.split('/').next().unwrap_or("");
    match port.parse::<u16>() {
        Ok(port) if port >= 1 => Ok(()),
        _ => Err("invalid preview port"),
    }
}

pub(crate) async fn forward_to_endpoint(
    endpoint: &HttpEndpoint,
    target: &str,
    req: Request<Incoming>,
    preview: Option<&PreviewPolicy>,
    connect_timeout: Duration,
    response_timeout: Duration,
) -> Response<BoxBody> {
    let stream = match timeout(
        connect_timeout,
        TcpStream::connect((endpoint.host.as_str(), endpoint.port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) => return unreachable_response("daemon_http connection failed"),
        Err(_) => return timeout_response(),
    };
    let (mut sender, connection) =
        match hyper::client::conn::http1::handshake::<_, BoxBody>(TokioIo::new(stream)).await {
            Ok(pair) => pair,
            Err(_) => return unreachable_response("daemon_http handshake failed"),
        };
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });

    if is_upgrade(req.headers()) {
        tunnel(&mut sender, target, req, preview, response_timeout).await
    } else {
        forward_plain(&mut sender, target, req, preview, response_timeout).await
    }
}

async fn forward_plain(
    sender: &mut hyper::client::conn::http1::SendRequest<BoxBody>,
    target: &str,
    req: Request<Incoming>,
    preview: Option<&PreviewPolicy>,
    response_timeout: Duration,
) -> Response<BoxBody> {
    let (parts, body) = req.into_parts();
    let peer = parts.extensions.get::<SocketAddr>().copied();
    let outbound = build_request(
        &parts.method,
        target,
        &parts.headers,
        peer,
        body.boxed(),
        false,
        preview.is_some(),
    );
    match send(sender, outbound, response_timeout).await {
        Ok(upstream) => relay_response(upstream, preview),
        Err(response) => response,
    }
}

async fn tunnel(
    sender: &mut hyper::client::conn::http1::SendRequest<BoxBody>,
    target: &str,
    mut req: Request<Incoming>,
    preview: Option<&PreviewPolicy>,
    response_timeout: Duration,
) -> Response<BoxBody> {
    let peer = req.extensions().get::<SocketAddr>().copied();
    let outbound = build_request(
        req.method(),
        target,
        req.headers(),
        peer,
        response::empty(),
        true,
        preview.is_some(),
    );
    let mut upstream = match send(sender, outbound, response_timeout).await {
        Ok(upstream) => upstream,
        Err(response) => return response,
    };
    if upstream.status() != StatusCode::SWITCHING_PROTOCOLS {
        return relay_response(upstream, preview);
    }
    let upstream_upgrade = hyper::upgrade::on(&mut upstream);
    let client_upgrade = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        if let (Ok(client), Ok(server)) = tokio::join!(client_upgrade, upstream_upgrade) {
            let mut client = TokioIo::new(client);
            let mut server = TokioIo::new(server);
            let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
        }
    });
    let (mut parts, _body) = upstream.into_parts();
    if preview.is_some() {
        apply_preview_headers(&mut parts.headers);
    }
    Response::from_parts(parts, response::empty())
}

async fn send(
    sender: &mut hyper::client::conn::http1::SendRequest<BoxBody>,
    request: Request<BoxBody>,
    response_timeout: Duration,
) -> Result<Response<Incoming>, Response<BoxBody>> {
    match timeout(response_timeout, sender.send_request(request)).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(_)) => Err(unreachable_response("daemon_http request failed")),
        Err(_) => Err(timeout_response()),
    }
}

fn build_request(
    method: &http::Method,
    target: &str,
    src_headers: &HeaderMap,
    peer: Option<SocketAddr>,
    body: BoxBody,
    upgrade: bool,
    preview: bool,
) -> Request<BoxBody> {
    let mut request = Request::new(body);
    *request.method_mut() = method.clone();
    *request.uri_mut() = target
        .parse::<Uri>()
        .unwrap_or_else(|_| Uri::from_static("/"));
    let headers = request.headers_mut();
    for (name, value) in src_headers {
        if (!preview || !is_preview_request_header(name))
            && (upgrade || !is_hop_by_hop(name.as_str()))
        {
            headers.append(name.clone(), value.clone());
        }
    }
    if let Some(peer) = peer {
        append_forwarded_for(headers, peer);
    }
    request
}

fn append_forwarded_for(headers: &mut HeaderMap, peer: SocketAddr) {
    if let Ok(value) = HeaderValue::from_str(&peer.ip().to_string()) {
        headers.insert(X_FORWARDED_FOR, value);
    }
}

fn relay_response(
    upstream: Response<Incoming>,
    preview: Option<&PreviewPolicy>,
) -> Response<BoxBody> {
    let (mut parts, body) = upstream.into_parts();
    for name in HOP_BY_HOP {
        parts.headers.remove(name);
    }
    if let Some(preview) = preview {
        if !rewrite_preview_redirect(&mut parts.headers, preview) {
            return response::text(
                StatusCode::FORBIDDEN,
                "preview redirects must stay within the selected preview route",
            );
        }
        apply_preview_headers(&mut parts.headers);
    }
    Response::from_parts(parts, body.boxed())
}

fn is_preview_request_header(name: &HeaderName) -> bool {
    matches!(
        *name,
        AUTHORIZATION | COOKIE | ORIGIN | PROXY_AUTHORIZATION | REFERER
    ) || name.as_str().starts_with("x-forwarded-")
}

fn apply_preview_headers(headers: &mut HeaderMap) {
    headers.remove(SET_COOKIE);
    headers.remove(CLEAR_SITE_DATA);
    headers.remove(SERVICE_WORKER_ALLOWED);
    headers.append(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PREVIEW_CSP),
    );
    headers.insert(
        PERMISSIONS_POLICY,
        HeaderValue::from_static(PREVIEW_PERMISSIONS_POLICY),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
}

fn rewrite_preview_redirect(headers: &mut HeaderMap, preview: &PreviewPolicy) -> bool {
    let Some(location) = headers.get(LOCATION) else {
        return true;
    };
    let Ok(location) = location.to_str() else {
        return false;
    };
    let Some(location) = preview_redirect_location(location, preview) else {
        return false;
    };
    headers.insert(LOCATION, location);
    true
}

fn preview_redirect_location(location: &str, preview: &PreviewPolicy) -> Option<HeaderValue> {
    if location.starts_with("//")
        || location.contains('\\')
        || location.to_ascii_lowercase().contains("%2e")
        || location.to_ascii_lowercase().contains("%2f")
        || location.to_ascii_lowercase().contains("%5c")
        || location.split_once(':').is_some_and(|(scheme, _)| {
            !scheme.contains('/') && !scheme.contains('?') && !scheme.contains('#')
        })
    {
        return None;
    }
    let (path, suffix) = location
        .find(['?', '#'])
        .map(|index| location.split_at(index))
        .unwrap_or((location, ""));
    let min_segments = preview
        .public_prefix
        .trim_start_matches('/')
        .split('/')
        .count();
    let base = if path.is_empty() {
        &preview.public_path
    } else if path.starts_with('/') {
        &preview.public_prefix
    } else {
        preview
            .public_path
            .rsplit_once('/')
            .map_or("", |(base, _)| base)
    };
    let mut segments: Vec<&str> = base
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." if segments.len() > min_segments => {
                segments.pop();
            }
            ".." => return None,
            _ => segments.push(segment),
        }
    }
    let trailing_slash = path.ends_with('/');
    let mut rewritten = format!("/{}", segments.join("/"));
    if trailing_slash && !rewritten.ends_with('/') {
        rewritten.push('/');
    }
    rewritten.push_str(suffix);
    HeaderValue::from_str(&rewritten).ok()
}

fn unreachable_response(detail: &'static str) -> Response<BoxBody> {
    response::text(StatusCode::BAD_GATEWAY, detail)
}

fn timeout_response() -> Response<BoxBody> {
    response::text(StatusCode::GATEWAY_TIMEOUT, "daemon_http timed out")
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers.contains_key(UPGRADE)
        && headers
            .get(CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
}

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Method;

    fn policy() -> PreviewPolicy {
        PreviewPolicy {
            public_prefix: "/s/eos-1/shared/5173".to_owned(),
            public_path: "/s/eos-1/shared/5173/nested/page".to_owned(),
        }
    }

    #[test]
    fn preview_requests_drop_console_credentials_and_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static("console_session=secret"));
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        headers.insert(ORIGIN, HeaderValue::from_static("https://console.test"));
        headers.insert(
            REFERER,
            HeaderValue::from_static("https://console.test/files"),
        );
        headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("198.51.100.7"));
        headers.insert("x-preview-test", HeaderValue::from_static("kept"));

        let request = build_request(
            &Method::GET,
            "/forward/shared/5173/",
            &headers,
            None,
            response::empty(),
            false,
            true,
        );

        for name in [COOKIE, AUTHORIZATION, ORIGIN, REFERER, X_FORWARDED_FOR] {
            assert!(request.headers().get(name).is_none());
        }
        assert_eq!(request.headers()["x-preview-test"], "kept");
    }

    #[test]
    fn preview_responses_add_the_opaque_origin_policy_and_cannot_write_console_cookies() {
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("console_session=unsafe"),
        );
        headers.insert(CLEAR_SITE_DATA, HeaderValue::from_static("\"cookies\""));
        headers.insert(SERVICE_WORKER_ALLOWED, HeaderValue::from_static("/"));

        apply_preview_headers(&mut headers);

        assert!(headers.get(SET_COOKIE).is_none());
        assert!(headers.get(CLEAR_SITE_DATA).is_none());
        assert!(headers.get(SERVICE_WORKER_ALLOWED).is_none());
        assert_eq!(headers.get(CONTENT_SECURITY_POLICY).unwrap(), PREVIEW_CSP);
        assert_eq!(
            headers.get(PERMISSIONS_POLICY).unwrap(),
            PREVIEW_PERMISSIONS_POLICY
        );
        assert_eq!(headers["referrer-policy"], "no-referrer");
    }

    #[test]
    fn preview_redirects_stay_under_the_selected_route() {
        let policy = policy();
        for (location, expected) in [
            ("/", "/s/eos-1/shared/5173/"),
            (
                "assets/app.js?rev=1",
                "/s/eos-1/shared/5173/nested/assets/app.js?rev=1",
            ),
            ("?next=1", "/s/eos-1/shared/5173/nested/page?next=1"),
        ] {
            assert_eq!(
                preview_redirect_location(location, &policy)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                expected,
            );
        }
        for location in [
            "https://console.test/api/rpc",
            "//console.test/api/rpc",
            "../../console",
            "/%2e%2e/api/rpc",
        ] {
            assert!(
                preview_redirect_location(location, &policy).is_none(),
                "{location}"
            );
        }
    }
}
