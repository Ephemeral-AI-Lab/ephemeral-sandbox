use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;

use crate::error::LayerStackError;
use crate::fs::canonical_key;
use crate::service::{
    LayerStackResourceSnapshot, LayerStackRouteSnapshot, NativeRouteCounters, StorageAuthority,
    StorageRolloutMode, WriterLockMetrics,
};

const OBSERVATION_SCHEMA_VERSION: u16 = 1;
const MODE_LEGACY: u64 = 0;
const MODE_VALIDATION: u64 = 1;
const MODE_STRICT_CANDIDATE: u64 = 2;

#[derive(Debug, Default)]
pub(crate) struct StorageObservationState {
    observation_epoch: AtomicU64,
    last_quiescence_epoch: AtomicU64,
    configured_mode: AtomicU64,
    fallback_count: AtomicU64,
    mismatch_count: AtomicU64,
    shadow_comparison_count: AtomicU64,
    shadow_completed_count: AtomicU64,
    bytes_scanned: AtomicU64,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    bytes_hashed: AtomicU64,
    bytes_reused: AtomicU64,
    bytes_newly_retained: AtomicU64,
    native_lookup_count: AtomicU64,
    native_validation_count: AtomicU64,
    native_admission_count: AtomicU64,
    native_mount_count: AtomicU64,
    native_cdc_count: AtomicU64,
    native_object_traversal_count: AtomicU64,
    native_hash_count: AtomicU64,
    native_locator_merge_count: AtomicU64,
    native_compaction_count: AtomicU64,
    native_pack_count: AtomicU64,
    native_gc_count: AtomicU64,
    native_squash_count: AtomicU64,
    native_materialization_count: AtomicU64,
    native_fallback_count: AtomicU64,
    active_operations: AtomicU64,
    high_water_active_operations: AtomicU64,
    active_publications: AtomicU64,
    high_water_active_publications: AtomicU64,
    active_buffers: AtomicU64,
    high_water_active_buffers: AtomicU64,
    active_tasks: AtomicU64,
    high_water_active_tasks: AtomicU64,
    active_workers: AtomicU64,
    high_water_active_workers: AtomicU64,
    queued_items: AtomicU64,
    high_water_queued_items: AtomicU64,
    queued_bytes: AtomicU64,
    high_water_queued_bytes: AtomicU64,
    byte_permits_in_use: AtomicU64,
    high_water_byte_permits_in_use: AtomicU64,
    open_transactions: AtomicU64,
    high_water_open_transactions: AtomicU64,
    staging_owners: AtomicU64,
    high_water_staging_owners: AtomicU64,
    materialization_owners: AtomicU64,
    high_water_materialization_owners: AtomicU64,
    materialization_waiters: AtomicU64,
    high_water_materialization_waiters: AtomicU64,
    materialization_targets: AtomicU64,
    high_water_materialization_targets: AtomicU64,
    materialization_byte_reservations: AtomicU64,
    high_water_materialization_byte_reservations: AtomicU64,
    materialization_workspace_bytes: AtomicU64,
    high_water_materialization_workspace_bytes: AtomicU64,
    materialization_fd_permits: AtomicU64,
    high_water_materialization_fd_permits: AtomicU64,
    high_water_active_leases: AtomicU64,
    quiescent_since_ms: AtomicU64,
    counter_saturated: AtomicBool,
}

pub(crate) struct StorageOperationGuard {
    state: Arc<StorageObservationState>,
    publication: bool,
    transaction: bool,
    staging: bool,
}

/// Private Stage 03 hidden-validation accounting handle.
///
/// The operation service owns the worker and queue, while LayerStack owns the
/// stable observation schema. This handle keeps that ownership boundary narrow.
#[doc(hidden)]
#[derive(Clone)]
pub struct HiddenValidationObservation {
    state: Arc<StorageObservationState>,
}

#[doc(hidden)]
pub struct HiddenQueuedWork {
    state: Arc<StorageObservationState>,
    bytes: u64,
    active: bool,
}

#[doc(hidden)]
pub struct HiddenTaskWork {
    state: Arc<StorageObservationState>,
    bytes: u64,
}

#[doc(hidden)]
pub struct HiddenWorkerGuard {
    state: Arc<StorageObservationState>,
}

#[doc(hidden)]
pub struct HiddenMaterializationOwnerGuard {
    state: Arc<StorageObservationState>,
    operation: Option<StorageOperationGuard>,
}

#[doc(hidden)]
pub struct HiddenMaterializationWaiterGuard {
    state: Arc<StorageObservationState>,
}

#[doc(hidden)]
pub struct HiddenMaterializationTargetGuard {
    state: Arc<StorageObservationState>,
    byte_reservation: u64,
    fd_permits: u64,
    workspace_bytes: u64,
}

impl StorageObservationState {
    pub(crate) fn begin_operation(
        self: &Arc<Self>,
        publication: bool,
        transaction: bool,
    ) -> StorageOperationGuard {
        self.quiescent_since_ms.store(0, Ordering::Relaxed);
        self.increment(&self.active_operations, &self.high_water_active_operations);
        if publication {
            self.increment(
                &self.active_publications,
                &self.high_water_active_publications,
            );
        }
        if transaction {
            self.increment(&self.open_transactions, &self.high_water_open_transactions);
        }
        StorageOperationGuard {
            state: Arc::clone(self),
            publication,
            transaction,
            staging: false,
        }
    }

    pub(crate) fn record_active_leases(&self, active_leases: usize) {
        let active_leases = u64::try_from(active_leases).unwrap_or(u64::MAX);
        if active_leases > 0 {
            self.quiescent_since_ms.store(0, Ordering::Relaxed);
        }
        self.update_high_water(&self.high_water_active_leases, active_leases);
    }

    pub(crate) fn observe_with_writer_lock(
        &self,
        active_leases: usize,
        writer_lock: WriterLockMetrics,
    ) -> (LayerStackRouteSnapshot, LayerStackResourceSnapshot) {
        let epoch = self.saturating_increment(&self.observation_epoch);
        let active_leases = u64::try_from(active_leases).unwrap_or(u64::MAX);
        self.update_high_water(&self.high_water_active_leases, active_leases);

        let active_operations = self.active_operations.load(Ordering::Relaxed);
        let active_publications = self.active_publications.load(Ordering::Relaxed);
        let active_buffers = self.active_buffers.load(Ordering::Relaxed);
        let active_tasks = self.active_tasks.load(Ordering::Relaxed);
        let active_workers = self.active_workers.load(Ordering::Relaxed);
        let queued_items = self.queued_items.load(Ordering::Relaxed);
        let queued_bytes = self.queued_bytes.load(Ordering::Relaxed);
        let byte_permits_in_use = self.byte_permits_in_use.load(Ordering::Relaxed);
        let open_transactions = self.open_transactions.load(Ordering::Relaxed);
        let staging_owners = self.staging_owners.load(Ordering::Relaxed);
        let materialization_owners = self.materialization_owners.load(Ordering::Relaxed);
        let materialization_waiters = self.materialization_waiters.load(Ordering::Relaxed);
        let materialization_targets = self.materialization_targets.load(Ordering::Relaxed);
        let materialization_byte_reservations = self
            .materialization_byte_reservations
            .load(Ordering::Relaxed);
        let materialization_workspace_bytes =
            self.materialization_workspace_bytes.load(Ordering::Relaxed);
        let materialization_fd_permits = self.materialization_fd_permits.load(Ordering::Relaxed);
        let logical_cleanup_complete = active_operations == 0
            && active_publications == 0
            && active_buffers == 0
            && active_tasks == 0
            && active_workers == 0
            && queued_items == 0
            && queued_bytes == 0
            && byte_permits_in_use == 0
            && open_transactions == 0
            && staging_owners == 0
            && materialization_owners == 0
            && materialization_waiters == 0
            && materialization_targets == 0
            && materialization_byte_reservations == 0
            && materialization_workspace_bytes == 0
            && materialization_fd_permits == 0
            && active_leases == 0;
        let quiescence_ms = if logical_cleanup_complete {
            let now = process_elapsed_ms();
            let encoded_now = now.saturating_add(1);
            if self
                .quiescent_since_ms
                .compare_exchange(0, encoded_now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                self.last_quiescence_epoch.store(epoch, Ordering::Relaxed);
            }
            let started = self.quiescent_since_ms.load(Ordering::Relaxed);
            Some(now.saturating_sub(started.saturating_sub(1)))
        } else {
            self.quiescent_since_ms.store(0, Ordering::Relaxed);
            None
        };

        let state_bytes = u64::try_from(std::mem::size_of::<Self>()).unwrap_or(u64::MAX);
        let owned_bytes = state_bytes.saturating_add(byte_permits_in_use);
        let high_water_owned_bytes =
            state_bytes.saturating_add(self.high_water_byte_permits_in_use.load(Ordering::Relaxed));
        let resource = LayerStackResourceSnapshot {
            schema_version: OBSERVATION_SCHEMA_VERSION,
            observation_epoch: epoch,
            live_owned_bytes: owned_bytes,
            high_water_owned_bytes,
            active_operations: narrow(active_operations),
            high_water_active_operations: narrow(
                self.high_water_active_operations.load(Ordering::Relaxed),
            ),
            active_publications: narrow(active_publications),
            high_water_active_publications: narrow(
                self.high_water_active_publications.load(Ordering::Relaxed),
            ),
            active_buffers: narrow(active_buffers),
            high_water_active_buffers: narrow(
                self.high_water_active_buffers.load(Ordering::Relaxed),
            ),
            active_tasks: narrow(active_tasks),
            high_water_active_tasks: narrow(self.high_water_active_tasks.load(Ordering::Relaxed)),
            active_workers: narrow(active_workers),
            high_water_active_workers: narrow(
                self.high_water_active_workers.load(Ordering::Relaxed),
            ),
            queued_items: narrow(queued_items),
            high_water_queued_items: narrow(self.high_water_queued_items.load(Ordering::Relaxed)),
            queued_bytes,
            high_water_queued_bytes: self.high_water_queued_bytes.load(Ordering::Relaxed),
            byte_permits_in_use,
            high_water_byte_permits_in_use: self
                .high_water_byte_permits_in_use
                .load(Ordering::Relaxed),
            active_leases: narrow(active_leases),
            high_water_active_leases: narrow(self.high_water_active_leases.load(Ordering::Relaxed)),
            open_transactions: narrow(open_transactions),
            high_water_open_transactions: narrow(
                self.high_water_open_transactions.load(Ordering::Relaxed),
            ),
            staging_owners: narrow(staging_owners),
            high_water_staging_owners: narrow(
                self.high_water_staging_owners.load(Ordering::Relaxed),
            ),
            cache_entries: 0,
            high_water_cache_entries: 0,
            registry_entries: narrow(active_leases),
            high_water_registry_entries: narrow(
                self.high_water_active_leases.load(Ordering::Relaxed),
            ),
            materialization_owners: narrow(materialization_owners),
            high_water_materialization_owners: narrow(
                self.high_water_materialization_owners
                    .load(Ordering::Relaxed),
            ),
            materialization_waiters: narrow(materialization_waiters),
            high_water_materialization_waiters: narrow(
                self.high_water_materialization_waiters
                    .load(Ordering::Relaxed),
            ),
            materialization_targets: narrow(materialization_targets),
            high_water_materialization_targets: narrow(
                self.high_water_materialization_targets
                    .load(Ordering::Relaxed),
            ),
            materialization_byte_reservations,
            high_water_materialization_byte_reservations: self
                .high_water_materialization_byte_reservations
                .load(Ordering::Relaxed),
            materialization_workspace_bytes,
            high_water_materialization_workspace_bytes: self
                .high_water_materialization_workspace_bytes
                .load(Ordering::Relaxed),
            open_file_descriptors: Some(narrow(materialization_fd_permits)),
            high_water_open_file_descriptors: Some(narrow(
                self.high_water_materialization_fd_permits
                    .load(Ordering::Relaxed),
            )),
            mapped_bytes: Some(0),
            high_water_mapped_bytes: Some(0),
            writer_lock_acquisitions: writer_lock.acquisitions,
            writer_lock_wait_ns: writer_lock.wait_ns,
            writer_lock_maximum_wait_ns: writer_lock.maximum_wait_ns,
            writer_lock_hold_ns: writer_lock.hold_ns,
            writer_lock_maximum_hold_ns: writer_lock.maximum_hold_ns,
            writer_lock_forbidden_tree_walks: writer_lock.forbidden_tree_walks,
            writer_lock_forbidden_payload_verifications: writer_lock
                .forbidden_payload_verifications,
            writer_lock_forbidden_history_scans: writer_lock.forbidden_history_scans,
            writer_lock_forbidden_permit_or_flight_waits: writer_lock
                .forbidden_permit_or_flight_waits,
            writer_lock_forbidden_worker_joins: writer_lock.forbidden_worker_joins,
            writer_lock_forbidden_cleanups: writer_lock.forbidden_cleanups,
            writer_lock_forbidden_provider_payload_io: writer_lock.forbidden_provider_payload_io,
            logical_cleanup_complete,
            quiescence_ms,
            counter_saturated: self.counter_saturated.load(Ordering::Relaxed)
                || active_operations > u64::from(u32::MAX)
                || active_publications > u64::from(u32::MAX)
                || active_buffers > u64::from(u32::MAX)
                || active_tasks > u64::from(u32::MAX)
                || active_workers > u64::from(u32::MAX)
                || queued_items > u64::from(u32::MAX)
                || open_transactions > u64::from(u32::MAX)
                || staging_owners > u64::from(u32::MAX)
                || materialization_owners > u64::from(u32::MAX)
                || materialization_waiters > u64::from(u32::MAX)
                || materialization_targets > u64::from(u32::MAX)
                || materialization_fd_permits > u64::from(u32::MAX)
                || active_leases > u64::from(u32::MAX),
        };
        let shadow_comparison_count = self.shadow_comparison_count.load(Ordering::Relaxed);
        let route = LayerStackRouteSnapshot {
            schema_version: OBSERVATION_SCHEMA_VERSION,
            observation_epoch: epoch,
            configured_mode: match self.configured_mode.load(Ordering::Relaxed) {
                MODE_LEGACY => StorageRolloutMode::Legacy,
                MODE_VALIDATION => StorageRolloutMode::Validation,
                MODE_STRICT_CANDIDATE => StorageRolloutMode::StrictCandidate,
                _ => StorageRolloutMode::Legacy,
            },
            write_authority: StorageAuthority::LegacyV1,
            read_authority: StorageAuthority::LegacyV1,
            fallback_count: self.fallback_count.load(Ordering::Relaxed),
            fallback_reason_counts: [],
            mismatch_count: self.mismatch_count.load(Ordering::Relaxed),
            shadow_comparison_count,
            shadow_completed_count: self.shadow_completed_count.load(Ordering::Relaxed),
            bytes_scanned: self.bytes_scanned.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            bytes_hashed: self.bytes_hashed.load(Ordering::Relaxed),
            bytes_reused: self.bytes_reused.load(Ordering::Relaxed),
            bytes_newly_retained: self.bytes_newly_retained.load(Ordering::Relaxed),
            native_route: NativeRouteCounters {
                lookup_count: self.native_lookup_count.load(Ordering::Relaxed),
                validation_count: self.native_validation_count.load(Ordering::Relaxed),
                admission_count: self.native_admission_count.load(Ordering::Relaxed),
                mount_count: self.native_mount_count.load(Ordering::Relaxed),
                cdc_count: self.native_cdc_count.load(Ordering::Relaxed),
                object_traversal_count: self.native_object_traversal_count.load(Ordering::Relaxed),
                hash_count: self.native_hash_count.load(Ordering::Relaxed),
                locator_merge_count: self.native_locator_merge_count.load(Ordering::Relaxed),
                compaction_count: self.native_compaction_count.load(Ordering::Relaxed),
                pack_count: self.native_pack_count.load(Ordering::Relaxed),
                gc_count: self.native_gc_count.load(Ordering::Relaxed),
                squash_count: self.native_squash_count.load(Ordering::Relaxed),
                materialization_count: self.native_materialization_count.load(Ordering::Relaxed),
                fallback_count: self.native_fallback_count.load(Ordering::Relaxed),
            },
            last_quiescence_epoch: self.last_quiescence_epoch.load(Ordering::Relaxed),
            counter_saturated: self.counter_saturated.load(Ordering::Relaxed),
        };
        (route, resource)
    }

    pub(crate) fn record_read(&self, bytes: u64) {
        self.add(&self.bytes_scanned, bytes);
        self.add(&self.bytes_read, bytes);
    }

    pub(crate) fn record_hashed(&self, bytes: u64) {
        self.add(&self.bytes_scanned, bytes);
        self.add(&self.bytes_hashed, bytes);
    }

    pub(crate) fn record_reused(&self, bytes: u64) {
        self.add(&self.bytes_reused, bytes);
    }

    pub(crate) fn record_committed(&self, bytes: u64) {
        self.add(&self.bytes_written, bytes);
        self.add(&self.bytes_newly_retained, bytes);
    }

    fn increment(&self, current: &AtomicU64, high_water: &AtomicU64) {
        let value = self.saturating_increment(current);
        self.update_high_water(high_water, value);
    }

    pub(crate) fn add(&self, value: &AtomicU64, amount: u64) {
        let previous = value
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(amount))
            })
            .unwrap_or_else(|current| current);
        if previous.checked_add(amount).is_none() {
            self.counter_saturated.store(true, Ordering::Relaxed);
        }
    }

    fn saturating_increment(&self, value: &AtomicU64) -> u64 {
        let previous = value
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(1))
            })
            .unwrap_or_else(|current| current);
        if previous == u64::MAX {
            self.counter_saturated.store(true, Ordering::Relaxed);
            u64::MAX
        } else {
            previous + 1
        }
    }

    fn update_high_water(&self, high_water: &AtomicU64, candidate: u64) {
        let _ = high_water.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            (candidate > current).then_some(candidate)
        });
    }
}

impl HiddenValidationObservation {
    pub(crate) fn new(state: Arc<StorageObservationState>) -> Self {
        Self { state }
    }

    /// Return the in-memory route counters without reopening or traversing the
    /// active stack. Callers that need manifest and lease topology must use the
    /// full stack observation instead.
    #[doc(hidden)]
    #[must_use]
    pub fn observe_route(&self, active_leases: usize) -> LayerStackRouteSnapshot {
        self.state
            .observe_with_writer_lock(active_leases, WriterLockMetrics::default())
            .0
    }

    pub fn configure(&self, mode: StorageRolloutMode) {
        let mode = match mode {
            StorageRolloutMode::Legacy => MODE_LEGACY,
            StorageRolloutMode::Validation => MODE_VALIDATION,
            StorageRolloutMode::StrictCandidate => MODE_STRICT_CANDIDATE,
        };
        self.state.configured_mode.store(mode, Ordering::Relaxed);
    }

    #[must_use]
    pub fn begin_materialization_owner(&self) -> HiddenMaterializationOwnerGuard {
        self.state.increment(
            &self.state.materialization_owners,
            &self.state.high_water_materialization_owners,
        );
        HiddenMaterializationOwnerGuard {
            state: Arc::clone(&self.state),
            operation: Some(self.state.begin_operation(false, false)),
        }
    }

    #[must_use]
    pub fn begin_materialization_waiter(&self) -> HiddenMaterializationWaiterGuard {
        self.state.increment(
            &self.state.materialization_waiters,
            &self.state.high_water_materialization_waiters,
        );
        HiddenMaterializationWaiterGuard {
            state: Arc::clone(&self.state),
        }
    }

    #[must_use]
    pub fn begin_materialization_target(
        &self,
        byte_reservation: usize,
        fd_permits: usize,
    ) -> HiddenMaterializationTargetGuard {
        let byte_reservation = u64::try_from(byte_reservation).unwrap_or(u64::MAX);
        let fd_permits = u64::try_from(fd_permits).unwrap_or(u64::MAX);
        self.state.increment(
            &self.state.materialization_targets,
            &self.state.high_water_materialization_targets,
        );
        self.state.add(
            &self.state.materialization_byte_reservations,
            byte_reservation,
        );
        self.state.update_high_water(
            &self.state.high_water_materialization_byte_reservations,
            self.state
                .materialization_byte_reservations
                .load(Ordering::Relaxed),
        );
        self.state
            .add(&self.state.materialization_fd_permits, fd_permits);
        self.state.update_high_water(
            &self.state.high_water_materialization_fd_permits,
            self.state
                .materialization_fd_permits
                .load(Ordering::Relaxed),
        );
        HiddenMaterializationTargetGuard {
            state: Arc::clone(&self.state),
            byte_reservation,
            fd_permits,
            workspace_bytes: 0,
        }
    }

    #[must_use]
    pub fn begin_worker(&self) -> HiddenWorkerGuard {
        self.state.increment(
            &self.state.active_workers,
            &self.state.high_water_active_workers,
        );
        HiddenWorkerGuard {
            state: Arc::clone(&self.state),
        }
    }

    #[must_use]
    pub fn enqueue(&self, bytes: u64) -> HiddenQueuedWork {
        self.state.increment(
            &self.state.active_buffers,
            &self.state.high_water_active_buffers,
        );
        self.state.increment(
            &self.state.queued_items,
            &self.state.high_water_queued_items,
        );
        self.state.add(&self.state.queued_bytes, bytes);
        self.state.update_high_water(
            &self.state.high_water_queued_bytes,
            self.state.queued_bytes.load(Ordering::Relaxed),
        );
        self.state.add(&self.state.byte_permits_in_use, bytes);
        self.state.update_high_water(
            &self.state.high_water_byte_permits_in_use,
            self.state.byte_permits_in_use.load(Ordering::Relaxed),
        );
        HiddenQueuedWork {
            state: Arc::clone(&self.state),
            bytes,
            active: true,
        }
    }

    pub fn record_completion(&self, matched: bool) {
        let _ = self
            .state
            .saturating_increment(&self.state.shadow_completed_count);
        let _ = self
            .state
            .saturating_increment(&self.state.shadow_comparison_count);
        if !matched {
            let _ = self.state.saturating_increment(&self.state.mismatch_count);
        }
    }

    pub fn record_fallback(&self) {
        let _ = self.state.saturating_increment(&self.state.fallback_count);
    }

    /// Record one plan-only lookup and validation attempt on the warm native
    /// route. This does not imply logical-object or carrier-content work.
    pub fn record_native_lookup_validation(&self) {
        let _ = self
            .state
            .saturating_increment(&self.state.native_lookup_count);
        let _ = self
            .state
            .saturating_increment(&self.state.native_validation_count);
    }

    /// Record a durable exact-generation lease admitted for a session.
    pub fn record_native_admission(&self) {
        let _ = self
            .state
            .saturating_increment(&self.state.native_admission_count);
    }

    /// Record a successfully created native-provider mount.
    pub fn record_native_mount(&self) {
        let _ = self
            .state
            .saturating_increment(&self.state.native_mount_count);
    }

    /// Record entry into the explicit cold materialization path.
    ///
    /// Cold reconstruction traverses logical objects and verifies native
    /// bytes. Those work classes are counted alongside the cold operation so
    /// their absence on warm-route deltas is meaningful.
    pub fn record_native_materialization(&self) {
        let _ = self
            .state
            .saturating_increment(&self.state.native_materialization_count);
        let _ = self
            .state
            .saturating_increment(&self.state.native_object_traversal_count);
        let _ = self
            .state
            .saturating_increment(&self.state.native_hash_count);
    }

    /// Record an actual fallback from the strict native route.
    pub fn record_native_fallback(&self) {
        let _ = self.state.saturating_increment(&self.state.fallback_count);
        let _ = self
            .state
            .saturating_increment(&self.state.native_fallback_count);
    }
}

impl HiddenQueuedWork {
    #[must_use]
    pub fn start(mut self) -> HiddenTaskWork {
        decrement(&self.state.queued_items);
        subtract(&self.state.queued_bytes, self.bytes);
        self.state.increment(
            &self.state.active_tasks,
            &self.state.high_water_active_tasks,
        );
        self.active = false;
        HiddenTaskWork {
            state: Arc::clone(&self.state),
            bytes: self.bytes,
        }
    }
}

impl Drop for HiddenQueuedWork {
    fn drop(&mut self) {
        if self.active {
            decrement(&self.state.queued_items);
            subtract(&self.state.queued_bytes, self.bytes);
            decrement(&self.state.active_buffers);
            subtract(&self.state.byte_permits_in_use, self.bytes);
        }
    }
}

impl Drop for HiddenTaskWork {
    fn drop(&mut self) {
        decrement(&self.state.active_tasks);
        decrement(&self.state.active_buffers);
        subtract(&self.state.byte_permits_in_use, self.bytes);
    }
}

impl Drop for HiddenWorkerGuard {
    fn drop(&mut self) {
        decrement(&self.state.active_workers);
    }
}

impl Drop for HiddenMaterializationOwnerGuard {
    fn drop(&mut self) {
        decrement(&self.state.materialization_owners);
        drop(self.operation.take());
    }
}

impl Drop for HiddenMaterializationWaiterGuard {
    fn drop(&mut self) {
        decrement(&self.state.materialization_waiters);
    }
}

impl HiddenMaterializationTargetGuard {
    pub fn reserve_workspace(&mut self, workspace_bytes: u64) {
        debug_assert_eq!(self.workspace_bytes, 0);
        self.state
            .add(&self.state.materialization_workspace_bytes, workspace_bytes);
        self.state.update_high_water(
            &self.state.high_water_materialization_workspace_bytes,
            self.state
                .materialization_workspace_bytes
                .load(Ordering::Relaxed),
        );
        self.workspace_bytes = workspace_bytes;
    }
}

impl Drop for HiddenMaterializationTargetGuard {
    fn drop(&mut self) {
        decrement(&self.state.materialization_targets);
        subtract(
            &self.state.materialization_byte_reservations,
            self.byte_reservation,
        );
        subtract(&self.state.materialization_fd_permits, self.fd_permits);
        subtract(
            &self.state.materialization_workspace_bytes,
            self.workspace_bytes,
        );
    }
}

impl StorageOperationGuard {
    pub(crate) fn state(&self) -> &StorageObservationState {
        &self.state
    }

    pub(crate) fn mark_staging(&mut self) {
        if self.staging {
            return;
        }
        self.state.increment(
            &self.state.staging_owners,
            &self.state.high_water_staging_owners,
        );
        self.staging = true;
    }
}

impl Drop for StorageOperationGuard {
    fn drop(&mut self) {
        decrement(&self.state.active_operations);
        if self.publication {
            decrement(&self.state.active_publications);
        }
        if self.transaction {
            decrement(&self.state.open_transactions);
        }
        if self.staging {
            decrement(&self.state.staging_owners);
        }
    }
}

pub(crate) fn shared_observation_state_for_root(
    storage_root: &Path,
) -> Result<Arc<StorageObservationState>, LayerStackError> {
    let key = canonical_key(storage_root);
    let mut states = shared_observation_states()
        .lock()
        .map_err(|_| LayerStackError::LockPoisoned("storage observation state map"))?;
    states.retain(|_, state| state.strong_count() > 0);
    if let Some(state) = states.get(&key).and_then(Weak::upgrade) {
        return Ok(state);
    }
    let state = Arc::new(StorageObservationState::default());
    states.insert(key, Arc::downgrade(&state));
    Ok(state)
}

pub(crate) fn reset_shared_observation_states_for_tests() {
    shared_observation_states()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

fn shared_observation_states() -> &'static Mutex<HashMap<String, Weak<StorageObservationState>>> {
    static STATES: OnceLock<Mutex<HashMap<String, Weak<StorageObservationState>>>> =
        OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn decrement(value: &AtomicU64) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(1))
    });
}

fn subtract(value: &AtomicU64, amount: u64) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(amount))
    });
}

fn narrow(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn process_elapsed_ms() -> u64 {
    static STARTED: OnceLock<Instant> = OnceLock::new();
    u64::try_from(STARTED.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}
