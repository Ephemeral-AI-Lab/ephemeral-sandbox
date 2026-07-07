//! The manager-owned export transaction (the `checkpoint_squash` template):
//! guard the host destination, forward `export_layerstack` to the sandbox
//! daemon, pull the sealed spool back over one token-gated `daemon_http`
//! octet-stream (spec decision 19; bounded `read_export_chunk` paging is the
//! compatibility fallback), hand the stream to the host-side applier, and
//! merge one result line. The manager — a host process — is the only host
//! writer; both CLIs stay pure catalog clients.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use sandbox_protocol::{
    error_kind, CliOperationScope, Request, Response, EXPORT_STREAM_PATH_PREFIX,
    EXPORT_STREAM_TOKEN_FIELD, EXPORT_STREAM_TOKEN_HEADER, REQUEST_READ_TIMEOUT_S,
};
use serde_json::{json, Value};

use crate::export_apply::{
    apply_dir_delta, write_archive, ArchiveFormat, DirApplyStats, MAX_STREAM_BYTES,
};
use crate::operation::ManagerServices;
use crate::router::forward_sandbox_request;
use crate::{ManagerError, SandboxHttpEndpoint, SandboxId};

const RUNTIME_EXPORT_OP: &str = "export_layerstack";
const RUNTIME_CHUNK_OP: &str = "read_export_chunk";
const EXPORT_SPOOL_DIR: &str = ".export";
const MAX_STREAM_HEAD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportFormat {
    Dir,
    Tar,
    TarZst,
}

impl ExportFormat {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "dir" => Some(Self::Dir),
            "tar" => Some(Self::Tar),
            "tar-zst" => Some(Self::TarZst),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::Tar => "tar",
            Self::TarZst => "tar-zst",
        }
    }
}

pub(crate) fn dispatch_export_changes(services: &ManagerServices, request: &Request) -> Response {
    let sandbox_id = match request.required_string("sandbox_id") {
        Ok(sandbox_id) => sandbox_id,
        Err(response) => return response,
    };
    let dest = match request.required_string("dest") {
        Ok(dest) => dest,
        Err(response) => return response,
    };
    let format = match request.optional_string("format") {
        Ok(format) => format.unwrap_or_else(|| "dir".to_owned()),
        Err(response) => return response,
    };
    let Some(format) = ExportFormat::parse(&format) else {
        return Response::fault(
            error_kind::INVALID_REQUEST,
            format!("format must be dir, tar, or tar-zst: {format}"),
        );
    };
    match run_export_changes(services, request, &sandbox_id, &dest, format) {
        Ok(value) => Response::ok(value),
        Err(response) => response,
    }
}

fn run_export_changes(
    services: &ManagerServices,
    request: &Request,
    sandbox_id: &str,
    dest: &str,
    format: ExportFormat,
) -> Result<Value, Response> {
    let normalized = guard_dest(services, dest, format).map_err(ManagerError::into_response)?;
    let start = forward(services, request, sandbox_id, RUNTIME_EXPORT_OP, json!({}))?;
    let start = translate_stale_start(start);
    if start.get("error").is_some() {
        return Err(Response::ok(start));
    }
    let export_id = start["export_id"]
        .as_str()
        .ok_or_else(|| export_failed("daemon start result carries no export_id").into_response())?;
    let dest_path =
        prepare_dest(services, dest, &normalized, format).map_err(ManagerError::into_response)?;
    match stream_delivery(services, &start, sandbox_id) {
        Some((endpoint, stream_token)) => {
            let delivery = open_spool_stream(&endpoint, export_id, &stream_token)
                .map_err(|message| export_failed(&message).into_response())?;
            render(delivery, format, &dest_path, &start)
        }
        None => {
            let compressed = page_stream(services, request, sandbox_id, export_id)?;
            render(compressed.as_slice(), format, &dest_path, &start)
        }
    }
}

/// Render one delivery stream — the token-gated socket or the paged buffer —
/// per the requested format. The dir applier's validation pass and the
/// archive writers consume the stream as it arrives (the sanctioned
/// single-worker overlap); every transport fault (truncation, overrun,
/// timeout) surfaces before any rename or mutation.
fn render(
    mut delivery: impl Read,
    format: ExportFormat,
    dest_path: &Path,
    start: &Value,
) -> Result<Value, Response> {
    match format {
        ExportFormat::Dir => {
            let stats = apply_dir_delta(&mut delivery, dest_path)
                .map_err(|message| export_failed(&message).into_response())?;
            Ok(dir_result(start, &stats))
        }
        ExportFormat::Tar | ExportFormat::TarZst => {
            let archive_format = match format {
                ExportFormat::Tar => ArchiveFormat::Tar,
                _ => ArchiveFormat::TarZst,
            };
            let bytes_written = write_archive(&mut delivery, dest_path, archive_format)
                .map_err(|message| export_failed(&message).into_response())?;
            Ok(archive_result(start, format, bytes_written))
        }
    }
}

fn forward(
    services: &ManagerServices,
    request: &Request,
    sandbox_id: &str,
    op: &str,
    args: Value,
) -> Result<Value, Response> {
    let runtime_request = Request::new(
        op,
        request.request_id.clone(),
        CliOperationScope::sandbox(sandbox_id.to_owned()),
        args,
    );
    match forward_sandbox_request(services, runtime_request) {
        Ok(response) => Ok(response.into_json_value()),
        Err(error) => Err(error.into_response()),
    }
}

fn page_stream(
    services: &ManagerServices,
    request: &Request,
    sandbox_id: &str,
    export_id: &str,
) -> Result<Vec<u8>, Response> {
    let mut compressed: Vec<u8> = Vec::new();
    loop {
        let args = json!({ "export_id": export_id, "offset": compressed.len() as u64 });
        let value = forward(services, request, sandbox_id, RUNTIME_CHUNK_OP, args)?;
        if let Some(error) = value.get("error") {
            let message = error["message"].as_str().unwrap_or("daemon fault");
            return Err(
                export_failed(&format!("read_export_chunk failed: {message}")).into_response(),
            );
        }
        let chunk = value["chunk"].as_str().ok_or_else(|| {
            export_failed("read_export_chunk returned no chunk field").into_response()
        })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(chunk)
            .map_err(|error| {
                export_failed(&format!("chunk is not valid base64: {error}")).into_response()
            })?;
        let eof = value["eof"].as_bool().ok_or_else(|| {
            export_failed("read_export_chunk returned no eof field").into_response()
        })?;
        if (compressed.len() as u64).saturating_add(bytes.len() as u64) > MAX_STREAM_BYTES {
            return Err(export_failed(&format!(
                "export stream cap exceeded ({MAX_STREAM_BYTES} compressed bytes)"
            ))
            .into_response());
        }
        if bytes.is_empty() && !eof {
            return Err(export_failed("daemon returned an empty non-final chunk").into_response());
        }
        compressed.extend_from_slice(&bytes);
        if eof {
            return Ok(compressed);
        }
    }
}

/// Stream delivery is available when the start result carries a stream token
/// (a daemon predating decision 19 sends none) and the sandbox record carries
/// a `daemon_http` endpoint. Otherwise the chunk-paging fallback runs.
fn stream_delivery(
    services: &ManagerServices,
    start: &Value,
    sandbox_id: &str,
) -> Option<(SandboxHttpEndpoint, String)> {
    let stream_token = start[EXPORT_STREAM_TOKEN_FIELD].as_str()?.to_owned();
    let id = SandboxId::new(sandbox_id.to_owned()).ok()?;
    let endpoint = services.store.inspect(&id).ok()?.daemon_http?;
    Some((endpoint, stream_token))
}

/// Open the sealed spool as one `GET /export/<export_id>` octet-stream over
/// a single blocking TCP connection — no base64, no JSON framing, no
/// per-chunk round trips — and return a bounded reader over its body so the
/// applier consumes bytes while they arrive. `MAX_STREAM_BYTES` is enforced
/// against the declared length before the first body byte and the whole
/// exchange rides one `REQUEST_READ_TIMEOUT_S` deadline. Completeness is a
/// hard gate the reader itself enforces: the response must carry
/// `Content-Length`, a short body errors as truncation, and bytes beyond the
/// declared length error as overrun — a torn stream can never become a
/// valid-looking archive or a mutated dest.
fn open_spool_stream(
    endpoint: &SandboxHttpEndpoint,
    export_id: &str,
    stream_token: &str,
) -> Result<SpoolStreamReader, String> {
    let deadline = Instant::now() + Duration::from_secs_f64(REQUEST_READ_TIMEOUT_S);
    let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .map_err(|error| format!("connect export stream: {error}"))?;
    let _ = stream.set_nodelay(true);
    let request = format!(
        "GET {EXPORT_STREAM_PATH_PREFIX}{export_id} HTTP/1.1\r\n\
         host: {}:{}\r\n\
         {EXPORT_STREAM_TOKEN_HEADER}: {stream_token}\r\n\
         connection: close\r\n\r\n",
        endpoint.host, endpoint.port
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("send export stream request: {error}"))?;
    let (head, buffered) = read_stream_head(&mut stream, deadline)?;
    let content_length = parse_stream_head(&head)?;
    if content_length > MAX_STREAM_BYTES {
        return Err(format!(
            "export stream cap exceeded ({MAX_STREAM_BYTES} compressed bytes)"
        ));
    }
    if buffered.len() as u64 > content_length {
        return Err("export stream sent more bytes than its content-length".to_owned());
    }
    Ok(SpoolStreamReader {
        stream,
        buffered,
        buffered_pos: 0,
        received: 0,
        content_length,
        deadline,
        eof_confirmed: false,
    })
}

/// Bounded body reader for the spool stream: serves the head-read remainder
/// first, then the socket under the export deadline; yields exactly
/// `content_length` bytes. A short body errors as truncation; after the last
/// byte, one confirming read must see the daemon's close or the stream
/// errors as overrun.
struct SpoolStreamReader {
    stream: TcpStream,
    buffered: Vec<u8>,
    buffered_pos: usize,
    received: u64,
    content_length: u64,
    deadline: Instant,
    eof_confirmed: bool,
}

impl Read for SpoolStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.buffered_pos < self.buffered.len() {
            let available = &self.buffered[self.buffered_pos..];
            let take = available.len().min(buf.len());
            buf[..take].copy_from_slice(&available[..take]);
            self.buffered_pos += take;
            self.received += take as u64;
            return Ok(take);
        }
        if self.received >= self.content_length {
            return self.confirm_eof();
        }
        let remaining = self.content_length - self.received;
        let window = buf
            .len()
            .min(usize::try_from(remaining).unwrap_or(buf.len()));
        let read = read_with_deadline(&mut self.stream, &mut buf[..window], self.deadline)
            .map_err(std::io::Error::other)?;
        if read == 0 {
            return Err(std::io::Error::other(format!(
                "export stream truncated: {} of {} bytes received",
                self.received, self.content_length
            )));
        }
        self.received += read as u64;
        Ok(read)
    }
}

impl SpoolStreamReader {
    fn confirm_eof(&mut self) -> std::io::Result<usize> {
        if self.eof_confirmed {
            return Ok(0);
        }
        let mut probe = [0u8; 64];
        let read = read_with_deadline(&mut self.stream, &mut probe, self.deadline)
            .map_err(std::io::Error::other)?;
        if read != 0 {
            return Err(std::io::Error::other(
                "export stream sent more bytes than its content-length".to_owned(),
            ));
        }
        self.eof_confirmed = true;
        Ok(0)
    }
}

/// Read until the end of the HTTP response head (`\r\n\r\n`), returning the
/// head and whatever body bytes arrived with it. The head is capped so a
/// hostile daemon cannot balloon it.
fn read_stream_head(
    stream: &mut TcpStream,
    deadline: Instant,
) -> Result<(String, Vec<u8>), String> {
    let mut collected: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if let Some(split) = find_head_end(&collected) {
            let head = String::from_utf8_lossy(&collected[..split]).into_owned();
            let body = collected[split + 4..].to_vec();
            return Ok((head, body));
        }
        if collected.len() > MAX_STREAM_HEAD_BYTES {
            return Err("export stream response head too large".to_owned());
        }
        let read = read_with_deadline(stream, &mut buf, deadline)?;
        if read == 0 {
            return Err("export stream closed before a full response head".to_owned());
        }
        collected.extend_from_slice(&buf[..read]);
    }
}

fn find_head_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Accept exactly `HTTP/1.1 200` with a `content-length` header; anything
/// else — 404 rejections, chunked encoding, a missing length — aborts the
/// export (re-running mints a fresh token and spool).
fn parse_stream_head(head: &str) -> Result<u64, String> {
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let mut status_parts = status.split_ascii_whitespace();
    let version = status_parts.next().unwrap_or_default();
    let code = status_parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") || code != "200" {
        return Err(format!("export stream rejected: {status}"));
    }
    let mut content_length: Option<u64> = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name == "transfer-encoding" {
            return Err(format!("export stream used unsupported {name}: {value}"));
        }
        if name == "content-length" {
            content_length = Some(
                value
                    .parse::<u64>()
                    .map_err(|_| format!("export stream content-length invalid: {value}"))?,
            );
        }
    }
    content_length.ok_or_else(|| "export stream response carries no content-length".to_owned())
}

fn read_with_deadline(
    stream: &mut TcpStream,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<usize, String> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "export stream timed out after {REQUEST_READ_TIMEOUT_S} s"
            ));
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|error| format!("set export stream timeout: {error}"))?;
        match stream.read(buf) {
            Ok(read) => return Ok(read),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("read export stream: {error}")),
        }
    }
}

/// Dest guard, before any forward and without touching the host tree:
/// absolute-only (the manager's CWD is not the caller's), deny-listed roots
/// rejected, dir/file shape rules per format. Beyond the deny-list, in-place
/// override is operator authority.
fn guard_dest(
    services: &ManagerServices,
    raw: &str,
    format: ExportFormat,
) -> Result<PathBuf, ManagerError> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(invalid_dest(raw, "dest must be an absolute host path"));
    }
    let normalized = lexical_normalize(path);
    deny_list_check(services, raw, &normalized)?;
    match format {
        ExportFormat::Dir => match std::fs::symlink_metadata(&normalized) {
            Ok(meta) if !meta.is_dir() => {
                Err(invalid_dest(raw, "dest exists and is not a directory"))
            }
            Ok(_) => Ok(normalized),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(normalized),
            Err(error) => Err(ManagerError::ExportFailed {
                message: format!("inspect dest: {error}"),
            }),
        },
        ExportFormat::Tar | ExportFormat::TarZst => {
            if normalized.is_dir() {
                return Err(invalid_dest(
                    raw,
                    "dest must be an archive file, not a directory",
                ));
            }
            let parent = normalized
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or_else(|| invalid_dest(raw, "dest has no parent directory"))?;
            if !parent.is_dir() {
                return Err(invalid_dest(raw, "dest parent directory does not exist"));
            }
            normalized
                .file_name()
                .ok_or_else(|| invalid_dest(raw, "dest has no file name"))?;
            Ok(normalized)
        }
    }
}

/// Materialize the validated destination once the daemon start succeeded:
/// dir dests are created if missing and canonicalized; archive dests resolve
/// through their canonical parent. The deny-list re-checks the canonical
/// form so a symlinked dest cannot resolve into a denied root.
fn prepare_dest(
    services: &ManagerServices,
    raw: &str,
    normalized: &Path,
    format: ExportFormat,
) -> Result<PathBuf, ManagerError> {
    match format {
        ExportFormat::Dir => {
            std::fs::create_dir_all(normalized).map_err(|error| ManagerError::ExportFailed {
                message: format!("create dest directory: {error}"),
            })?;
            let canonical =
                normalized
                    .canonicalize()
                    .map_err(|error| ManagerError::ExportFailed {
                        message: format!("canonicalize dest: {error}"),
                    })?;
            deny_list_check(services, raw, &canonical)?;
            Ok(canonical)
        }
        ExportFormat::Tar | ExportFormat::TarZst => {
            let parent = normalized
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or_else(|| invalid_dest(raw, "dest has no parent directory"))?;
            let file_name = normalized
                .file_name()
                .ok_or_else(|| invalid_dest(raw, "dest has no file name"))?
                .to_os_string();
            let parent = parent
                .canonicalize()
                .map_err(|_| invalid_dest(raw, "dest parent directory does not exist"))?;
            let full = parent.join(file_name);
            deny_list_check(services, raw, &full)?;
            Ok(full)
        }
    }
}

fn deny_list_check(services: &ManagerServices, raw: &str, path: &Path) -> Result<(), ManagerError> {
    if path == Path::new("/") {
        return Err(invalid_dest(raw, "the filesystem root is denied"));
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() && path == lexical_normalize(Path::new(&home)) {
            return Err(invalid_dest(raw, "the home directory is denied"));
        }
    }
    if let Some(registry) = services.store.registry_path() {
        if path == registry {
            return Err(invalid_dest(raw, "the manager registry file is denied"));
        }
        if let Some(state_dir) = registry.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            if path == state_dir {
                return Err(invalid_dest(raw, "the manager state directory is denied"));
            }
        }
    }
    if path
        .components()
        .any(|component| component.as_os_str() == EXPORT_SPOOL_DIR)
    {
        return Err(invalid_dest(
            raw,
            "paths inside a .export spool directory are denied",
        ));
    }
    Ok(())
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => normalized.push(Component::RootDir),
            Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        normalized
    }
}

fn translate_stale_start(value: Value) -> Value {
    let unknown = value
        .get("error")
        .and_then(|error| error.get("kind"))
        .and_then(Value::as_str)
        == Some("unknown_op");
    if unknown {
        return sandbox_protocol::error_response_with_details(
            error_kind::OPERATION_FAILED,
            "sandbox daemon does not support export_changes; recreate the sandbox so it uses the current daemon binary",
            json!({ "daemon_op": RUNTIME_EXPORT_OP }),
        );
    }
    value
}

fn dir_result(start: &Value, stats: &DirApplyStats) -> Value {
    let mut result = json!({
        "manifest_version": start["manifest_version"].clone(),
        "format": ExportFormat::Dir.as_str(),
        "layers_exported": start["layers_exported"].clone(),
        "files_written": stats.files_written,
        "symlinks_written": stats.symlinks_written,
        "deletes_applied": stats.deletes_applied,
        "opaque_clears": stats.opaque_clears,
        "skipped_unchanged": stats.skipped_unchanged,
        "bytes_written": stats.bytes_written,
    });
    attach_live_sessions(&mut result, start);
    result
}

fn archive_result(start: &Value, format: ExportFormat, bytes_written: u64) -> Value {
    let mut result = json!({
        "manifest_version": start["manifest_version"].clone(),
        "format": format.as_str(),
        "layers_exported": start["layers_exported"].clone(),
        "files_written": start["entries"]["files"].clone(),
        "symlinks_written": start["entries"]["symlinks"].clone(),
        "whiteouts_emitted": start["entries"]["whiteouts"].clone(),
        "bytes_written": bytes_written,
    });
    attach_live_sessions(&mut result, start);
    result
}

fn attach_live_sessions(result: &mut Value, start: &Value) {
    if let Some(live) = start.get("live_workspace_sessions") {
        result["live_workspace_sessions"] = live.clone();
    }
}

fn invalid_dest(raw: &str, reason: &str) -> ManagerError {
    ManagerError::InvalidExportDest {
        value: raw.to_owned(),
        reason: reason.to_owned(),
    }
}

fn export_failed(message: &str) -> ManagerError {
    ManagerError::ExportFailed {
        message: message.to_owned(),
    }
}
