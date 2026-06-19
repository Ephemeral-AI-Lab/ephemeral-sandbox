#[path = "service/cache.rs"]
mod cache;
#[path = "service/impls/mod.rs"]
mod impls;
#[path = "service/model.rs"]
mod model;
#[path = "service/support.rs"]
mod support;

pub use impls::{
    acquire_snapshot_with_lease, compact_snapshot_layers, get_snapshot,
    publish_changes_to_layerstack, release_lease,
};
pub use model::{
    CompactSnapshotLayersRequest, CompactSnapshotLayersResult, LeasedSnapshot,
    PublishChangesRequest, PublishChangesResult, Snapshot,
};

#[doc(hidden)]
#[allow(unused_imports)]
pub(crate) use cache::{
    normalize_root_key, reset_service_cache_for_tests, services, RootService, ServiceCache,
    SERVICE_CACHE_MAX,
};
#[doc(hidden)]
#[allow(unused_imports)]
pub(crate) use support::{snapshot_manifest, snapshot_manifest_preserving_layer_ids};
