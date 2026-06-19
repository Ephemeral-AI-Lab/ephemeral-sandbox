#![forbid(unsafe_code)]

mod commit;
mod error;
mod model;
pub mod service;
mod stack;
mod storage;
mod workspace_base;

pub(crate) use storage::{fs, lock, whiteout};

pub use model::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, CasError, LayerChange, LayerPath,
    LayerRef, Manifest, MANIFEST_SCHEMA_VERSION,
};

pub use commit::model::FileResult;
pub use commit::{ChangesetResult, CommitError, CommitOptions, CommitStatus, OccTraceEvent};
pub use error::LayerStackError;
pub use stack::reclaim_unpinned_layers::{
    LeaseParentCompactionOutcome, ReclaimUnpinnedLayersCopyThroughOutcome,
    ReclaimUnpinnedLayersOutcome,
};
pub use stack::{LayerStack, Lease, MergedView, SquashOutcome};
pub use workspace_base::{
    build_workspace_base, ensure_workspace_base, read_workspace_binding, require_workspace_binding,
    WorkspaceBinding, WORKSPACE_BINDING_FILE,
};

pub(crate) const AUTO_SQUASH_MAX_DEPTH: usize = 100;

pub(crate) const LAYERS_DIR: &str = "layers";

pub(crate) const STAGING_DIR: &str = "staging";

pub const ACTIVE_MANIFEST_FILE: &str = "manifest.json";

pub(crate) const LAYER_METADATA_DIR: &str = ".layer-metadata";

/// Reset process-wide layerstack registries for isolated tests.
///
/// Layerstack intentionally keeps lease registries, per-root commit writers,
/// storage-root locks, and auto-squash config process-wide so all daemon
/// runtime instances in one process share the same single-writer and lease
/// view. Call this only from tests when no layerstack operations are live.
#[doc(hidden)]
pub fn reset_process_state_for_tests() {
    service::reset_service_cache_for_tests();
    stack::reset_shared_registries_for_tests();
    lock::reset_storage_lock_registry_for_tests();
}
