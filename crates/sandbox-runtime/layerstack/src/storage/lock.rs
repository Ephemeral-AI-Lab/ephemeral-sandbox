use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::marker::PhantomData;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::ThreadId;
use std::time::Instant;

#[cfg(not(windows))]
use rustix::fs::{flock, FlockOperation};

use crate::error::LayerStackError;
use crate::service::WriterLockMetrics;

pub(crate) const STORAGE_WRITER_LOCK_FILE: &str = ".storage-writer.lock";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WriterLockForbiddenWork {
    #[cfg(target_os = "linux")]
    TreeWalk,
    #[cfg(target_os = "linux")]
    PayloadVerification,
    HistoryScan,
    PermitOrFlightWait,
    WorkerJoin,
    Cleanup,
    #[cfg(target_os = "linux")]
    ProviderPayloadIo,
}

#[derive(Debug)]
pub(crate) struct StorageWriterLockLease {
    key: String,
}

impl StorageWriterLockLease {
    pub(crate) fn acquire(storage_root: &Path) -> Result<Self, LayerStackError> {
        std::fs::create_dir_all(storage_root)?;
        let key = crate::fs::canonical_key(storage_root);
        let mut registry = lock_registry()?;
        if let Some(record) = registry.get_mut(&key) {
            record.refcount += 1;
            return Ok(Self { key });
        }

        let lock_path = storage_root.join(STORAGE_WRITER_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        #[cfg(not(windows))]
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(LayerStackError::StorageRootOwned(
                    storage_root.display().to_string(),
                ));
            }
            Err(err) => return Err(LayerStackError::Io(err.into())),
        }

        registry.insert(
            key.clone(),
            LockRecord {
                _file: file,
                refcount: 1,
                lock: Arc::new(ReentrantRwLock::default()),
            },
        );
        drop(registry);
        Ok(Self { key })
    }

    pub(crate) fn shared(&self) -> Result<SharedGuard<'_>, LayerStackError> {
        let lock = self.lock()?;
        lock.read()?;
        Ok(SharedGuard {
            lock,
            _lease: PhantomData,
        })
    }

    pub(crate) fn exclusive(&self) -> Result<ExclusiveGuard<'_>, LayerStackError> {
        let lock = self.lock()?;
        let wait_started = Instant::now();
        lock.write()?;
        lock.record_wait(wait_started.elapsed());
        HELD_WRITER_LOCKS.with(|held| held.borrow_mut().push(Arc::clone(&lock)));
        Ok(ExclusiveGuard {
            lock,
            hold_started: Instant::now(),
            _lease: PhantomData,
        })
    }

    pub(crate) fn metrics(&self) -> Result<WriterLockMetrics, LayerStackError> {
        Ok(self.lock()?.metrics())
    }

    fn lock(&self) -> Result<Arc<ReentrantRwLock>, LayerStackError> {
        let registry = lock_registry()?;
        registry
            .get(&self.key)
            .map(|record| record.lock.clone())
            .ok_or(LayerStackError::StorageWriterLockClosed)
    }
}

impl Drop for StorageWriterLockLease {
    fn drop(&mut self) {
        let mut registry = registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(record) = registry.get_mut(&self.key) else {
            return;
        };
        record.refcount = record.refcount.saturating_sub(1);
        if record.refcount > 0 {
            return;
        }
        if let Some(_record) = registry.remove(&self.key) {
            #[cfg(not(windows))]
            let _ = flock(&_record._file, FlockOperation::Unlock);
        }
    }
}

#[derive(Debug)]
pub(crate) struct SharedGuard<'lease> {
    lock: Arc<ReentrantRwLock>,
    _lease: PhantomData<&'lease StorageWriterLockLease>,
}

impl Drop for SharedGuard<'_> {
    fn drop(&mut self) {
        self.lock.read_unlock();
    }
}

#[derive(Debug)]
pub(crate) struct ExclusiveGuard<'lease> {
    lock: Arc<ReentrantRwLock>,
    hold_started: Instant,
    _lease: PhantomData<&'lease StorageWriterLockLease>,
}

impl Drop for ExclusiveGuard<'_> {
    fn drop(&mut self) {
        HELD_WRITER_LOCKS.with(|held| {
            let popped = held.borrow_mut().pop();
            debug_assert!(
                popped
                    .as_ref()
                    .is_some_and(|lock| Arc::ptr_eq(lock, &self.lock)),
                "storage writer lock rank stack is corrupt"
            );
        });
        self.lock.record_hold(self.hold_started.elapsed());
        self.lock.write_unlock();
    }
}

#[derive(Debug)]
struct LockRecord {
    _file: File,
    refcount: usize,
    lock: Arc<ReentrantRwLock>,
}

#[derive(Debug, Default)]
struct ReentrantRwLock {
    state: Mutex<ReentrantRwState>,
    waiters: Condvar,
    acquisitions: AtomicU64,
    wait_ns: AtomicU64,
    maximum_wait_ns: AtomicU64,
    hold_ns: AtomicU64,
    maximum_hold_ns: AtomicU64,
    forbidden_tree_walks: AtomicU64,
    forbidden_payload_verifications: AtomicU64,
    forbidden_history_scans: AtomicU64,
    forbidden_permit_or_flight_waits: AtomicU64,
    forbidden_worker_joins: AtomicU64,
    forbidden_cleanups: AtomicU64,
    forbidden_provider_payload_io: AtomicU64,
}

#[derive(Debug, Default)]
struct ReentrantRwState {
    writer: Option<ThreadId>,
    write_depth: usize,
    readers: usize,
    waiting_writers: usize,
}

impl ReentrantRwLock {
    fn record_wait(&self, elapsed: std::time::Duration) {
        let elapsed = duration_ns(elapsed);
        saturating_add(&self.acquisitions, 1);
        saturating_add(&self.wait_ns, elapsed);
        update_maximum(&self.maximum_wait_ns, elapsed);
    }

    fn record_hold(&self, elapsed: std::time::Duration) {
        let elapsed = duration_ns(elapsed);
        saturating_add(&self.hold_ns, elapsed);
        update_maximum(&self.maximum_hold_ns, elapsed);
    }

    fn record_forbidden(&self, class: WriterLockForbiddenWork) {
        let counter = match class {
            #[cfg(target_os = "linux")]
            WriterLockForbiddenWork::TreeWalk => &self.forbidden_tree_walks,
            #[cfg(target_os = "linux")]
            WriterLockForbiddenWork::PayloadVerification => &self.forbidden_payload_verifications,
            WriterLockForbiddenWork::HistoryScan => &self.forbidden_history_scans,
            WriterLockForbiddenWork::PermitOrFlightWait => &self.forbidden_permit_or_flight_waits,
            WriterLockForbiddenWork::WorkerJoin => &self.forbidden_worker_joins,
            WriterLockForbiddenWork::Cleanup => &self.forbidden_cleanups,
            #[cfg(target_os = "linux")]
            WriterLockForbiddenWork::ProviderPayloadIo => &self.forbidden_provider_payload_io,
        };
        saturating_add(counter, 1);
    }

    fn metrics(&self) -> WriterLockMetrics {
        WriterLockMetrics {
            acquisitions: self.acquisitions.load(Ordering::Relaxed),
            wait_ns: self.wait_ns.load(Ordering::Relaxed),
            maximum_wait_ns: self.maximum_wait_ns.load(Ordering::Relaxed),
            hold_ns: self.hold_ns.load(Ordering::Relaxed),
            maximum_hold_ns: self.maximum_hold_ns.load(Ordering::Relaxed),
            forbidden_tree_walks: self.forbidden_tree_walks.load(Ordering::Relaxed),
            forbidden_payload_verifications: self
                .forbidden_payload_verifications
                .load(Ordering::Relaxed),
            forbidden_history_scans: self.forbidden_history_scans.load(Ordering::Relaxed),
            forbidden_permit_or_flight_waits: self
                .forbidden_permit_or_flight_waits
                .load(Ordering::Relaxed),
            forbidden_worker_joins: self.forbidden_worker_joins.load(Ordering::Relaxed),
            forbidden_cleanups: self.forbidden_cleanups.load(Ordering::Relaxed),
            forbidden_provider_payload_io: self
                .forbidden_provider_payload_io
                .load(Ordering::Relaxed),
        }
    }

    fn read(&self) -> Result<(), LayerStackError> {
        let current = std::thread::current().id();
        let mut state = self
            .state
            .lock()
            .map_err(|_| LayerStackError::LockPoisoned("storage root lock"))?;
        loop {
            let writer_is_self = state.writer == Some(current);
            if writer_is_self || (state.writer.is_none() && state.waiting_writers == 0) {
                state.readers += 1;
                return Ok(());
            }
            state = self
                .waiters
                .wait(state)
                .map_err(|_| LayerStackError::LockPoisoned("storage root read wait"))?;
        }
    }

    fn read_unlock(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.readers = state.readers.saturating_sub(1);
        if state.readers == 0 {
            drop(state);
            self.waiters.notify_all();
        }
    }

    fn write(&self) -> Result<(), LayerStackError> {
        let current = std::thread::current().id();
        let mut state = self
            .state
            .lock()
            .map_err(|_| LayerStackError::LockPoisoned("storage root lock"))?;
        if state.writer == Some(current) {
            state.write_depth += 1;
            return Ok(());
        }
        state.waiting_writers += 1;
        loop {
            if state.writer.is_none() && state.readers == 0 {
                state.waiting_writers = state.waiting_writers.saturating_sub(1);
                state.writer = Some(current);
                state.write_depth = 1;
                return Ok(());
            }
            state = match self.waiters.wait(state) {
                Ok(state) => state,
                Err(err) => {
                    let mut state = err.into_inner();
                    state.waiting_writers = state.waiting_writers.saturating_sub(1);
                    return Err(LayerStackError::LockPoisoned("storage root write wait"));
                }
            };
        }
    }

    fn write_unlock(&self) {
        let current = std::thread::current().id();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.writer != Some(current) {
            return;
        }
        state.write_depth = state.write_depth.saturating_sub(1);
        if state.write_depth == 0 {
            state.writer = None;
            drop(state);
            self.waiters.notify_all();
        }
    }
}

thread_local! {
    static HELD_WRITER_LOCKS: RefCell<Vec<Arc<ReentrantRwLock>>> = const {
        RefCell::new(Vec::new())
    };
}

pub(crate) fn assert_writer_lock_allows(class: WriterLockForbiddenWork) {
    let held = HELD_WRITER_LOCKS.with(|held| held.borrow().last().cloned());
    let Some(lock) = held else {
        return;
    };
    lock.record_forbidden(class);
    debug_assert!(
        false,
        "forbidden {class:?} attempted while holding the storage writer lock"
    );
}

fn duration_ns(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn saturating_add(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        Some(value.saturating_add(amount))
    });
}

fn update_maximum(counter: &AtomicU64, candidate: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        (candidate > current).then_some(candidate)
    });
}

fn registry() -> &'static Mutex<HashMap<String, LockRecord>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, LockRecord>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_registry() -> Result<MutexGuard<'static, HashMap<String, LockRecord>>, LayerStackError> {
    registry()
        .lock()
        .map_err(|_| LayerStackError::LockPoisoned("storage lock registry"))
}

pub(crate) fn reset_storage_lock_registry_for_tests() {
    let mut registry = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let inactive_keys = registry
        .iter()
        .filter(|(_, record)| record.refcount == 0)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in inactive_keys {
        let Some(_record) = registry.remove(&key) else {
            continue;
        };
        #[cfg(not(windows))]
        let _ = flock(&_record._file, FlockOperation::Unlock);
    }
}
