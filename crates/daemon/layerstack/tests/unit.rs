#![forbid(unsafe_code)]

#[path = "../src/commit/mod.rs"]
mod commit;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/fs.rs"]
pub(crate) mod fs;
#[path = "../src/lease_aware.rs"]
mod lease_aware;
#[path = "../src/lock.rs"]
pub(crate) mod lock;
#[path = "../src/model.rs"]
mod model;
#[path = "../src/service.rs"]
pub mod service;
#[path = "../src/squash.rs"]
mod squash;
#[path = "../src/stack/mod.rs"]
mod stack;
#[path = "../src/whiteout.rs"]
mod whiteout;
#[path = "../src/workspace.rs"]
mod workspace;

pub(crate) use commit::{
    CaptureRouteStats, ChangesetResult, CommitError, CommitOptions, CommitStatus,
};
pub use error::LayerStackError;
pub(crate) use lease_aware::{
    plan_lease_aware_gaps, LeaseAwareCheckpointMode, LeaseAwarePlan, LeaseAwarePlanEntry,
};
pub use model::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, CasError, LayerChange, LayerPath,
    LayerRef, Manifest, MANIFEST_SCHEMA_VERSION,
};
pub use stack::{LayerStack, Lease, MergedView, SquashOutcome};
pub use workspace::{
    build_workspace_base, ensure_workspace_base, read_workspace_binding, require_workspace_binding,
    WorkspaceBinding, WORKSPACE_BINDING_FILE,
};

pub(crate) const AUTO_SQUASH_MAX_DEPTH: usize = 100;
pub(crate) const LAYERS_DIR: &str = "layers";
pub(crate) const STAGING_DIR: &str = "staging";
pub const ACTIVE_MANIFEST_FILE: &str = "manifest.json";
pub(crate) const LAYER_METADATA_DIR: &str = ".layer-metadata";

#[doc(hidden)]
pub fn reset_process_state_for_tests() {
    service::reset_service_cache_for_tests();
    stack::reset_shared_registries_for_tests();
    lock::reset_storage_lock_registry_for_tests();
}

pub(crate) fn process_state_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) use commit::worker::auto_squash::*;
pub(crate) use commit::worker::queue::*;
pub(crate) use commit::worker::transaction::*;
pub(crate) use commit::*;
pub(crate) use lock::*;
pub(crate) use model::*;
pub(crate) use service::*;
pub(crate) use squash::*;
pub(crate) use stack::*;

#[path = "unit/test_fixture.rs"]
mod test_fixture;

#[path = "unit/commit/queue.rs"]
mod commit_queue_tests;
#[path = "unit/commit/transaction.rs"]
mod commit_transaction_tests;
#[path = "unit/lease_aware.rs"]
mod lease_aware_tests;
#[path = "unit/model.rs"]
mod model_tests;
#[path = "unit/route.rs"]
mod route_tests;
#[path = "unit/service.rs"]
mod service_tests;
#[path = "unit/squash.rs"]
mod squash_tests;
#[path = "unit/stack.rs"]
mod stack_tests;
#[path = "unit/storage_lock.rs"]
mod storage_lock_tests;
