//! Process-local advisory writer lock for a layer-stack storage root.
//!
//! # The DUAL-LAYER lease (both layers MUST be reproduced)
//!
//! 1. **Cross-process advisory lease** — `flock(fd, LOCK_EX | LOCK_NB)` on the
//!    `.storage-writer.lock` file (`O_RDWR | O_CREAT, 0o644`). Prevents a second
//!    daemon *process* from owning the same root; a contended acquire returns
//!    [`crate::LayerStackError::StorageRootOwned`] rather than blocking. Released
//!    with `LOCK_UN` + `close` once the in-process refcount hits zero.
//!    `// PORT backend/src/sandbox/layer_stack/storage_lock.py:71,55,69`
//! 2. **In-process reentrant mutex + refcount** — a per-root registry keyed by
//!    the canonical absolute path serializes multiple in-process `LayerStack`
//!    managers that may coexist after cache drops / overlay resets. The mutex is
//!    a **reentrant** `threading.RLock` in Python.
//!    `// PORT backend/src/sandbox/layer_stack/storage_lock.py:13,14,22,78`
//!
//! # ⚠ THE REENTRANT-RLock → non-reentrant-Mutex DEADLOCK TRAP
//!
//! Python holds a `threading.RLock` (REENTRANT). The same thread re-acquires it
//! via `.exclusive()` while it already holds it — e.g. `LayerStack.squash`
//! takes `_storage_write_guard()` (the RLock) and then `self._lock` (a SECOND
//! RLock), and `release_lease` is called *inside* `squash`'s `finally` while the
//! write guard is still held. A naive 1:1 port to `std::sync::Mutex`
//! (NON-reentrant) **DEADLOCKS** on the second same-thread acquire.
//!
//! Do NOT 1:1-port. The future implementer must either (a) restructure the
//! re-entrant sections so re-entry is impossible (thread the already-acquired
//! guard through the call graph instead of re-locking), or (b) use a reentrant
//! guard type. This is a `todo!()` skeleton for now.
//! `// PORT backend/src/sandbox/layer_stack/transaction.py:45`
//! `// PORT backend/src/sandbox/layer_stack/stack.py:365`

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::marker::PhantomData;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::ThreadId;

use rustix::fs::{flock, FlockOperation};

use crate::error::LayerStackError;

/// Lock-file name placed at the root of every storage root.
/// `// PORT backend/src/sandbox/layer_stack/storage_lock.py:13`
pub const STORAGE_WRITER_LOCK_FILE: &str = ".storage-writer.lock";

/// A held cross-process + in-process writer lease for one storage root.
///
/// RAII: dropping the last lease for a root releases the `flock` and closes the
/// fd (refcount-gated). `exclusive()` returns the reentrant in-process guard.
/// `// PORT backend/src/sandbox/layer_stack/storage_lock.py:25-43 — StorageWriterLockLease`
#[derive(Debug)]
pub struct StorageWriterLockLease {
    key: String,
}

impl StorageWriterLockLease {
    /// Acquire (or refcount-bump) the dual-layer writer lease for `storage_root`.
    ///
    /// Fails with [`LayerStackError::StorageRootOwned`] if another process holds
    /// the `flock`. The registry key is the canonicalized absolute path.
    /// `// PORT backend/src/sandbox/layer_stack/storage_lock.py:59-84 — acquire_storage_writer_lock`
    pub fn acquire(storage_root: &Path) -> Result<Self, LayerStackError> {
        std::fs::create_dir_all(storage_root)?;
        let key = storage_root
            .canonicalize()
            .unwrap_or_else(|_| storage_root.to_path_buf())
            .to_string_lossy()
            .into_owned();
        let mut registry = registry().lock().expect("storage lock registry poisoned");
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
                mutex: Arc::new(ReentrantMutex::default()),
            },
        );
        Ok(Self { key })
    }

    /// Enter the in-process exclusive (reentrant) write guard for this root.
    ///
    /// See the module-level DEADLOCK TRAP: the returned guard must tolerate
    /// same-thread re-entry (Python `threading.RLock`).
    /// `// PORT backend/src/sandbox/layer_stack/storage_lock.py:33-40 — exclusive`
    pub fn exclusive(&self) -> Result<ExclusiveGuard<'_>, LayerStackError> {
        let lock = {
            let registry = registry().lock().expect("storage lock registry poisoned");
            registry
                .get(&self.key)
                .map(|record| record.mutex.clone())
                .ok_or(LayerStackError::StorageWriterLockClosed)?
        };
        lock.lock();
        Ok(ExclusiveGuard {
            lock,
            _lease: PhantomData,
        })
    }
}

impl Drop for StorageWriterLockLease {
    fn drop(&mut self) {
        let mut registry = registry().lock().expect("storage lock registry poisoned");
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

/// In-process exclusive write guard. Reentrant on the same thread (see TRAP).
/// `// PORT backend/src/sandbox/layer_stack/storage_lock.py:33-40`
#[derive(Debug)]
pub struct ExclusiveGuard<'lease> {
    lock: Arc<ReentrantMutex>,
    _lease: PhantomData<&'lease StorageWriterLockLease>,
}

impl Drop for ExclusiveGuard<'_> {
    fn drop(&mut self) {
        self.lock.unlock();
    }
}

#[derive(Debug)]
struct LockRecord {
    file: File,
    refcount: usize,
    mutex: Arc<ReentrantMutex>,
}

#[derive(Debug, Default)]
struct ReentrantMutex {
    state: Mutex<ReentrantState>,
    waiters: Condvar,
}

#[derive(Debug, Default)]
struct ReentrantState {
    owner: Option<ThreadId>,
    depth: usize,
}

impl ReentrantMutex {
    fn lock(&self) {
        let current = std::thread::current().id();
        let mut state = self.state.lock().expect("storage root mutex poisoned");
        loop {
            match state.owner {
                None => {
                    state.owner = Some(current);
                    state.depth = 1;
                    return;
                }
                Some(owner) if owner == current => {
                    state.depth += 1;
                    return;
                }
                Some(_) => {
                    state = self
                        .waiters
                        .wait(state)
                        .expect("storage root mutex poisoned while waiting");
                }
            }
        }
    }

    fn unlock(&self) {
        let current = std::thread::current().id();
        let mut state = self.state.lock().expect("storage root mutex poisoned");
        if state.owner != Some(current) {
            return;
        }
        state.depth = state.depth.saturating_sub(1);
        if state.depth == 0 {
            state.owner = None;
            self.waiters.notify_one();
        }
    }
}

fn registry() -> &'static Mutex<HashMap<String, LockRecord>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, LockRecord>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}
