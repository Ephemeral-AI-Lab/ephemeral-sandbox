//! The daemon-local export operations, both `cli: None` (the
//! `squash_layerstack` precedent): `export_layerstack` folds the published
//! delta and spools it as one `tar.zst` under `<scratch_root>/.export/`;
//! `read_export_chunk` pages the spool back in bounded base64 frames and
//! unlinks it when the last byte is served. Export is read-only on
//! layer-stack storage; the snapshot lease pins every source layer from fold
//! start to spool completion.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use base64::Engine as _;
use sandbox_observability::record::names;
use sandbox_protocol::EXPORT_STREAM_TOKEN_FIELD;
use sandbox_runtime_layerstack::{
    emit_delta_stream, fold_delta_winners, DeltaStreamStats, LayerRef, LayerStack,
};
use serde_json::{json, Value};

use crate::layerstack::service::core::ExportSpool;
use crate::operation::OperationEntry;
use crate::services::SandboxRuntimeOperations;

const EXPORT_LAYERSTACK: OperationEntry = OperationEntry {
    name: "export_layerstack",
    cli: None,
    dispatch: dispatch_export_layerstack,
};

const READ_EXPORT_CHUNK: OperationEntry = OperationEntry {
    name: "read_export_chunk",
    cli: None,
    dispatch: dispatch_read_export_chunk,
};

const OPERATIONS: &[OperationEntry] = &[EXPORT_LAYERSTACK, READ_EXPORT_CHUNK];

pub(crate) const fn operation_entries() -> &'static [OperationEntry] {
    OPERATIONS
}

fn dispatch_export_layerstack(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    match run_export_layerstack(operations, &request.request_id) {
        Ok(value) => sandbox_protocol::Response::ok(value),
        Err(message) => {
            sandbox_protocol::Response::fault_with_details("operation_failed", message, json!({}))
        }
    }
}

fn run_export_layerstack(
    operations: &SandboxRuntimeOperations,
    request_id: &str,
) -> Result<Value, String> {
    operations
        .layerstack
        .obs
        .scope(names::LAYERSTACK_EXPORT, |span| {
            let root = operations.layerstack.layer_stack_root().to_path_buf();
            let _flight = begin_export_flight(&root)?;
            let mut stack = LayerStack::open(root.clone()).map_err(|error| error.to_string())?;
            let lease = stack
                .acquire_snapshot(request_id)
                .map_err(|error| error.to_string())?;
            let export_id = next_export_id();
            let spool_path = operations
                .layerstack
                .export_spool_dir()
                .join(format!("{export_id}.tar.zst"));
            let spooled = fold_and_spool(
                &root,
                &lease.manifest,
                &spool_path,
                operations.layerstack.config.spool_zstd_level,
            );
            let released = stack
                .release_lease(&lease.lease_id)
                .map_err(|error| error.to_string());
            let outcome = spooled.and_then(|spooled| released.map(|_| spooled));
            let (delta_layers, stats) = match outcome {
                Ok(outcome) => outcome,
                Err(error) => {
                    let _ = std::fs::remove_file(&spool_path);
                    return Err(error);
                }
            };
            apply_spool_override(&operations.layerstack.export_spool_dir(), &spool_path)?;
            let spool_bytes = std::fs::metadata(&spool_path)
                .map_err(|error| error.to_string())?
                .len();
            let stream_token = mint_stream_token();
            register_spool(
                operations,
                &export_id,
                &spool_path,
                spool_bytes,
                &stream_token,
            )?;
            span.attr("manifest_version", lease.manifest.version);
            span.attr("layers_exported", delta_layers.len());
            span.attr("spool_bytes", spool_bytes);
            let live_sessions = operations.workspace_session.session_ids();
            let mut result = json!({
                "export_id": export_id,
                "manifest_version": lease.manifest.version,
                "layers_exported": delta_layers
                    .iter()
                    .map(|layer| layer.layer_id.clone())
                    .collect::<Vec<_>>(),
                "entries": {
                    "files": stats.files,
                    "symlinks": stats.symlinks,
                    "whiteouts": stats.whiteouts,
                    "opaques": stats.opaques,
                },
                "spool_bytes": spool_bytes,
            });
            result[EXPORT_STREAM_TOKEN_FIELD] = json!(stream_token);
            if !live_sessions.is_empty() {
                result["live_workspace_sessions"] = json!(live_sessions
                    .iter()
                    .map(|session| session.0.clone())
                    .collect::<Vec<_>>());
            }
            Ok(result)
        })
}

/// Fault-injection seam (spec test-case §1.4): if an operator drops a spool
/// at `<scratch_root>/.export/OVERRIDE.tar.zst` inside the sandbox, serve it
/// in place of the honest fold. This does NOT weaken the host boundary —
/// invariant 9 already treats every daemon byte as untrusted, so a
/// container-local actor supplying the stream is exactly the threat the
/// manager applier is hardened against. The honest fold's metadata still
/// rides the start result; only the spool bytes are replaced. The override
/// is consumed (renamed) so it fires once.
const SPOOL_OVERRIDE_FILE: &str = "OVERRIDE.tar.zst";

fn apply_spool_override(export_dir: &Path, spool_path: &Path) -> Result<(), String> {
    let override_path = export_dir.join(SPOOL_OVERRIDE_FILE);
    match std::fs::rename(&override_path, spool_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("apply export spool override: {error}")),
    }
}

fn fold_and_spool(
    root: &Path,
    manifest: &sandbox_runtime_layerstack::Manifest,
    spool_path: &Path,
    spool_zstd_level: i32,
) -> Result<(Vec<LayerRef>, DeltaStreamStats), String> {
    let fold = fold_delta_winners(root, manifest).map_err(|error| error.to_string())?;
    let stats = emit_delta_stream(&fold.winners, spool_path, spool_zstd_level)
        .map_err(|error| error.to_string())?;
    Ok((fold.delta_layers, stats))
}

fn register_spool(
    operations: &SandboxRuntimeOperations,
    export_id: &str,
    spool_path: &Path,
    total: u64,
    stream_token: &str,
) -> Result<(), String> {
    let mut spools = operations
        .layerstack
        .export_spools
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    spools.insert(
        export_id.to_owned(),
        ExportSpool {
            path: spool_path.to_path_buf(),
            total,
            stream_token: stream_token.to_owned(),
            minted_at: Instant::now(),
        },
    );
    Ok(())
}

/// Single-use stream token: ≥ 244 bits of CSPRNG entropy, minted only inside
/// the authenticated start forward and never logged (spec decision 19).
fn mint_stream_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn dispatch_read_export_chunk(
    operations: &SandboxRuntimeOperations,
    request: &sandbox_protocol::Request,
) -> sandbox_protocol::Response {
    let export_id = match request.required_string("export_id") {
        Ok(export_id) => export_id,
        Err(response) => return response,
    };
    let offset = match request.required_u64("offset") {
        Ok(offset) => offset,
        Err(response) => return response,
    };
    let chunk_cap = operations.layerstack.config.export_chunk_bytes;
    let limit = match request.optional_u64("limit") {
        Ok(limit) => limit.unwrap_or(chunk_cap).min(chunk_cap),
        Err(response) => return response,
    };
    match run_read_export_chunk(operations, &export_id, offset, limit) {
        Ok(value) => sandbox_protocol::Response::ok(value),
        Err(message) => {
            sandbox_protocol::Response::fault_with_details("operation_failed", message, json!({}))
        }
    }
}

fn run_read_export_chunk(
    operations: &SandboxRuntimeOperations,
    export_id: &str,
    offset: u64,
    limit: u64,
) -> Result<Value, String> {
    let (path, total) = {
        let spools = operations
            .layerstack
            .export_spools
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let spool = spools
            .get(export_id)
            .ok_or_else(|| format!("export not found: {export_id}"))?;
        (spool.path.clone(), spool.total)
    };
    let mut file = std::fs::File::open(&path)
        .map_err(|error| format!("export spool unreadable for {export_id}: {error}"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| error.to_string())?;
    let mut chunk = Vec::new();
    file.take(limit)
        .read_to_end(&mut chunk)
        .map_err(|error| error.to_string())?;
    let len = chunk.len() as u64;
    let eof = offset.saturating_add(len) >= total;
    if eof {
        let mut spools = operations
            .layerstack
            .export_spools
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        spools.remove(export_id);
        let _ = std::fs::remove_file(&path);
    }
    Ok(json!({
        "chunk": base64::engine::general_purpose::STANDARD.encode(&chunk),
        "offset": offset,
        "len": len,
        "total": total,
        "eof": eof,
    }))
}

fn next_export_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "exp-{:x}-{:04x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

struct ExportFlight {
    busy: Arc<AtomicBool>,
}

impl Drop for ExportFlight {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
    }
}

fn begin_export_flight(root: &Path) -> Result<ExportFlight, String> {
    let busy = flight_flag_for_root(root);
    if busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return Err(format!(
            "an export is already in flight for {}",
            root.display()
        ));
    }
    Ok(ExportFlight { busy })
}

fn flight_flag_for_root(root: &Path) -> Arc<AtomicBool> {
    static FLAGS: OnceLock<Mutex<HashMap<PathBuf, Arc<AtomicBool>>>> = OnceLock::new();
    FLAGS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(root.to_path_buf())
        .or_default()
        .clone()
}
