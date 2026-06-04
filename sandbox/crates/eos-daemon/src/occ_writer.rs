//! Shared OCC writer, route oracle, and per-root OCC service cache.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use ignore::gitignore::GitignoreBuilder;
use ignore::Match;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use eos_layerstack::{LayerStack, MergedView, AUTO_SQUASH_MAX_DEPTH};
use eos_occ::{
    ChangesetResult, CommitQueue, CommitTransactionPort, FileResult, OccRouteProvider, OccService,
    OccStatus, PreparedChangeset, PublishConflict, Route,
};
use eos_protocol::{LayerChange, LayerPath, Manifest};

use crate::error::DaemonError;
use crate::response_timings::{i64_to_f64_saturating, usize_to_f64_saturating};

#[derive(Clone)]
pub(crate) struct LayerStackCommitTransaction {
    pub(crate) root: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OccRouteMetrics {
    pub(crate) gated_path_count: usize,
    pub(crate) direct_path_count: usize,
}

impl CommitTransactionPort for LayerStackCommitTransaction {
    fn revalidate_and_publish(
        &self,
        combined: &PreparedChangeset,
    ) -> std::result::Result<ChangesetResult, PublishConflict> {
        let total_start = Instant::now();
        let mut stack = match LayerStack::open(self.root.clone()) {
            Ok(stack) => stack,
            Err(err) => return Ok(failed_revalidate_result(combined, &err, total_start)),
        };
        let validate_start = Instant::now();
        let active = match stack.read_active_manifest() {
            Ok(manifest) => manifest,
            Err(err) => return Ok(failed_revalidate_result(combined, &err, total_start)),
        };
        let view = MergedView::new(self.root.clone());
        let validations = validate_prepared(&self.root, &view, &active, combined);
        let validate_s = validate_start.elapsed().as_secs_f64();
        if combined.atomic
            && validations
                .iter()
                .any(|file| is_validation_failure(file.status))
        {
            return Ok(atomic_validation_drop_result(
                combined,
                validations,
                validate_s,
                total_start,
            ));
        }
        let publishable_changes = publishable_changes(combined, &validations);
        if publishable_changes.is_empty() {
            return Ok(no_publish_result(
                combined,
                validations,
                validate_s,
                total_start,
            ));
        }
        let publish_start = Instant::now();
        match stack.publish_layer(&publishable_changes) {
            Ok(manifest) => {
                let publish_s = publish_start.elapsed().as_secs_f64();
                let auto_squash_timings = run_auto_squash(&mut stack);
                Ok(committed_changeset_result(
                    combined,
                    validations,
                    manifest_version_u64_optional(manifest.version),
                    PublishedCommitTimings {
                        validate_s,
                        publish_s,
                        auto_squash_timings,
                        total_start,
                    },
                ))
            }
            Err(eos_layerstack::LayerStackError::ManifestConflict { found, .. }) => {
                Err(PublishConflict {
                    observed_version: manifest_version_u64_optional(found),
                })
            }
            Err(err) => {
                let publish_s = publish_start.elapsed().as_secs_f64();
                let timings = commit_timings(
                    combined,
                    validate_s,
                    publish_s,
                    total_start.elapsed().as_secs_f64(),
                );
                Ok(failed_changeset_with_timings(
                    combined,
                    &err.to_string(),
                    timings,
                ))
            }
        }
    }
}

fn failed_revalidate_result(
    combined: &PreparedChangeset,
    err: &eos_layerstack::LayerStackError,
    total_start: Instant,
) -> ChangesetResult {
    let timings = commit_timings(combined, 0.0, 0.0, total_start.elapsed().as_secs_f64());
    failed_changeset_with_timings(combined, &err.to_string(), timings)
}

fn atomic_validation_drop_result(
    combined: &PreparedChangeset,
    validations: Vec<FileResult>,
    validate_s: f64,
    total_start: Instant,
) -> ChangesetResult {
    ChangesetResult {
        files: validations
            .into_iter()
            .map(|file| {
                if file.status.is_published() {
                    FileResult {
                        status: OccStatus::Dropped,
                        message: "not published because atomic changeset validation failed"
                            .to_owned(),
                        ..file
                    }
                } else {
                    file
                }
            })
            .collect(),
        published_manifest_version: None,
        timings: commit_timings(
            combined,
            validate_s,
            0.0,
            total_start.elapsed().as_secs_f64(),
        ),
    }
}

fn publishable_changes(
    combined: &PreparedChangeset,
    validations: &[FileResult],
) -> Vec<LayerChange> {
    let publishable_paths = validations
        .iter()
        .filter(|file| file.status.is_published())
        .map(|file| file.path.as_str())
        .collect::<HashSet<_>>();
    combined
        .changes
        .iter()
        .filter(|change| publishable_paths.contains(change.path().as_str()))
        .cloned()
        .collect()
}

fn no_publish_result(
    combined: &PreparedChangeset,
    validations: Vec<FileResult>,
    validate_s: f64,
    total_start: Instant,
) -> ChangesetResult {
    ChangesetResult {
        files: validations,
        published_manifest_version: None,
        timings: commit_timings(
            combined,
            validate_s,
            0.0,
            total_start.elapsed().as_secs_f64(),
        ),
    }
}

fn run_auto_squash(stack: &mut LayerStack) -> BTreeMap<String, f64> {
    let mut timings = BTreeMap::new();
    let Some(active) = stack.read_active_manifest().ok() else {
        return timings;
    };
    if active.depth() <= AUTO_SQUASH_MAX_DEPTH
        || !stack
            .can_squash(AUTO_SQUASH_MAX_DEPTH)
            .is_ok_and(|can_squash| can_squash)
    {
        return timings;
    }

    let squash_start = Instant::now();
    let squashed = stack.squash(AUTO_SQUASH_MAX_DEPTH).ok().flatten();
    let squash_elapsed_s = squash_start.elapsed().as_secs_f64();
    timings.insert(
        "layer_stack.auto_squash.total_s".to_owned(),
        squash_elapsed_s,
    );
    timings.insert(
        "layer_stack.auto_squash.max_depth".to_owned(),
        usize_to_f64_saturating(AUTO_SQUASH_MAX_DEPTH),
    );
    timings.insert(
        "layer_stack.auto_squash.depth_before".to_owned(),
        usize_to_f64_saturating(active.depth()),
    );
    match squashed {
        Some(manifest) => {
            timings.insert(
                "layer_stack.auto_squash.depth_after".to_owned(),
                usize_to_f64_saturating(manifest.depth()),
            );
            timings.insert(
                "layer_stack.auto_squash.manifest_version".to_owned(),
                i64_to_f64_saturating(manifest.version),
            );
        }
        None => {
            timings.insert("layer_stack.auto_squash.raced".to_owned(), 1.0);
        }
    }
    timings
}

fn committed_changeset_result(
    combined: &PreparedChangeset,
    validations: Vec<FileResult>,
    published_manifest_version: Option<u64>,
    phases: PublishedCommitTimings,
) -> ChangesetResult {
    let mut timings = commit_timings(
        combined,
        phases.validate_s,
        phases.publish_s,
        phases.total_start.elapsed().as_secs_f64(),
    );
    timings.extend(phases.auto_squash_timings);
    ChangesetResult {
        files: validations
            .into_iter()
            .map(|file| {
                if file.status.is_published() {
                    FileResult {
                        status: OccStatus::Committed,
                        ..file
                    }
                } else {
                    file
                }
            })
            .collect(),
        published_manifest_version,
        timings,
    }
}

struct PublishedCommitTimings {
    validate_s: f64,
    publish_s: f64,
    auto_squash_timings: BTreeMap<String, f64>,
    total_start: Instant,
}

#[derive(Clone)]
pub(crate) struct LayerStackRouteProvider {
    pub(crate) root: PathBuf,
}

impl OccRouteProvider for LayerStackRouteProvider {
    fn is_ignored(&self, path: &LayerPath) -> std::result::Result<bool, eos_occ::OccError> {
        // Per-call re-read of the active merged manifest: opening a fresh
        // `LayerStack` here is load-bearing, so a `.gitignore` edit committed
        // between ops is observed by the next route decision.
        let stack = LayerStack::open(self.root.clone())
            .map_err(|err| eos_occ::OccError::RoutePreparation(err.to_string()))?;
        path_is_ignored(&stack, path.as_str())
            .map_err(|err| eos_occ::OccError::RoutePreparation(err.to_string()))
    }

    fn base_hash(
        &self,
        path: &LayerPath,
    ) -> std::result::Result<Option<String>, eos_occ::OccError> {
        let stack = LayerStack::open(self.root.clone())
            .map_err(|err| eos_occ::OccError::RoutePreparation(err.to_string()))?;
        let (bytes, exists) = stack
            .read_bytes(path.as_str())
            .map_err(|err| eos_occ::OccError::RoutePreparation(err.to_string()))?;
        Ok(hash_current(bytes.as_deref(), exists))
    }
}

pub(crate) fn apply_occ_changeset(
    root: &Path,
    snapshot_version: Option<u64>,
    changes: &[LayerChange],
    base_hashes: &[(LayerPath, Option<String>)],
) -> Result<ChangesetResult, DaemonError> {
    let lookup = occ_service_for_root(root)?;
    let mut result = lookup.service.apply_changeset_with_base_hashes(
        changes,
        snapshot_version,
        true,
        base_hashes,
    )?;
    lookup.insert_timings(&mut result.timings);
    Ok(result)
}

pub(crate) fn occ_route_metrics(
    root: &Path,
    changes: &[LayerChange],
) -> Result<OccRouteMetrics, DaemonError> {
    let stack = LayerStack::open(root.to_path_buf())?;
    let mut metrics = OccRouteMetrics::default();
    for change in changes {
        let path = change.path().as_str();
        if path == ".git" || path.starts_with(".git/") {
            continue;
        }
        if path_is_ignored(&stack, path)? {
            metrics.direct_path_count += 1;
        } else {
            metrics.gated_path_count += 1;
        }
    }
    Ok(metrics)
}

pub(crate) fn insert_occ_route_timings(
    timings: &mut serde_json::Map<String, Value>,
    metrics: OccRouteMetrics,
    route_s: f64,
    occ_s: f64,
) {
    for (key, value) in [
        ("occ.prepare.prepare_groups_s", route_s),
        ("occ.prepare.group_by_route_s", route_s),
        ("occ.prepare.route_and_base_hash_s", route_s),
        ("occ.prepare.total_s", route_s),
        ("occ.commit.total_s", occ_s),
        (
            "occ.commit.gated_path_count",
            usize_to_f64_saturating(metrics.gated_path_count),
        ),
        (
            "occ.commit.direct_path_count",
            usize_to_f64_saturating(metrics.direct_path_count),
        ),
    ] {
        timings.insert(key.to_owned(), json!(value));
    }
    for key in [
        "occ.commit.validate_groups_s",
        "occ.commit.publish_layer_s",
        "occ.commit.stager_write_total_s",
        "occ.commit.stager_write_count",
        "occ.commit.gated_read_current_total_s",
        "occ.commit.gated_apply_changes_total_s",
        "occ.commit.gated_stage_delta_total_s",
        "occ.commit.direct_read_current_total_s",
        "occ.commit.direct_apply_changes_total_s",
        "occ.commit.direct_stage_delta_total_s",
    ] {
        timings.entry(key.to_owned()).or_insert_with(|| json!(0.0));
    }
}

pub(crate) fn base_hashes_for_snapshot(
    root: &Path,
    manifest: &eos_layerstack::Manifest,
    changes: &[LayerChange],
) -> Result<Vec<(LayerPath, Option<String>)>, DaemonError> {
    let view = MergedView::new(root.to_path_buf());
    changes
        .iter()
        .map(|change| {
            if matches!(change, LayerChange::OpaqueDir { .. }) {
                return Ok((change.path().clone(), None));
            }
            let (bytes, exists) = view.read_bytes(change.path().as_str(), manifest)?;
            Ok((
                change.path().clone(),
                hash_current(bytes.as_deref(), exists),
            ))
        })
        .collect()
}

pub(crate) const OCC_SERVICE_CACHE_MAX: usize = 256;

pub(crate) struct OccServiceLookup {
    service: Arc<OccService<LayerStackCommitTransaction>>,
    pub(crate) lock_wait_s: f64,
    pub(crate) cache_hit: bool,
    pub(crate) cache_created: bool,
    pub(crate) evicted_count: usize,
    pub(crate) cache_size: usize,
}

impl OccServiceLookup {
    fn insert_timings(&self, timings: &mut BTreeMap<String, f64>) {
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

    pub(crate) fn insert_or_get(
        &mut self,
        key: String,
        service: Arc<OccService<LayerStackCommitTransaction>>,
        lock_wait_s: f64,
    ) -> OccServiceLookup {
        self.record_lock_wait(lock_wait_s);
        if let Some(existing) = self.entries.get(&key).cloned() {
            self.touch(&key);
            self.stats.hits_total += 1;
            return OccServiceLookup {
                service: existing,
                lock_wait_s,
                cache_hit: true,
                cache_created: false,
                evicted_count: 0,
                cache_size: self.entries.len(),
            };
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
        OccServiceLookup {
            service,
            lock_wait_s,
            cache_hit: false,
            cache_created: true,
            evicted_count,
            cache_size: self.entries.len(),
        }
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

fn occ_service_for_root(root: &Path) -> Result<OccServiceLookup, DaemonError> {
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
    Ok(cache.insert_or_get(key, service, lock_start.elapsed().as_secs_f64()))
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

fn validate_prepared(
    root: &Path,
    view: &MergedView,
    manifest: &Manifest,
    prepared: &PreparedChangeset,
) -> Vec<FileResult> {
    let mut parent_absent_cache = HashMap::new();
    prepared
        .path_groups
        .iter()
        .map(|group| match group.route {
            Route::Drop => FileResult {
                path: group.path.clone(),
                status: OccStatus::Dropped,
                message: group
                    .message
                    .clone()
                    .unwrap_or_else(|| "change dropped".to_owned()),
            },
            Route::Reject => FileResult {
                path: group.path.clone(),
                status: OccStatus::Rejected,
                message: group
                    .message
                    .clone()
                    .unwrap_or_else(|| "change rejected".to_owned()),
            },
            Route::Direct => validate_direct_group(&group.path),
            Route::Gated => validate_gated_group(
                root,
                view,
                manifest,
                prepared,
                &group.path,
                group.base_hash.as_deref(),
                &mut parent_absent_cache,
            ),
            _ => FileResult {
                path: group.path.clone(),
                status: OccStatus::Rejected,
                message: "unsupported route".to_owned(),
            },
        })
        .collect()
}

fn validate_direct_group(path: &LayerPath) -> FileResult {
    FileResult {
        path: path.clone(),
        status: OccStatus::Accepted,
        message: String::new(),
    }
}

fn validate_gated_group(
    root: &Path,
    view: &MergedView,
    manifest: &Manifest,
    prepared: &PreparedChangeset,
    path: &LayerPath,
    base_hash: Option<&str>,
    parent_absent_cache: &mut HashMap<String, bool>,
) -> FileResult {
    let path_str = path.as_str();
    if prepared.changes.iter().any(|change| {
        change.path().as_str() == path_str && matches!(change, LayerChange::Symlink { .. })
    }) {
        return FileResult {
            path: path.clone(),
            status: OccStatus::Rejected,
            message: "unsupported gated change kind: SymlinkChange".to_owned(),
        };
    }
    if base_hash.is_none() {
        if let Some(parent) = parent_dir(path_str) {
            let parent_absent = *parent_absent_cache
                .entry(parent.to_owned())
                .or_insert_with(|| parent_absent_from_manifest(root, manifest, parent));
            if parent_absent {
                return FileResult {
                    path: path.clone(),
                    status: OccStatus::Accepted,
                    message: String::new(),
                };
            }
        }
    }
    match view.read_bytes(path_str, manifest) {
        Ok((bytes, exists)) if hash_current(bytes.as_deref(), exists).as_deref() == base_hash => {
            FileResult {
                path: path.clone(),
                status: OccStatus::Accepted,
                message: String::new(),
            }
        }
        Ok(_) => FileResult {
            path: path.clone(),
            status: OccStatus::AbortedVersion,
            message: "content changed".to_owned(),
        },
        Err(err) => FileResult {
            path: path.clone(),
            status: OccStatus::Failed,
            message: err.to_string(),
        },
    }
}

fn parent_dir(path: &str) -> Option<&str> {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .filter(|parent| !parent.is_empty())
}

fn parent_absent_from_manifest(root: &Path, manifest: &Manifest, parent: &str) -> bool {
    manifest.layers.iter().all(|layer| {
        let path = PathBuf::from(&layer.path);
        let layer_dir = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        matches!(
            std::fs::symlink_metadata(layer_dir.join(parent)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound
        )
    })
}

const fn is_validation_failure(status: OccStatus) -> bool {
    matches!(
        status,
        OccStatus::AbortedOverlap
            | OccStatus::AbortedVersion
            | OccStatus::Failed
            | OccStatus::Rejected
    )
}

pub(crate) fn hash_current(content: Option<&[u8]>, exists: bool) -> Option<String> {
    if !exists {
        return None;
    }
    content.map(hash_bytes)
}

pub(crate) fn hash_bytes(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
        out.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// OCC route oracle: does `path` (always a concrete file change) match a
/// `.gitignore` rule in this layer-stack snapshot?
///
/// This is the one shared routine behind both `LayerStackRouteProvider::is_ignored`
/// (DIRECT vs GATED) and `occ_route_metrics` (telemetry). It reproduces the Python
/// `PathspecGitignoreOracle` semantics (`/tmp/oldpy/.../occ/gitignore.py`):
/// per-directory `.gitignore` read from the merged snapshot, deeper-wins
/// inheritance, and the directory-exclusion seal (an excluded ancestor dir seals
/// its whole subtree — a deeper `!` re-include cannot rescue it).
///
/// All `.gitignore` reads go through `stack.read_bytes`, i.e. the active merged
/// manifest (newest-layer-wins, whiteout-aware) — the same view the overlay mount
/// projects, never a disk-walk. The per-pattern matching (dir-only-at-any-depth,
/// `*`-not-crossing-`/`, `**`, `!` ordering, char classes) is delegated to the
/// `ignore` crate's gitignore engine.
fn path_is_ignored(stack: &LayerStack, path: &str) -> Result<bool, DaemonError> {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        return Ok(false);
    }
    // Directory-exclusion seal: if any ancestor directory of `path` is excluded
    // as a directory, `path` is ignored regardless of any deeper re-include.
    let parts: Vec<&str> = rel.split('/').collect();
    let mut accum = String::new();
    for part in &parts[..parts.len() - 1] {
        accum = join_rel(&accum, part);
        if dir_is_excluded(stack, &accum)? {
            return Ok(true);
        }
    }
    match_with_inheritance(stack, rel, false)
}

/// Is directory `dir_rel` excluded? Walks its components root→leaf; once an
/// ancestor is excluded the whole chain stays excluded (Git's directory seal).
fn dir_is_excluded(stack: &LayerStack, dir_rel: &str) -> Result<bool, DaemonError> {
    let mut accum = String::new();
    let mut excluded = false;
    for part in dir_rel.split('/').filter(|part| !part.is_empty()) {
        accum = join_rel(&accum, part);
        if !excluded {
            excluded = match_with_inheritance(stack, &accum, true)?;
        }
    }
    Ok(excluded)
}

/// Last-match-wins evaluation across every `.gitignore` at or above `path`'s
/// ancestor directories (root → `path`'s parent), deeper directories overriding
/// shallower ones. The caller owns the directory seal; this is the unsealed
/// evaluator. `as_dir` lets directory-only patterns (`foo/`) fire.
fn match_with_inheritance(
    stack: &LayerStack,
    path: &str,
    as_dir: bool,
) -> Result<bool, DaemonError> {
    let parts: Vec<&str> = path.split('/').collect();
    let mut ignored = false;
    let mut accum = String::new();
    for part in &parts {
        if let Some(matcher) = matcher_for(stack, &accum)? {
            // Pass `path` relative to `accum`. The matcher is rooted at `.`
            // (see `matcher_for`), so the crate performs no further stripping and
            // per-dir pattern anchoring (`/build`, `src/*.rs`) is preserved.
            let sub = if accum.is_empty() {
                path
            } else {
                path[accum.len()..].trim_start_matches('/')
            };
            if !sub.is_empty() {
                match matcher.matched(sub, as_dir) {
                    Match::Ignore(_) => ignored = true,
                    Match::Whitelist(_) => ignored = false,
                    Match::None => {}
                }
            }
        }
        accum = join_rel(&accum, part);
    }
    Ok(ignored)
}

/// Build the gitignore matcher for `dir_rel`'s own `.gitignore`, read from the
/// merged snapshot. A missing, non-UTF-8, or unparseable file contributes no
/// patterns (`Ok(None)`) — the safe, validated GATED route. Only a genuine
/// `read_bytes` I/O error propagates.
fn matcher_for(
    stack: &LayerStack,
    dir_rel: &str,
) -> Result<Option<ignore::gitignore::Gitignore>, DaemonError> {
    let rel = join_rel(dir_rel, ".gitignore");
    let (bytes, exists) = stack.read_bytes(&rel)?;
    if !exists {
        return Ok(None);
    }
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    // Root `.` (not `dir_rel`): the caller in `match_with_inheritance` already
    // makes the candidate relative to this directory, and the `ignore` crate's
    // `Gitignore::matched` re-strips its root by raw byte prefix — rooting at
    // `dir_rel` would strip it a second time whenever a child component repeats
    // the directory name (e.g. `a/.gitignore` `/x` vs `a/a/x`). Root `.` disables
    // that strip; per-pattern anchoring comes from the pattern text, not the root.
    let mut builder = GitignoreBuilder::new(".");
    for line in text.lines() {
        // `add_line` skips comments/blanks itself; ignore malformed patterns.
        let _ = builder.add_line(None, line);
    }
    Ok(builder.build().ok())
}

/// Join a relative dir prefix with a child component (`""` + `c` -> `c`).
fn join_rel(prefix: &str, child: &str) -> String {
    if prefix.is_empty() {
        child.to_owned()
    } else {
        format!("{prefix}/{child}")
    }
}

fn failed_changeset_with_timings(
    prepared: &PreparedChangeset,
    message: &str,
    timings: BTreeMap<String, f64>,
) -> ChangesetResult {
    ChangesetResult {
        files: prepared
            .path_groups
            .iter()
            .map(|group| FileResult {
                path: group.path.clone(),
                status: OccStatus::Failed,
                message: message.to_owned(),
            })
            .collect(),
        published_manifest_version: None,
        timings,
    }
}

fn commit_timings(
    prepared: &PreparedChangeset,
    validate_s: f64,
    publish_s: f64,
    total_s: f64,
) -> BTreeMap<String, f64> {
    let mut timings = BTreeMap::new();
    timings.insert("occ.apply.total_s".to_owned(), total_s);
    timings.insert("occ.commit.total_s".to_owned(), total_s);
    timings.insert("occ.commit.validate_groups_s".to_owned(), validate_s);
    timings.insert("occ.commit.publish_layer_s".to_owned(), publish_s);
    timings.insert(
        "occ.commit.stager_write_count".to_owned(),
        usize_to_f64_saturating(prepared.changes.len()),
    );
    timings.insert("occ.commit.stager_write_total_s".to_owned(), publish_s);
    timings.insert(
        "occ.commit.gated_path_count".to_owned(),
        usize_to_f64_saturating(
            prepared
                .path_groups
                .iter()
                .filter(|group| group.route == Route::Gated)
                .count(),
        ),
    );
    timings.insert(
        "occ.commit.direct_path_count".to_owned(),
        usize_to_f64_saturating(
            prepared
                .path_groups
                .iter()
                .filter(|group| group.route == Route::Direct)
                .count(),
        ),
    );
    for key in [
        "occ.commit.gated_read_current_total_s",
        "occ.commit.gated_apply_changes_total_s",
        "occ.commit.gated_stage_delta_total_s",
        "occ.commit.direct_read_current_total_s",
        "occ.commit.direct_apply_changes_total_s",
        "occ.commit.direct_stage_delta_total_s",
    ] {
        timings.insert(key.to_owned(), 0.0);
    }
    timings
}

pub(crate) fn manifest_version_u64(version: i64) -> Result<u64, DaemonError> {
    u64::try_from(version).map_err(|_| {
        DaemonError::LayerStack(eos_layerstack::LayerStackError::Manifest(format!(
            "manifest version must be non-negative: {version}"
        )))
    })
}

fn manifest_version_u64_optional(version: i64) -> Option<u64> {
    u64::try_from(version).ok()
}
