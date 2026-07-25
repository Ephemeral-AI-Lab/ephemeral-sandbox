use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;

use crate::error::LayerStackError;
use crate::fs::canonical_key;
use crate::service::{
    LayerStackResourceSnapshot, LayerStackRouteSnapshot, StorageAuthority, StorageRolloutMode,
};

const OBSERVATION_SCHEMA_VERSION: u16 = 1;

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

    pub(crate) fn observe(
        &self,
        active_leases: usize,
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
            open_file_descriptors: None,
            high_water_open_file_descriptors: None,
            mapped_bytes: None,
            high_water_mapped_bytes: None,
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
                || active_leases > u64::from(u32::MAX),
        };
        let shadow_comparison_count = self.shadow_comparison_count.load(Ordering::Relaxed);
        let route = LayerStackRouteSnapshot {
            schema_version: OBSERVATION_SCHEMA_VERSION,
            observation_epoch: epoch,
            configured_mode: if self.configured_mode.load(Ordering::Relaxed) == 0 {
                StorageRolloutMode::Legacy
            } else {
                StorageRolloutMode::Validation
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

    pub fn configure(&self, mode: StorageRolloutMode) {
        self.state.configured_mode.store(
            u64::from(matches!(mode, StorageRolloutMode::Validation)),
            Ordering::Relaxed,
        );
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
