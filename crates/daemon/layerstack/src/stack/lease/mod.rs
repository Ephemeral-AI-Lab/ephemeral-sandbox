mod cleanup;
mod registry;

pub(in crate::stack) use cleanup::{
    release_lease_locked, remove_unreferenced_layer_candidates_locked, retarget_lease_locked,
};
pub(crate) use registry::reset_shared_registries_for_tests;
pub(in crate::stack) use registry::{
    lock_shared_registry, lock_shared_registry_recover, shared_registry_for_root,
    SharedLeaseRegistry,
};
