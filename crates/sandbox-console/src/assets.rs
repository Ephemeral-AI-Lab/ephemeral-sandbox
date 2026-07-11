//! Static SPA asset serving with client-route fallback. Unknown extensions
//! and missing files fall back to `index.html` so deep links resolve from a
//! cold load; a built-in placeholder page covers the unbuilt-SPA case.

use std::path::{Component, Path, PathBuf};

use http::header::{HeaderValue, CONTENT_SECURITY_POLICY};
use http::StatusCode;
use hyper::Response;

use crate::response::{self, BoxBody};

const PLACEHOLDER_PAGE: &str = "<!doctype html>\
<html lang=\"en\"><head><meta charset=\"utf-8\"><title>EphemeralOS Console</title></head>\
<body style=\"font-family: system-ui; margin: 4rem auto; max-width: 40rem; color: #1a1d21;\">\
<h1>EphemeralOS Console</h1>\
<p>The console server is running, but the SPA assets are not built.</p>\
<p>Build them with <code>cargo run -p xtask -- package-console</code>, then reload.</p>\
</body></html>";

pub async fn serve(assets_dir: Option<&Path>, uri_path: &str) -> Response<BoxBody> {
    let Some(assets_dir) = assets_dir else {
        return console_document(response::html(StatusCode::OK, PLACEHOLDER_PAGE));
    };
    let Some(relative) = sanitize(uri_path) else {
        return response::text(StatusCode::BAD_REQUEST, "invalid asset path");
    };
    let candidate = if relative.as_os_str().is_empty() {
        assets_dir.join("index.html")
    } else {
        assets_dir.join(&relative)
    };
    match read_file(&candidate).await {
        Some(body) => file_response(&candidate, body),
        None => match read_file(&assets_dir.join("index.html")).await {
            Some(body) => {
                console_document(response::no_store(response::html(StatusCode::OK, body)))
            }
            None => console_document(response::html(StatusCode::OK, PLACEHOLDER_PAGE)),
        },
    }
}

fn sanitize(uri_path: &str) -> Option<PathBuf> {
    let trimmed = uri_path.trim_start_matches('/');
    let mut relative = PathBuf::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(relative)
}

async fn read_file(path: &Path) -> Option<Vec<u8>> {
    if !path.is_file() {
        return None;
    }
    tokio::fs::read(path).await.ok()
}

fn file_response(path: &Path, body: Vec<u8>) -> Response<BoxBody> {
    let content_type = content_type_for(path);
    let mut response = Response::new(response::full(body));
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static(content_type),
    );
    if is_fingerprinted(path) {
        response.headers_mut().insert(
            http::header::CACHE_CONTROL,
            http::HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    if content_type.starts_with("text/html") {
        console_document(response)
    } else {
        response
    }
}

fn console_document(mut response: Response<BoxBody>) -> Response<BoxBody> {
    response.headers_mut().append(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-src 'self'"),
    );
    response
}

fn is_fingerprinted(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|part| part == "assets")
    })
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
