use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::marker::PhantomData;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::ThreadId;

use rustix::fs::{flock, FlockOperation};

use crate::error::LayerStackError;

pub(crate) const STORAGE_WRITER_LOCK_FILE: &str = ".storage-writer.lock";

#[derive(Debug)]
pub(crate) struct StorageWriterLockLease {
    key: String,
}

impl StorageWriterLockLease {
    pub fn acquire(storage_root: &Path) -> Result<Self, LayerStackError> {
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

    pub fn shared(&self) -> Result<SharedGuard<'_>, LayerStackError> {
        let lock = self.lock()?;
        lock.read()?;
        Ok(SharedGuard {
            lock,
            _lease: PhantomData,
        })
    }

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
#[path = "../tests/unit/storage_lock.rs"]
mod tests;
