#![forbid(unsafe_code)]

#[path = "../src/error.rs"]
mod error;
#[path = "../src/storage/fs.rs"]
pub(crate) mod fs_impl;
// Syscall-recording shim (tests-only): locally-defined items shadow the glob
// re-export, so every `crate::fs::syncfs_storage_root` call from the
// re-included source tree lands here and is observed before delegating.
pub(crate) mod fs {
    use std::path::Path;
    use std::sync::Mutex;

    pub(crate) use super::fs_impl::*;

    #[derive(Debug, Clone)]
    pub(crate) struct SyncfsCall {
        pub(crate) manifest_version_at_call: i64,
        pub(crate) promoted_s_dirs: Vec<String>,
    }

    pub(crate) static SYNCFS_CALLS: Mutex<Vec<SyncfsCall>> = Mutex::new(Vec::new());

    pub(crate) fn syncfs_storage_root(storage_root: &Path) -> Result<(), crate::LayerStackError> {
        let manifest_version_at_call =
            super::fs_impl::read_manifest(storage_root.join(crate::ACTIVE_MANIFEST_FILE))
                .map(|manifest| manifest.version)
                .unwrap_or(-1);
        let promoted_s_dirs = std::fs::read_dir(storage_root.join(crate::LAYERS_DIR))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with('S'))
                    .collect()
            })
            .unwrap_or_default();
        SYNCFS_CALLS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(SyncfsCall {
                manifest_version_at_call,
                promoted_s_dirs,
            });
        super::fs_impl::syncfs_storage_root(storage_root)
    }
}
#[path = "../src/storage/lock.rs"]
pub(crate) mod lock;
#[path = "../src/model/mod.rs"]
mod model;
#[path = "../src/observability.rs"]
mod observability;
#[path = "../src/service/mod.rs"]
pub mod service;
#[path = "../src/stack/mod.rs"]
mod stack;
#[path = "../src/storage/whiteout.rs"]
mod whiteout;
#[path = "../src/workspace_base/mod.rs"]
mod workspace_base;

pub use error::LayerStackError;
pub use model::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, CasError, LayerChange, LayerPath,
    LayerRef, Manifest, MANIFEST_SCHEMA_VERSION,
};
pub use stack::{LayerStack, Lease, MergedView};
pub use workspace_base::{
    build_shared_workspace_base, build_workspace_base, ensure_workspace_base,
    read_workspace_binding, require_workspace_binding, SharedWorkspaceBase, WorkspaceBinding,
    SHARED_BASE_DIR, WORKSPACE_BASE_LAYER_ID, WORKSPACE_BINDING_FILE,
};

pub(crate) const LAYERS_DIR: &str = "layers";
pub(crate) const STAGING_DIR: &str = "staging";
pub const ACTIVE_MANIFEST_FILE: &str = "manifest.json";
pub(crate) const LAYER_METADATA_DIR: &str = ".layer-metadata";

pub fn reset_process_state_for_tests() {
    stack::reset_shared_registries_for_tests();
    lock::reset_storage_lock_registry_for_tests();
}

pub(crate) fn process_state_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) use lock::*;
pub(crate) use model::*;

#[path = "unit/test_fixture.rs"]
mod test_fixture;

#[path = "unit/merge.rs"]
mod merge_tests;
#[path = "unit/model.rs"]
mod model_tests;
#[path = "unit/observe.rs"]
mod observe_tests;
#[path = "unit/publish.rs"]
mod publish_tests;
#[path = "unit/service.rs"]
mod service_tests;
#[path = "unit/sidecar.rs"]
mod sidecar_tests;
#[path = "unit/squash.rs"]
mod squash_tests;
#[path = "unit/stack.rs"]
mod stack_tests;
#[path = "unit/storage_lock.rs"]
mod storage_lock_tests;
