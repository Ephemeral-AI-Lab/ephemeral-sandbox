mod cleanup;
mod registry;
pub(crate) mod rewrite;

pub use cleanup::SweepReport;
pub(in crate::stack) use cleanup::{release_lease_locked, sweep_storage_locked};
pub(crate) use registry::reset_shared_registries_for_tests;
pub(crate) use registry::{lock_shared_registry, LeaseRegistry};
pub(in crate::stack) use registry::{lock_shared_registry_recover, shared_registry_for_root};
pub(crate) use rewrite::reset_shared_substitutions_for_tests;
pub use rewrite::RewrittenLease;
