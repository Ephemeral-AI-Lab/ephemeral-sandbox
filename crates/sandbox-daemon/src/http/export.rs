//! The export spool stream route (spec decision 19): `GET
//! /export/<export_id>` claims a sealed spool with the single-use token from
//! the authenticated `export_layerstack` start and streams its bytes as one
//! `application/octet-stream` response. Every rejection — unknown export,
//! bad token, expiry, reuse — is one uniform 404; a mid-stream read failure
//! ends the body early and the manager's Content-Length completeness gate
//! rejects the truncated stream.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::header::{HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt as _;
use hyper::body::{Body, Frame, Incoming};
use sandbox_protocol::{EXPORT_STREAM_PATH_PREFIX, EXPORT_STREAM_TOKEN_HEADER};
use sandbox_runtime::ClaimedExportStream;

use super::response::{self, BoxBody};
use super::server::HttpState;

const STREAM_FRAME_BYTES: usize = 1024 * 1024;
const STREAM_CHANNEL_FRAMES: usize = 4;

pub(crate) async fn handle(state: Arc<HttpState>, req: Request<Incoming>) -> Response<BoxBody> {
    if req.method() != Method::GET {
        return response::text(StatusCode::METHOD_NOT_ALLOWED, "use GET");
    }
    let Some(export_id) = export_route_id(req.uri().path()) else {
        return unavailable();
    };
    let Some(token) = header_token(&req) else {
        return unavailable();
    };
    let export_id = export_id.to_owned();
    let operations = Arc::clone(&state.operations);
    let claimed = tokio::task::spawn_blocking(move || {
        operations
            .layerstack
            .claim_export_stream(&export_id, &token)
    })
    .await;
    match claimed {
        Ok(Some(claimed)) => stream_response(claimed),
        Ok(None) | Err(_) => unavailable(),
    }
}

fn export_route_id(path: &str) -> Option<&str> {
    let export_id = path.strip_prefix(EXPORT_STREAM_PATH_PREFIX)?;
    if export_id.is_empty() || export_id.contains('/') {
        return None;
    }
    Some(export_id)
}

fn header_token(req: &Request<Incoming>) -> Option<String> {
    req.headers()
        .get(EXPORT_STREAM_TOKEN_HEADER)?
        .to_str()
        .ok()
        .map(str::to_owned)
}

fn unavailable() -> Response<BoxBody> {
    response::text(StatusCode::NOT_FOUND, "export stream unavailable")
}

fn stream_response(claimed: ClaimedExportStream) -> Response<BoxBody> {
    let total = claimed.total;
    let body = spool_body(claimed.file);
    let mut response = Response::new(body.boxed());
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, HeaderValue::from(total));
    response
}

/// Streams the claimed (already unlinked) spool fd as 1 MiB body frames fed
/// by ONE sequential blocking reader through a small bounded channel — no
/// per-read thread-pool handoff in the response path, and the only overlap
/// is the channel/socket buffer filling while the peer drains (the sanctioned
/// single-stream overlap). A read error terminates the body early instead of
/// erroring: the manager's completeness gate (received == Content-Length)
/// converts the truncation into a clean abort. A dropped body hangs up the
/// channel and the reader exits on its next send.
fn spool_body(file: std::fs::File) -> SpoolBody {
    let (sender, receiver) = tokio::sync::mpsc::channel::<Bytes>(STREAM_CHANNEL_FRAMES);
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;

        let mut file = file;
        loop {
            let mut buf = vec![0u8; STREAM_FRAME_BYTES];
            match file.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    buf.truncate(read);
                    if sender.blocking_send(Bytes::from(buf)).is_err() {
                        break;
                    }
                }
            }
        }
    });
    SpoolBody { receiver }
}

struct SpoolBody {
    receiver: tokio::sync::mpsc::Receiver<Bytes>,
}

impl Body for SpoolBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.get_mut().receiver.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(None) => Poll::Ready(None),
        }
    }
}
