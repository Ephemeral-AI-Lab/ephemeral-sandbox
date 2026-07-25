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

use super::hidden_validation::HiddenValidationWorker;

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
    active_lease_counter: sandbox_runtime_layerstack::ActiveLeaseCounter,
    _route_observation: sandbox_runtime_layerstack::HiddenValidationObservation,
    pub(super) hidden_validation: Option<HiddenValidationWorker>,
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
        let stack = sandbox_runtime_layerstack::LayerStack::open(layer_stack_root.clone())
            .map_err(|error| LayerStackServiceError::LayerStack {
                operation: "open active lease counter",
                error,
            })?;
        let active_lease_counter = stack.active_lease_counter();
        let hidden_observation = stack.hidden_validation_observation();
        hidden_observation.configure(config.rollout_mode);
        let hidden_validation = match config.rollout_mode {
            sandbox_runtime_layerstack::service::StorageRolloutMode::Legacy => None,
            sandbox_runtime_layerstack::service::StorageRolloutMode::Validation => {
                Some(HiddenValidationWorker::spawn(
                    stack,
                    layer_stack_root.clone(),
                    hidden_observation.clone(),
                )?)
            }
            sandbox_runtime_layerstack::service::StorageRolloutMode::StrictCandidate => {
                if !cfg!(target_os = "linux") {
                    return Err(LayerStackServiceError::Init {
                        layer_stack_root,
                        error: "strict candidate profile linux-overlayfs-v1 requires Linux"
                            .to_owned(),
                    });
                }
                None
            }
        };
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
            active_lease_counter,
            _route_observation: hidden_observation,
            hidden_validation,
        })
    }

    #[must_use]
    pub fn layer_stack_root(&self) -> &std::path::Path {
        &self.layer_stack_root
    }

    #[must_use]
    pub fn active_lease_count(&self) -> usize {
        self.active_lease_counter.active_lease_count()
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

    #[doc(hidden)]
    pub fn force_next_hidden_validation_mismatch_for_tests(&self) {
        if let Some(worker) = &self.hidden_validation {
            worker.force_next_mismatch();
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn hidden_validation_last_correlation(&self) -> Option<String> {
        self.hidden_validation
            .as_ref()
            .and_then(HiddenValidationWorker::last_correlation)
    }

    #[doc(hidden)]
    pub fn pause_hidden_validation_for_tests(&self, paused: bool) {
        if let Some(worker) = &self.hidden_validation {
            worker.set_paused(paused);
        }
    }
}
