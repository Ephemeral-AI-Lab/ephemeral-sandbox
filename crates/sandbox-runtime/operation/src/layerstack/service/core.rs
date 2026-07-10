use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use sandbox_observability::Observer;
use sandbox_protocol::EXPORT_STREAM_TOKEN_TTL_S;

use crate::file::FileService;
use crate::layerstack::LayerStackServiceError;
use crate::services::LayerstackRuntimeConfig;

pub(crate) const EXPORT_SPOOL_DIR: &str = ".export";

/// One sealed export spool: the on-disk `tar.zst`, its byte total, and the
/// single-use stream token minted with it (spec decision 19). The registry is
/// in-memory by design — a daemon restart drops it and delivery aborts with
/// export-not-found; re-running the export is the recovery.
pub(crate) struct ExportSpool {
    pub(crate) path: PathBuf,
    pub(crate) total: u64,
    pub(crate) stream_token: String,
    pub(crate) minted_at: Instant,
}

/// A successfully claimed spool stream: the opened (and already unlinked)
/// spool file plus its byte total for the response content length.
pub struct ClaimedExportStream {
    pub file: std::fs::File,
    pub total: u64,
}

pub struct LayerStackService {
    pub(crate) layer_stack_root: PathBuf,
    pub(crate) scratch_root: PathBuf,
    pub(crate) config: LayerstackRuntimeConfig,
    pub(crate) obs: Observer,
    pub(crate) file: Arc<FileService>,
    pub(crate) audit_gate: Mutex<()>,
    pub(crate) export_spools: Mutex<HashMap<String, ExportSpool>>,
}

impl LayerStackService {
    pub fn new(
        layer_stack_root: PathBuf,
        scratch_root: PathBuf,
        config: LayerstackRuntimeConfig,
        obs: Observer,
        file: Arc<FileService>,
    ) -> Result<Self, LayerStackServiceError> {
        sandbox_runtime_layerstack::require_workspace_binding(&layer_stack_root).map_err(
            |error| LayerStackServiceError::Init {
                layer_stack_root: layer_stack_root.clone(),
                error: error.to_string(),
            },
        )?;
        Ok(Self {
            layer_stack_root,
            scratch_root,
            config,
            obs,
            file,
            audit_gate: Mutex::new(()),
            export_spools: Mutex::new(HashMap::new()),
        })
    }

    #[must_use]
    pub fn layer_stack_root(&self) -> &std::path::Path {
        &self.layer_stack_root
    }

    #[must_use]
    pub(crate) fn export_spool_dir(&self) -> PathBuf {
        self.scratch_root.join(EXPORT_SPOOL_DIR)
    }

    /// Claim an export spool for stream delivery (spec decision 19): compare
    /// the token in constant time, enforce the mint TTL, and on success take
    /// the registry entry, open the spool, and unlink it so the bytes live
    /// only on the returned fd. Reuse, expiry, an unknown export, and a token
    /// minted for a different export all return `None` — the caller must not
    /// distinguish them. A mismatched token does NOT consume the entry.
    pub fn claim_export_stream(
        &self,
        export_id: &str,
        stream_token: &str,
    ) -> Option<ClaimedExportStream> {
        self.claim_export_stream_at(export_id, stream_token, Instant::now())
    }

    /// Deterministic-clock variant of [`Self::claim_export_stream`].
    pub fn claim_export_stream_at(
        &self,
        export_id: &str,
        stream_token: &str,
        now: Instant,
    ) -> Option<ClaimedExportStream> {
        let mut spools = self
            .export_spools
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entry = spools.get(export_id)?;
        let expired =
            now.saturating_duration_since(entry.minted_at).as_secs() >= EXPORT_STREAM_TOKEN_TTL_S;
        if expired {
            if let Some(entry) = spools.remove(export_id) {
                let _ = std::fs::remove_file(&entry.path);
            }
            return None;
        }
        if !constant_time_eq(entry.stream_token.as_bytes(), stream_token.as_bytes()) {
            return None;
        }
        let entry = spools.remove(export_id)?;
        let file = std::fs::File::open(&entry.path);
        let _ = std::fs::remove_file(&entry.path);
        Some(ClaimedExportStream {
            file: file.ok()?,
            total: entry.total,
        })
    }
}

/// Byte-wise constant-time equality. Lengths are public (both sides are
/// daemon-minted fixed-size tokens), so the length check may short-circuit;
/// the content comparison never does.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
