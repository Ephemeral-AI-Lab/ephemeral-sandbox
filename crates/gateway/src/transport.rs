use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::json;

use crate::catalog::Catalog;
use crate::engine::Engine;
use crate::router::{handle, Surface};
use crate::wire::{
    bare_trace_context, elapsed_us, error_response, parse_request, read_request_line,
    response_line, REQUEST_READ_TIMEOUT,
};

static GATEWAY_CONNECTION_SEQ: AtomicU64 = AtomicU64::new(1);
const MAX_CONCURRENT_CONNECTIONS: usize = 256;

struct ConnectionLimiter {
    active: AtomicUsize,
}

struct ConnectionPermit {
    limiter: Arc<ConnectionLimiter>,
}

impl ConnectionLimiter {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
        }
    }

    fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        let mut active = self.active.load(Ordering::Acquire);
        loop {
            if active >= MAX_CONCURRENT_CONNECTIONS {
                return None;
            }
            match self.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ConnectionPermit {
                        limiter: Arc::clone(self),
                    });
                }
                Err(next) => active = next,
            }
        }
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.limiter.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn next_gateway_connection_id() -> String {
    format!(
        "gwc-{}",
        GATEWAY_CONNECTION_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn operator_socket_path(listen: &Path) -> PathBuf {
    let mut name = listen.file_name().unwrap_or_default().to_os_string();
    name.push(".operator");
    listen.with_file_name(name)
}

pub(crate) fn serve(listen: &Path, engine: Arc<dyn Engine>) -> Result<()> {
    let catalog = Arc::new(Catalog::load_builtin()?);
    serve_with_catalog(listen, catalog, engine)
}

pub(crate) fn serve_with_catalog(
    listen: &Path,
    catalog: Arc<Catalog>,
    engine: Arc<dyn Engine>,
) -> Result<()> {
    let operator_path = operator_socket_path(listen);
    let operator = bind(&operator_path)?;
    let connection_limiter = Arc::new(ConnectionLimiter::new());
    {
        let catalog = Arc::clone(&catalog);
        let engine = Arc::clone(&engine);
        let socket_path: Arc<str> = Arc::from(operator_path.to_string_lossy().as_ref());
        let connection_limiter = Arc::clone(&connection_limiter);
        std::thread::spawn(move || {
            accept_loop(
                &operator,
                Surface::Operator,
                &socket_path,
                catalog,
                engine,
                connection_limiter,
            );
        });
    }
    let client = bind(listen)?;
    eprintln!(
        "sandbox-gateway: serving {} (operator: {})",
        listen.display(),
        operator_path.display()
    );
    let socket_path: Arc<str> = Arc::from(listen.to_string_lossy().as_ref());
    accept_loop(
        &client,
        Surface::Client,
        &socket_path,
        catalog,
        engine,
        connection_limiter,
    );
    Ok(())
}

fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("remove stale socket {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        let parent_existed = parent.exists();
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket dir {}", parent.display()))?;
        #[cfg(unix)]
        if !parent_existed {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("chmod 700 {}", parent.display()))?;
        }
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    Ok(listener)
}

fn accept_loop(
    listener: &UnixListener,
    surface: Surface,
    socket_path: &Arc<str>,
    catalog: Arc<Catalog>,
    engine: Arc<dyn Engine>,
    connection_limiter: Arc<ConnectionLimiter>,
) {
    loop {
        let Ok((stream, _)) = listener.accept() else {
            continue;
        };
        let Some(permit) = connection_limiter.try_acquire() else {
            continue;
        };
        let catalog = Arc::clone(&catalog);
        let engine = Arc::clone(&engine);
        let socket_path = Arc::clone(socket_path);
        std::thread::spawn(move || {
            let _permit = permit;
            handle_connection(stream, surface, &socket_path, &catalog, &*engine);
        });
    }
}

pub(crate) fn handle_connection(
    stream: UnixStream,
    surface: Surface,
    socket_path: &str,
    catalog: &Catalog,
    engine: &dyn Engine,
) {
    let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
    let gateway_connection_id = next_gateway_connection_id();
    let read_started = Instant::now();
    let parsed = read_request_line(&stream).and_then(|line| {
        let request_bytes = line.len();
        parse_request(&line).map(|mut request| {
            request.trace.push_gateway_event(
                "gateway.transport",
                "accepted",
                json!({
                    "gateway_connection_id": gateway_connection_id,
                    "surface": surface.label(),
                    "socket_path": socket_path,
                }),
            );
            request.trace.push_gateway_event(
                "gateway.transport",
                "request_read",
                json!({
                    "gateway_connection_id": gateway_connection_id,
                    "surface": surface.label(),
                    "socket_path": socket_path,
                    "request_bytes": request_bytes,
                    "read_duration_us": elapsed_us(read_started),
                }),
            );
            request
        })
    });
    let (response, trace_target) = match parsed {
        Ok(request) => {
            let trace_target = request
                .sandbox_id
                .clone()
                .map(|sandbox_id| (sandbox_id, request.trace.clone()));
            (handle(catalog, engine, surface, &request), trace_target)
        }
        Err(err) => {
            // A parse failure with a known sandbox still closes its trace; a
            // request too malformed to name a sandbox has no store row to join.
            let trace_target = err.sandbox_id.clone().map(|sandbox_id| {
                let trace = bare_trace_context();
                engine.record_trace_event(
                    &sandbox_id,
                    &trace,
                    "gateway.transport",
                    "parse_failed",
                    json!({
                        "gateway_connection_id": gateway_connection_id,
                        "surface": surface.label(),
                        "socket_path": socket_path,
                        "read_duration_us": elapsed_us(read_started),
                        "error_kind": err.kind,
                        "message": err.message,
                    }),
                );
                (sandbox_id, trace)
            });
            (error_response(err.kind, &err.message), trace_target)
        }
    };
    let mut stream = stream;
    let line = response_line(&response);
    let write_started = Instant::now();
    let write_result = stream.write_all(&line);
    if let Some((sandbox_id, trace)) = trace_target {
        match &write_result {
            Ok(()) => engine.record_trace_event(
                &sandbox_id,
                &trace,
                "gateway.transport",
                "response_written",
                json!({
                    "gateway_connection_id": gateway_connection_id,
                    "surface": surface.label(),
                    "socket_path": socket_path,
                    "response_bytes": line.len(),
                    "write_duration_us": elapsed_us(write_started),
                }),
            ),
            Err(err) => engine.record_trace_event(
                &sandbox_id,
                &trace,
                "gateway.transport",
                "write_failed",
                json!({
                    "gateway_connection_id": gateway_connection_id,
                    "surface": surface.label(),
                    "socket_path": socket_path,
                    "response_bytes": line.len(),
                    "write_duration_us": elapsed_us(write_started),
                    "error_kind": "write_failed",
                    "message": err.to_string(),
                }),
            ),
        }
    }
    if write_result.is_ok() {
        let _ = stream.flush();
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

#[cfg(test)]
mod tests {
    use super::{ConnectionLimiter, MAX_CONCURRENT_CONNECTIONS};
    use std::sync::Arc;

    #[test]
    fn connection_limiter_rejects_after_limit_and_releases_permits() {
        let limiter = Arc::new(ConnectionLimiter::new());
        let mut permits = Vec::new();
        for _ in 0..MAX_CONCURRENT_CONNECTIONS {
            permits.push(
                limiter
                    .try_acquire()
                    .expect("permit should be available below the limit"),
            );
        }

        assert!(
            limiter.try_acquire().is_none(),
            "limiter should reject once all permits are held"
        );
        permits.pop();
        assert!(
            limiter.try_acquire().is_some(),
            "dropping a permit should reopen capacity"
        );
    }
}
