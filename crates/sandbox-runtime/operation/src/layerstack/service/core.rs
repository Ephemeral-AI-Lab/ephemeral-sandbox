use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sandbox_observability_telemetry::Observer;

use crate::file::FileService;
use crate::layerstack::autosquash_engine::{
    internal_context, AutosquashQueue, AutosquashTriggerReason,
};
use crate::layerstack::LayerStackServiceError;
use crate::services::LayerstackRuntimeConfig;

pub(crate) const EXPORT_SPOOL_DIR: &str = ".export";

/// One sealed export spool: the on-disk `tar.zst` and its byte total. The
/// registry is in-memory by design — a daemon restart drops it and delivery
/// aborts with export-not-found; re-running the export is the recovery.
pub(crate) struct ExportSpool {
    pub(crate) path: PathBuf,
    pub(crate) total: u64,
}

pub struct LayerStackService {
    pub(crate) layer_stack_root: PathBuf,
    pub(crate) scratch_root: PathBuf,
    pub(crate) config: LayerstackRuntimeConfig,
    pub(crate) obs: Observer,
    pub(crate) file: Arc<FileService>,
    pub(crate) audit_gate: Mutex<()>,
    pub(crate) squash_gate: Mutex<()>,
    pub(crate) autosquash_queue: Option<Arc<AutosquashQueue>>,
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
        let autosquash_queue = config
            .autosquash_squash_at_n_layers
            .map(|_| Arc::new(AutosquashQueue::new()));
        Ok(Self {
            layer_stack_root,
            scratch_root,
            config,
            obs,
            file,
            audit_gate: Mutex::new(()),
            squash_gate: Mutex::new(()),
            autosquash_queue,
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

    pub(crate) fn notify_autosquash_layer_committed(&self) {
        let Some(queue) = &self.autosquash_queue else {
            return;
        };
        queue.notify(
            internal_context("layer-committed"),
            AutosquashTriggerReason::LayerCommitted,
        );
    }
}
