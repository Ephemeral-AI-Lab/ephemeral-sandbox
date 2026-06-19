use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::commit::{CommitError, CommitOptions, CommitWriter};

pub(crate) type RootService = Arc<CommitWriter>;

pub(crate) const SERVICE_CACHE_MAX: usize = 256;

#[derive(Default)]
pub(crate) struct ServiceCache {
    pub(crate) entries: HashMap<String, RootService>,
    lru: VecDeque<String>,
}

impl ServiceCache {
    fn get(&mut self, key: &str) -> Option<RootService> {
        let service = self.entries.get(key)?.clone();
        self.touch(key);
        Some(service)
    }

    pub(crate) fn insert_or_get(&mut self, key: String, service: RootService) -> RootService {
        if let Some(existing) = self.entries.get(&key).cloned() {
            self.touch(&key);
            return existing;
        }
        self.lru.push_back(key.clone());
        self.entries.insert(key, service.clone());
        self.evict_oldest();
        service
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.lru.iter().position(|entry| entry == key) {
            self.lru.remove(position);
        }
        self.lru.push_back(key.to_owned());
    }

    fn evict_oldest(&mut self) {
        while self.entries.len() > SERVICE_CACHE_MAX {
            let Some(key) = self.lru.pop_front() else {
                break;
            };
            self.entries.remove(&key);
        }
    }
}

pub(crate) fn services() -> &'static Mutex<ServiceCache> {
    static SERVICES: OnceLock<Mutex<ServiceCache>> = OnceLock::new();
    SERVICES.get_or_init(|| Mutex::new(ServiceCache::default()))
}

fn lock_services() -> Result<MutexGuard<'static, ServiceCache>, CommitError> {
    services()
        .lock()
        .map_err(|_| CommitError::QueueStatePoisoned("per-root service registry"))
}

pub(crate) fn reset_service_cache_for_tests() {
    let mut cache = services()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *cache = ServiceCache::default();
}

pub(crate) fn service_for_root(
    root: &Path,
    options: CommitOptions,
) -> Result<RootService, CommitError> {
    let options = CommitOptions::new(options.auto_squash_max_depth);
    let key = service_cache_key(root, options);
    {
        let mut cache = lock_services()?;
        if let Some(service) = cache.get(&key) {
            return Ok(service);
        }
    }
    let service = Arc::new(CommitWriter::with_options(root.to_path_buf(), options)?);
    let mut cache = lock_services()?;
    Ok(cache.insert_or_get(key, service))
}

pub(crate) fn normalize_root_key(root: &Path) -> String {
    crate::fs::canonical_key(root)
}

fn service_cache_key(root: &Path, options: CommitOptions) -> String {
    format!(
        "{}|auto_squash_max_depth={}",
        normalize_root_key(root),
        options.auto_squash_max_depth
    )
}
