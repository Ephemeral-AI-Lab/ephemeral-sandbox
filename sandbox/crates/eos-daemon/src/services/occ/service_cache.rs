use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use eos_occ::{CommitQueue, OccService};
use serde_json::{json, Value};

use crate::error::DaemonError;
use crate::response_timings::usize_to_f64_saturating;

use super::publish::LayerStackCommitTransaction;
use super::route::LayerStackRouteProvider;

pub(crate) const OCC_SERVICE_CACHE_MAX: usize = 256;

pub(crate) struct OccServiceLookup {
    pub(super) service: Arc<OccService<LayerStackCommitTransaction>>,
    pub(crate) lock_wait_s: f64,
    pub(crate) cache_hit: bool,
    pub(crate) cache_created: bool,
    pub(crate) evicted_count: usize,
    pub(crate) cache_size: usize,
}

impl OccServiceLookup {
    pub(super) fn insert_timings(&self, timings: &mut std::collections::BTreeMap<String, f64>) {
        for (key, value) in [
            ("occ.runtime_service.cache_lock_wait_s", self.lock_wait_s),
            (
                "occ.runtime_service.cache_hit",
                if self.cache_hit { 1.0 } else { 0.0 },
            ),
            (
                "occ.runtime_service.cache_miss",
                if self.cache_hit { 0.0 } else { 1.0 },
            ),
            (
                "occ.runtime_service.cache_created",
                if self.cache_created { 1.0 } else { 0.0 },
            ),
            (
                "occ.runtime_service.cache_reused",
                if self.cache_created { 0.0 } else { 1.0 },
            ),
            (
                "occ.runtime_service.cache_evicted_count",
                usize_to_f64_saturating(self.evicted_count),
            ),
            (
                "occ.runtime_service.cache_size",
                usize_to_f64_saturating(self.cache_size),
            ),
            (
                "occ.runtime_service.cache_capacity",
                usize_to_f64_saturating(OCC_SERVICE_CACHE_MAX),
            ),
        ] {
            timings.entry(key.to_owned()).or_insert(value);
        }
    }
}

#[derive(Default)]
pub(crate) struct OccServiceCacheStats {
    pub(crate) hits_total: u64,
    pub(crate) misses_total: u64,
    pub(crate) creates_total: u64,
    pub(crate) evictions_total: u64,
    pub(crate) lock_wait_s_total: f64,
    pub(crate) lock_wait_s_max: f64,
}

#[derive(Default)]
pub(crate) struct OccServiceCache {
    pub(crate) entries: HashMap<String, Arc<OccService<LayerStackCommitTransaction>>>,
    lru: VecDeque<String>,
    pub(crate) stats: OccServiceCacheStats,
}

impl OccServiceCache {
    fn record_lock_wait(&mut self, lock_wait_s: f64) {
        self.stats.lock_wait_s_total += lock_wait_s;
        self.stats.lock_wait_s_max = self.stats.lock_wait_s_max.max(lock_wait_s);
    }

    fn get(&mut self, key: &str, lock_wait_s: f64) -> Option<OccServiceLookup> {
        self.record_lock_wait(lock_wait_s);
        let service = self.entries.get(key)?.clone();
        self.touch(key);
        self.stats.hits_total += 1;
        Some(OccServiceLookup {
            service,
            lock_wait_s,
            cache_hit: true,
            cache_created: false,
            evicted_count: 0,
            cache_size: self.entries.len(),
        })
    }

    /// Insert `service` for `key`, or return the already-cached service when a
    /// concurrent caller won the race. On a hit the passed-in `service` is handed
    /// back as the second tuple element so the caller can drop it AFTER releasing
    /// the cache lock: its `Drop` closes a commit queue and joins the worker
    /// thread, which must not block the process-wide cache mutex.
    pub(crate) fn insert_or_get(
        &mut self,
        key: String,
        service: Arc<OccService<LayerStackCommitTransaction>>,
        lock_wait_s: f64,
    ) -> (
        OccServiceLookup,
        Option<Arc<OccService<LayerStackCommitTransaction>>>,
    ) {
        self.record_lock_wait(lock_wait_s);
        if let Some(existing) = self.entries.get(&key).cloned() {
            self.touch(&key);
            self.stats.hits_total += 1;
            return (
                OccServiceLookup {
                    service: existing,
                    lock_wait_s,
                    cache_hit: true,
                    cache_created: false,
                    evicted_count: 0,
                    cache_size: self.entries.len(),
                },
                Some(service),
            );
        }
        self.stats.misses_total += 1;
        self.stats.creates_total += 1;
        self.lru.push_back(key.clone());
        self.entries.insert(key, service.clone());
        let evicted_count = self.evict_oldest();
        self.stats.evictions_total = self
            .stats
            .evictions_total
            .saturating_add(u64::try_from(evicted_count).unwrap_or(u64::MAX));
        (
            OccServiceLookup {
                service,
                lock_wait_s,
                cache_hit: false,
                cache_created: true,
                evicted_count,
                cache_size: self.entries.len(),
            },
            None,
        )
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.lru.iter().position(|entry| entry == key) {
            self.lru.remove(position);
        }
        self.lru.push_back(key.to_owned());
    }

    fn evict_oldest(&mut self) -> usize {
        let mut evicted_count = 0;
        while self.entries.len() > OCC_SERVICE_CACHE_MAX {
            let Some(key) = self.lru.pop_front() else {
                break;
            };
            if self.entries.remove(&key).is_some() {
                evicted_count += 1;
            }
        }
        evicted_count
    }
}

pub(super) fn occ_service_for_root(root: &Path) -> Result<OccServiceLookup, DaemonError> {
    let key = normalize_root_key(root);
    let lock_start = Instant::now();
    {
        let mut cache = lock_occ_services()?;
        if let Some(lookup) = cache.get(&key, lock_start.elapsed().as_secs_f64()) {
            return Ok(lookup);
        }
    }
    let transaction = LayerStackCommitTransaction {
        root: root.to_path_buf(),
    };
    let route_provider = Arc::new(LayerStackRouteProvider {
        root: root.to_path_buf(),
    });
    let service = Arc::new(OccService::with_route_provider(
        CommitQueue::new(transaction),
        route_provider,
    )?);
    let lock_start = Instant::now();
    let mut cache = lock_occ_services()?;
    let (lookup, rejected) = cache.insert_or_get(key, service, lock_start.elapsed().as_secs_f64());
    // Release the global cache lock BEFORE dropping the rejected loser: its
    // `OccService::drop` closes the commit queue and joins the worker thread,
    // which must not run while the process-wide cache mutex is held.
    drop(cache);
    drop(rejected);
    Ok(lookup)
}

fn occ_services() -> &'static Mutex<OccServiceCache> {
    static SERVICES: OnceLock<Mutex<OccServiceCache>> = OnceLock::new();
    SERVICES.get_or_init(|| Mutex::new(OccServiceCache::default()))
}

fn lock_occ_services() -> Result<MutexGuard<'static, OccServiceCache>, DaemonError> {
    occ_services()
        .lock()
        .map_err(|_| DaemonError::StateLockPoisoned("occ service registry"))
}

pub(crate) fn normalize_root_key(root: &Path) -> String {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn occ_service_cache_snapshot() -> Value {
    let lock_start = Instant::now();
    let (
        size,
        hits_total,
        misses_total,
        creates_total,
        evictions_total,
        lock_wait_s_total,
        lock_wait_s_max,
        lock_wait_s,
    ) = {
        let mut cache = match lock_occ_services() {
            Ok(cache) => cache,
            Err(err) => {
                return json!({
                    "capacity": OCC_SERVICE_CACHE_MAX,
                    "size": 0,
                    "poisoned": true,
                    "error": err.to_string(),
                });
            }
        };
        let lock_wait_s = lock_start.elapsed().as_secs_f64();
        cache.record_lock_wait(lock_wait_s);
        (
            cache.entries.len(),
            cache.stats.hits_total,
            cache.stats.misses_total,
            cache.stats.creates_total,
            cache.stats.evictions_total,
            cache.stats.lock_wait_s_total,
            cache.stats.lock_wait_s_max,
            lock_wait_s,
        )
    };
    json!({
        "capacity": OCC_SERVICE_CACHE_MAX,
        "size": size,
        "hits_total": hits_total,
        "misses_total": misses_total,
        "creates_total": creates_total,
        "evictions_total": evictions_total,
        "lock_wait_s_total": lock_wait_s_total,
        "lock_wait_s_max": lock_wait_s_max,
        "last_lock_wait_s": lock_wait_s,
    })
}
