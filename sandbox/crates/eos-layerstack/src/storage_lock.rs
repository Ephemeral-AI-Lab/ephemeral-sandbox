//! Process-local advisory writer lock for a layer-stack storage root.
//!
//! # The DUAL-LAYER lease (both layers MUST be reproduced)
//!
//! 1. **Cross-process advisory lease** — `flock(fd, LOCK_EX | LOCK_NB)` on the
//!    `.storage-writer.lock` file (`O_RDWR | O_CREAT, 0o644`). Prevents a second
//!    daemon *process* from owning the same root; a contended acquire returns
//!    [`crate::LayerStackError::StorageRootOwned`] rather than blocking. Released
//!    with `LOCK_UN` + `close` once the in-process refcount hits zero.
//! 2. **In-process shared/exclusive lock + refcount** — a per-root registry keyed
//!    by the canonical absolute path coordinates multiple in-process
//!    `LayerStack` managers that may coexist after cache drops / overlay resets.
//!    Snapshot reads can share the lock; storage mutations take the reentrant
//!    exclusive side. Rust used a process-local `threading.RLock` for
//!    snapshots and a separate storage-writer guard for mutations.
//!
//! # ⚠ THE REENTRANT-RLock → non-reentrant-Mutex DEADLOCK TRAP
//!
//! Rust holds a `threading.RLock` (REENTRANT). The same thread re-acquires it
//! via `.exclusive()` while it already holds it — e.g. `LayerStack.squash`
//! takes `_storage_write_guard()` (the `RLock`) and then `self._lock` (a SECOND
//! `RLock`), and `release_lease` is called *inside* `squash`'s `finally` while the
//! write guard is still held. A naive 1:1 port to `std::sync::Mutex`
//! (NON-reentrant) **DEADLOCKS** on the second same-thread acquire.
//!
//! Do NOT 1:1-port. This module uses a small reentrant read/write guard type that
//! preserves the Rust same-thread write re-entry semantics without holding an
//! async lock across awaits.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::marker::PhantomData;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::ThreadId;

use rustix::fs::{flock, FlockOperation};

use crate::error::LayerStackError;

/// Lock-file name placed at the root of every storage root.
pub const STORAGE_WRITER_LOCK_FILE: &str = ".storage-writer.lock";

/// A held cross-process + in-process writer lease for one storage root.
///
/// RAII: dropping the last lease for a root releases the `flock` and closes the
/// fd (refcount-gated). `shared()` returns the in-process read guard;
/// `exclusive()` returns the reentrant in-process write guard.
#[derive(Debug)]
pub struct StorageWriterLockLease {
    key: String,
}

impl StorageWriterLockLease {
    /// Acquire (or refcount-bump) the dual-layer writer lease for `storage_root`.
    ///
    /// Fails with [`LayerStackError::StorageRootOwned`] if another process holds
    /// the `flock`. The registry key is the canonicalized absolute path.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the storage root cannot be created,
    /// canonicalized/locked, or when the process-local registry is poisoned.
    pub fn acquire(storage_root: &Path) -> Result<Self, LayerStackError> {
        std::fs::create_dir_all(storage_root)?;
        let key = storage_root
            .canonicalize()
            .unwrap_or_else(|_| storage_root.to_path_buf())
            .to_string_lossy()
            .into_owned();
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
                file,
                refcount: 1,
                lock: Arc::new(ReentrantRwLock::default()),
            },
        );
        drop(registry);
        Ok(Self { key })
    }

    /// Enter the in-process shared read guard for this root.
    ///
    /// Multiple shared guards can coexist. An active writer excludes new readers,
    /// and pending writers block new readers so write-side maintenance is not
    /// starved by a stream of snapshots.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the registry or lock state is poisoned, or
    /// when this lease has already been closed.
    pub fn shared(&self) -> Result<SharedGuard<'_>, LayerStackError> {
        let lock = self.lock()?;
        lock.read()?;
        Ok(SharedGuard {
            lock,
            _lease: PhantomData,
        })
    }

    /// Enter the in-process exclusive (reentrant) write guard for this root.
    ///
    /// See the module-level DEADLOCK TRAP: the returned guard must tolerate
    /// same-thread re-entry (a reentrant write lock).
    ///
    /// CAUTION: re-entry is write-over-write only. Calling `exclusive()` while
    /// the same thread still holds a [`SharedGuard`] for this root self-deadlocks
    /// (the writer waits for `readers == 0`, which includes its own read). Drop
    /// the shared guard before acquiring the exclusive one.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the registry or reentrant lock state is
    /// poisoned, or when this lease has already been closed.
    pub fn exclusive(&self) -> Result<ExclusiveGuard<'_>, LayerStackError> {
        let lock = self.lock()?;
        lock.write()?;
        Ok(ExclusiveGuard {
            lock,
            _lease: PhantomData,
        })
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
        if let Some(record) = registry.remove(&self.key) {
            let _ = flock(&record.file, FlockOperation::Unlock);
        }
    }
}

/// In-process shared read guard.
#[derive(Debug)]
pub struct SharedGuard<'lease> {
    lock: Arc<ReentrantRwLock>,
    _lease: PhantomData<&'lease StorageWriterLockLease>,
}

impl Drop for SharedGuard<'_> {
    fn drop(&mut self) {
        self.lock.read_unlock();
    }
}

/// In-process exclusive write guard. Reentrant on the same thread (see TRAP).
#[derive(Debug)]
pub struct ExclusiveGuard<'lease> {
    lock: Arc<ReentrantRwLock>,
    _lease: PhantomData<&'lease StorageWriterLockLease>,
}

impl Drop for ExclusiveGuard<'_> {
    fn drop(&mut self) {
        self.lock.write_unlock();
    }
}

#[derive(Debug)]
struct LockRecord {
    file: File,
    refcount: usize,
    lock: Arc<ReentrantRwLock>,
}

#[derive(Debug, Default)]
struct ReentrantRwLock {
    state: Mutex<ReentrantRwState>,
    waiters: Condvar,
}

#[derive(Debug, Default)]
struct ReentrantRwState {
    writer: Option<ThreadId>,
    write_depth: usize,
    readers: usize,
    waiting_writers: usize,
}

impl ReentrantRwLock {
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

fn registry() -> &'static Mutex<HashMap<String, LockRecord>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, LockRecord>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_registry() -> Result<MutexGuard<'static, HashMap<String, LockRecord>>, LayerStackError> {
    registry()
        .lock()
        .map_err(|_| LayerStackError::LockPoisoned("storage lock registry"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::StorageWriterLockLease;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn shared_guards_overlap_and_block_exclusive() -> TestResult {
        let fixture = Fixture::new("shared-overlap")?;
        let lease = StorageWriterLockLease::acquire(&fixture.root)?;
        let shared = lease.shared()?;

        let (shared_tx, shared_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let root = fixture.root.clone();
        let shared_thread = std::thread::spawn(move || -> TestResult {
            let lease = StorageWriterLockLease::acquire(&root)?;
            let _shared = lease.shared()?;
            shared_tx.send(())?;
            release_rx.recv()?;
            Ok(())
        });
        shared_rx.recv_timeout(Duration::from_secs(1))?;
        release_tx.send(())?;
        join_test_thread(shared_thread)?;

        let (exclusive_tx, exclusive_rx) = mpsc::channel();
        let root = fixture.root.clone();
        let exclusive_thread = std::thread::spawn(move || -> TestResult {
            let lease = StorageWriterLockLease::acquire(&root)?;
            let _exclusive = lease.exclusive()?;
            exclusive_tx.send(())?;
            Ok(())
        });
        assert!(
            exclusive_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "exclusive guard acquired while a shared guard was still held"
        );
        drop(shared);
        exclusive_rx.recv_timeout(Duration::from_secs(1))?;
        join_test_thread(exclusive_thread)?;
        Ok(())
    }

    #[test]
    fn exclusive_guard_is_reentrant_and_blocks_shared() -> TestResult {
        let fixture = Fixture::new("exclusive-reentrant")?;
        let lease = StorageWriterLockLease::acquire(&fixture.root)?;
        let exclusive = lease.exclusive()?;
        let nested = lease.exclusive()?;

        let (shared_tx, shared_rx) = mpsc::channel();
        let root = fixture.root.clone();
        let shared_thread = std::thread::spawn(move || -> TestResult {
            let lease = StorageWriterLockLease::acquire(&root)?;
            let _shared = lease.shared()?;
            shared_tx.send(())?;
            Ok(())
        });
        assert!(
            shared_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "shared guard acquired while the outer exclusive guard was held"
        );
        drop(nested);
        assert!(
            shared_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "shared guard acquired while the reentrant exclusive guard was still held"
        );
        drop(exclusive);
        shared_rx.recv_timeout(Duration::from_secs(1))?;
        join_test_thread(shared_thread)?;
        Ok(())
    }

    fn join_test_thread(handle: std::thread::JoinHandle<TestResult>) -> TestResult {
        handle
            .join()
            .map_err(|_| std::io::Error::other("test thread panicked"))?
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> TestResult<Self> {
            let root = std::env::temp_dir().join(format!(
                "eos-layerstack-storage-lock-{label}-{}-{}",
                std::process::id(),
                NEXT_TMP.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root)?;
            Ok(Self { root })
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
