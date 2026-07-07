//! The manager-owned export transaction (the `checkpoint_squash` template):
//! guard the host destination, forward `export_layerstack` to the sandbox
//! daemon, page the compressed delta back in bounded `read_export_chunk`
//! forwards (every forward reuses the manager request's `request_id`), hand
//! the stream to the host-side applier, and merge one result line. The
//! manager — a host process — is the only host writer; both CLIs stay pure
//! catalog clients.

use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use sandbox_protocol::{error_kind, CliOperationScope, Request, Response};
use serde_json::{json, Value};

use crate::export_apply::{
    apply_dir_delta, write_archive, ArchiveFormat, DirApplyStats, MAX_STREAM_BYTES,
};
use crate::operation::ManagerServices;
use crate::router::forward_sandbox_request;
use crate::ManagerError;

const RUNTIME_EXPORT_OP: &str = "export_layerstack";
const RUNTIME_CHUNK_OP: &str = "read_export_chunk";
const EXPORT_SPOOL_DIR: &str = ".export";

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
    let compressed = page_stream(services, request, sandbox_id, export_id)?;
    match format {
        ExportFormat::Dir => {
            let stats = apply_dir_delta(&compressed, &dest_path)
                .map_err(|message| export_failed(&message).into_response())?;
            Ok(dir_result(&start, &stats))
        }
        ExportFormat::Tar | ExportFormat::TarZst => {
            let archive_format = match format {
                ExportFormat::Tar => ArchiveFormat::Tar,
                _ => ArchiveFormat::TarZst,
            };
            let bytes_written = write_archive(&compressed, &dest_path, archive_format)
                .map_err(|message| export_failed(&message).into_response())?;
            Ok(archive_result(&start, format, bytes_written))
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
