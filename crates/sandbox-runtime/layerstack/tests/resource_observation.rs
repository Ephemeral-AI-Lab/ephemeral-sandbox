use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sandbox_runtime_layerstack::service::{
    NativeRouteCounters, StorageAuthority, StorageRolloutMode,
};
use sandbox_runtime_layerstack::{
    build_workspace_base, LayerChange, LayerPath, LayerStack, ACTIVE_MANIFEST_FILE,
};

mod error {
    pub(crate) use sandbox_runtime_layerstack::LayerStackError;
}

#[allow(dead_code)]
mod fs {
    use std::path::Path;

    pub(crate) fn canonical_key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
}

mod service {
    pub(crate) use sandbox_runtime_layerstack::service::{
        LayerStackResourceSnapshot, LayerStackRouteSnapshot, NativeRouteCounters, StorageAuthority,
        StorageRolloutMode, WriterLockMetrics,
    };
}

#[path = "../src/stack/observation.rs"]
#[allow(dead_code)]
mod observation_impl;

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "layerstack-resource-observation-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace)?;
        build_workspace_base(&root, &workspace, false)?;
        Ok(Self {
            base,
            root,
            workspace,
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn legacy_route_and_owned_resources_are_bounded_and_released(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("lifecycle")?;
    let mut stack = LayerStack::open(fixture.root.clone())?;

    let initial = stack.observe()?;
    assert_eq!(initial.route.schema_version, 1);
    assert_eq!(initial.resources.schema_version, 1);
    assert_eq!(
        initial.route.observation_epoch,
        initial.resources.observation_epoch
    );
    assert_eq!(initial.route.configured_mode, StorageRolloutMode::Legacy);
    assert_eq!(initial.route.write_authority, StorageAuthority::LegacyV1);
    assert_eq!(initial.route.read_authority, StorageAuthority::LegacyV1);
    assert_eq!(initial.route.fallback_count, 0);
    assert!(initial.route.fallback_reason_counts.is_empty());
    assert_eq!(initial.route.mismatch_count, 0);
    assert_eq!(initial.route.shadow_comparison_count, 0);
    assert_eq!(initial.route.shadow_completed_count, 0);
    assert_eq!(initial.route.native_route, NativeRouteCounters::default());
    assert!(initial.resources.logical_cleanup_complete);
    assert_eq!(initial.resources.open_file_descriptors, Some(0));
    assert_eq!(initial.resources.mapped_bytes, Some(0));

    stack.publish_layer(&[LayerChange::Write {
        path: LayerPath::parse("evidence.txt")?,
        content: b"abc".to_vec(),
    }])?;
    let published = stack.observe()?;
    assert!(published.route.observation_epoch > initial.route.observation_epoch);
    assert_eq!(published.route.bytes_scanned, 3);
    assert_eq!(published.route.bytes_hashed, 3);
    assert_eq!(published.route.bytes_written, 3);
    assert_eq!(published.route.bytes_newly_retained, 3);
    assert_eq!(published.resources.active_operations, 0);
    assert_eq!(published.resources.active_publications, 0);
    assert_eq!(published.resources.open_transactions, 0);
    assert_eq!(published.resources.staging_owners, 0);
    assert!(published.resources.high_water_active_operations >= 1);
    assert!(published.resources.high_water_active_publications >= 1);
    assert!(published.resources.high_water_open_transactions >= 1);
    assert!(published.resources.high_water_staging_owners >= 1);
    assert!(published.resources.logical_cleanup_complete);

    assert_eq!(
        stack.read_bytes("evidence.txt")?,
        (Some(b"abc".to_vec()), true)
    );
    let read = stack.observe()?;
    assert_eq!(read.route.bytes_scanned, 6);
    assert_eq!(read.route.bytes_read, 3);

    let lease = stack.acquire_snapshot("resource-observation")?;
    let leased = stack.observe()?;
    assert_eq!(leased.resources.active_leases, 1);
    assert_eq!(leased.resources.registry_entries, 1);
    assert!(leased.resources.high_water_active_leases >= 1);
    assert!(!leased.resources.logical_cleanup_complete);
    assert_eq!(leased.resources.quiescence_ms, None);

    assert!(stack.release_lease(&lease.lease_id)?);
    let released = stack.observe()?;
    assert_eq!(released.resources.active_leases, 0);
    assert_eq!(released.resources.registry_entries, 0);
    assert!(released.resources.high_water_active_leases >= 1);
    assert!(released.resources.logical_cleanup_complete);
    assert!(released.resources.quiescence_ms.is_some());
    assert_eq!(
        released.route.last_quiescence_epoch,
        released.route.observation_epoch
    );
    assert!(std::fs::read_dir(fixture.root.join("staging"))?
        .next()
        .is_none());
    assert!(fixture.workspace.exists());
    Ok(())
}

#[test]
fn materialization_resource_guards_report_exact_high_water_and_settle() {
    let state = Arc::new(observation_impl::StorageObservationState::default());
    let observation = observation_impl::HiddenValidationObservation::new(Arc::clone(&state));

    let owner = observation.begin_materialization_owner();
    let waiter = observation.begin_materialization_waiter();
    let mut target = observation.begin_materialization_target(64 * 1024, 16);
    target.reserve_workspace(2 * 1024 * 1024);

    let (_, active) = state.observe_with_writer_lock(0, service::WriterLockMetrics::default());
    assert_eq!(active.active_operations, 1);
    assert_eq!(active.materialization_owners, 1);
    assert_eq!(active.materialization_waiters, 1);
    assert_eq!(active.materialization_targets, 1);
    assert_eq!(active.materialization_byte_reservations, 64 * 1024);
    assert_eq!(active.materialization_workspace_bytes, 2 * 1024 * 1024);
    assert_eq!(active.open_file_descriptors, Some(16));
    assert_eq!(active.mapped_bytes, Some(0));
    assert!(!active.logical_cleanup_complete);

    drop(target);
    drop(waiter);
    drop(owner);

    let (_, settled) = state.observe_with_writer_lock(0, service::WriterLockMetrics::default());
    assert_eq!(settled.active_operations, 0);
    assert_eq!(settled.materialization_owners, 0);
    assert_eq!(settled.materialization_waiters, 0);
    assert_eq!(settled.materialization_targets, 0);
    assert_eq!(settled.materialization_byte_reservations, 0);
    assert_eq!(settled.materialization_workspace_bytes, 0);
    assert_eq!(settled.open_file_descriptors, Some(0));
    assert_eq!(settled.high_water_active_operations, 1);
    assert_eq!(settled.high_water_materialization_owners, 1);
    assert_eq!(settled.high_water_materialization_waiters, 1);
    assert_eq!(settled.high_water_materialization_targets, 1);
    assert_eq!(
        settled.high_water_materialization_byte_reservations,
        64 * 1024
    );
    assert_eq!(
        settled.high_water_materialization_workspace_bytes,
        2 * 1024 * 1024
    );
    assert_eq!(settled.high_water_open_file_descriptors, Some(16));
    assert_eq!(settled.high_water_mapped_bytes, Some(0));
    assert!(settled.logical_cleanup_complete);
}

#[test]
fn observation_failure_is_read_only() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = Fixture::new("failure-read-only")?;
    let stack = LayerStack::open(fixture.root.clone())?;
    let manifest_path = fixture.root.join(ACTIVE_MANIFEST_FILE);
    std::fs::write(&manifest_path, b"{not-json")?;
    let before = std::fs::read(&manifest_path)?;

    assert!(stack.observe().is_err());

    assert_eq!(std::fs::read(manifest_path)?, before);
    assert!(std::fs::read_dir(fixture.root.join("staging"))?
        .next()
        .is_none());
    Ok(())
}

#[test]
fn observation_counters_saturate_without_wrapping() {
    let state = observation_impl::StorageObservationState::default();
    let counter = AtomicU64::new(u64::MAX - 1);

    state.add(&counter, 4);
    let (route, resources) =
        state.observe_with_writer_lock(0, service::WriterLockMetrics::default());

    assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    assert!(route.counter_saturated);
    assert!(resources.counter_saturated);
}

#[test]
fn native_route_progress_and_forbidden_work_are_accounted_separately() {
    let state = Arc::new(observation_impl::StorageObservationState::default());
    let observation = observation_impl::HiddenValidationObservation::new(Arc::clone(&state));
    observation.configure(StorageRolloutMode::StrictCandidate);

    observation.record_native_lookup_validation();
    observation.record_native_lookup_validation();
    observation.record_native_admission();
    observation.record_native_mount();
    let (warm, _) = state.observe_with_writer_lock(1, service::WriterLockMetrics::default());

    assert_eq!(warm.configured_mode, StorageRolloutMode::StrictCandidate);
    assert_eq!(warm.write_authority, StorageAuthority::LegacyV1);
    assert_eq!(warm.read_authority, StorageAuthority::LegacyV1);
    assert_eq!(warm.native_route.lookup_count, 2);
    assert_eq!(warm.native_route.validation_count, 2);
    assert_eq!(warm.native_route.admission_count, 1);
    assert_eq!(warm.native_route.mount_count, 1);
    assert_eq!(warm.native_route.cdc_count, 0);
    assert_eq!(warm.native_route.object_traversal_count, 0);
    assert_eq!(warm.native_route.hash_count, 0);
    assert_eq!(warm.native_route.locator_merge_count, 0);
    assert_eq!(warm.native_route.compaction_count, 0);
    assert_eq!(warm.native_route.pack_count, 0);
    assert_eq!(warm.native_route.gc_count, 0);
    assert_eq!(warm.native_route.squash_count, 0);
    assert_eq!(warm.native_route.materialization_count, 0);
    assert_eq!(warm.native_route.fallback_count, 0);

    observation.record_native_materialization();
    let (cold, _) = state.observe_with_writer_lock(2, service::WriterLockMetrics::default());
    assert_eq!(cold.native_route.materialization_count, 1);
    assert_eq!(cold.native_route.object_traversal_count, 1);
    assert_eq!(cold.native_route.hash_count, 1);

    observation.record_native_fallback();
    let (fallback, _) = state.observe_with_writer_lock(3, service::WriterLockMetrics::default());
    assert_eq!(fallback.native_route.fallback_count, 1);
    assert_eq!(fallback.fallback_count, 1);
}

#[test]
fn shared_observation_registry_does_not_retain_dead_roots(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    observation_impl::reset_shared_observation_states_for_tests();
    let fixture = Fixture::new("shared-registry")?;
    let first = observation_impl::shared_observation_state_for_root(&fixture.root)?;
    let second = observation_impl::shared_observation_state_for_root(&fixture.root)?;
    assert!(Arc::ptr_eq(&first, &second));

    let weak = Arc::downgrade(&first);
    drop(first);
    drop(second);
    assert!(weak.upgrade().is_none());

    let replacement = observation_impl::shared_observation_state_for_root(&fixture.root)?;
    assert!(weak.upgrade().is_none());
    drop(replacement);
    observation_impl::reset_shared_observation_states_for_tests();
    Ok(())
}
