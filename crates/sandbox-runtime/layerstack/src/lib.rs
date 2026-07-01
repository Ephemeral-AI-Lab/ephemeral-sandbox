#![forbid(unsafe_code)]

mod error;
mod model;
mod observability;
pub mod service;
mod stack;
mod storage;
mod workspace_base;

pub(crate) use storage::{fs, lock, whiteout};

pub use model::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, published_layer_bytes, CasError,
    LayerChange, LayerPath, LayerRef, Manifest, MANIFEST_SCHEMA_VERSION,
};

pub use error::LayerStackError;
pub use stack::file_read::{AmendCommit, AmendError, ManifestFileRead};
pub use stack::publish::merge::{three_way_merge, LineRange, MergeOutcome, Origin};
pub use stack::publish::model::{
    ContentFingerprint, LayerProtectedDrop, LayerProtectedDropReason, PublishBase,
    PublishBaseRevision, PublishReject, PublishRejectReason, PublishRouteSummary,
    PublishValidatedChangesRequest, PublishValidatedChangesResult, ResolvedChangeset,
    SourceConflict,
};
pub use stack::{LayerStack, Lease, MergedView};
pub use workspace_base::{
    build_shared_workspace_base, build_workspace_base, ensure_workspace_base,
    read_workspace_binding, require_workspace_binding, SharedWorkspaceBase, WorkspaceBinding,
    SHARED_BASE_DIR, WORKSPACE_BASE_LAYER_ID, WORKSPACE_BINDING_FILE,
};

pub const LAYERS_DIR: &str = "layers";

pub const STAGING_DIR: &str = "staging";

pub const ACTIVE_MANIFEST_FILE: &str = "manifest.json";

pub const LAYER_METADATA_DIR: &str = ".layer-metadata";

/// Reset process-wide layerstack registries for isolated tests.
///
/// Layerstack intentionally keeps lease registries, per-root commit writers,
/// and storage-root locks process-wide so all daemon runtime instances in one
/// process share the same single-writer and lease view. Call this only from
/// tests when no layerstack operations are live.
#[doc(hidden)]
pub fn reset_process_state_for_tests() {
    stack::reset_shared_registries_for_tests();
    lock::reset_storage_lock_registry_for_tests();
}
