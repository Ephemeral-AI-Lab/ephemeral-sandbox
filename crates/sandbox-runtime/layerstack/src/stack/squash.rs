//! LayerStack squash: fold every squashable block of published layers into
//! equivalent flattened layers, in one storage transaction.
//!
//! Plan (shared, brief): boundaries are the newest layers of live leases from
//! `lease_newest_layers()`; blocks are maximal contiguous runs of ≥ 2
//! non-base layers between boundaries; the plan lease is an ordinary
//! snapshot whose commit-time release is the one and only GC. Build (no
//! lock): flatten each block into `staging/`. Commit (one exclusive critical
//! section): recheck run presence in the latest manifest, promote by same-fs
//! rename, one `syncfs` on the storage-root fd, atomic manifest replace,
//! record substitutions, release the plan lease (refcount GC returns the
//! removed set). `S` layers carry zero sidecars. Squash is singleflight per
//! root: the flight guard rides the outcome so the caller's post-commit
//! remount sweep stays inside it.

pub(crate) mod flatten;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::error::LayerStackError;
use crate::fs::{
    allocate_layer_dirs, canonical_key, remove_path, resolve_layer_path, syncfs_storage_root,
    write_manifest,
};
use crate::model::{LayerRef, Manifest};
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR};

use super::lease::release_lease_locked;
use super::{lock_shared_registry, LayerStack};

const BASE_LAYER_PREFIX: char = 'B';

/// One committed block: the flattened layer and the sources it replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquashedBlock {
    pub squashed_layer: LayerRef,
    pub replaced: Vec<LayerRef>,
}

/// A committed squash. Dropping the outcome ends the per-root singleflight,
/// so the caller keeps it alive across its post-commit remount sweep.
#[derive(Debug)]
pub struct SquashOutcome {
    pub manifest: Manifest,
    pub blocks: Vec<SquashedBlock>,
    pub removed: Vec<LayerRef>,
    _flight: SquashFlight,
}

/// Closed storage phases of one squash invocation.
///
/// The observer surface intentionally exposes only phase identity and the
/// operation result. Planning and build implementation types remain private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SquashPhase {
    Plan,
    Flatten,
    Commit,
}

/// Synchronous observer for the three storage phases of a squash.
///
/// Implementations must execute `body` exactly once and return its result.
/// This is a closed instrumentation seam, not a runtime plugin surface.
pub trait SquashPhaseObserver {
    fn observe<T>(
        &self,
        phase: SquashPhase,
        body: impl FnOnce() -> Result<T, LayerStackError>,
    ) -> Result<T, LayerStackError>;
}

#[derive(Debug, Default)]
struct NoopSquashPhaseObserver;

impl SquashPhaseObserver for NoopSquashPhaseObserver {
    fn observe<T>(
        &self,
        _phase: SquashPhase,
        body: impl FnOnce() -> Result<T, LayerStackError>,
    ) -> Result<T, LayerStackError> {
        body()
    }
}

#[derive(Debug)]
struct SquashFlight {
    busy: Arc<AtomicBool>,
}

impl Drop for SquashFlight {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
    }
}

impl LayerStack {
    /// Squash every squashable block and commit the compact manifest.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when another squash is in flight for this
    /// root, when the flatten build or any commit step fails (the old
    /// manifest stays valid and partial S dirs are removed in-process), or
    /// when the commit recheck finds a planned run broken.
    pub fn squash(&mut self) -> Result<SquashOutcome, LayerStackError> {
        self.squash_with_observer(&NoopSquashPhaseObserver)
    }

    /// Squash every squashable block while observing the three closed storage
    /// phases. The observer cannot inspect or alter private plan/build values.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`LayerStack::squash`].
    pub fn squash_with_observer(
        &mut self,
        observer: &impl SquashPhaseObserver,
    ) -> Result<SquashOutcome, LayerStackError> {
        let flight = begin_flight(&self.storage_root)?;
        let plan = observer.observe(SquashPhase::Plan, || self.plan_squash())?;
        let Some(plan) = plan else {
            let manifest = self.read_active_manifest()?;
            return Ok(SquashOutcome {
                manifest,
                blocks: Vec::new(),
                removed: Vec::new(),
                _flight: flight,
            });
        };
        let built = match observer.observe(SquashPhase::Flatten, || self.build_blocks(&plan)) {
            Ok(built) => built,
            Err(error) => {
                let _ = self.release_lease(&plan.plan_lease_id);
                return Err(error);
            }
        };
        let (manifest, blocks, removed) =
            match observer.observe(SquashPhase::Commit, || self.commit_squash(&plan, &built)) {
                Ok(committed) => committed,
                Err(error) => {
                    for block in &built {
                        let _ = remove_path(&block.staging_dir);
                    }
                    let _ = self.release_lease(&plan.plan_lease_id);
                    return Err(error);
                }
            };
        Ok(SquashOutcome {
            manifest,
            blocks,
            removed,
            _flight: flight,
        })
    }

    /// Phase 1 — plan under a brief shared guard: snapshot the manifest,
    /// compute boundaries and blocks, acquire the plan lease. Returns `None`
    /// when no block of ≥ 2 layers exists.
    pub(crate) fn plan_squash(&self) -> Result<Option<SquashPlan>, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest_unlocked()?;
        let (boundaries, plan_lease_id) = {
            let mut leases = lock_shared_registry(&self.leases)?;
            let boundaries: BTreeSet<String> = leases
                .lease_newest_layers()
                .into_iter()
                .map(|layer| layer.layer_id)
                .collect();
            let record = leases.acquire(manifest.clone(), "squash-plan")?;
            (boundaries, record.lease_id)
        };
        let blocks = partition_blocks(&manifest.layers, &boundaries);
        if blocks.is_empty() {
            let mut leases = lock_shared_registry(&self.leases)?;
            leases.release(&plan_lease_id);
            return Ok(None);
        }
        Ok(Some(SquashPlan {
            manifest,
            blocks,
            plan_lease_id,
        }))
    }

    /// Phase 2 — build outside any lock: flatten each planned block into a
    /// nonce-named staging tree. Sources stay in the active manifest for the
    /// whole build (publishes only prepend), so nothing can reclaim them.
    pub(crate) fn build_blocks(
        &self,
        plan: &SquashPlan,
    ) -> Result<Vec<BuiltBlock>, LayerStackError> {
        let mut built = Vec::with_capacity(plan.blocks.len());
        let result = (|| -> Result<(), LayerStackError> {
            for block in &plan.blocks {
                let (layer_id, staging_dir, layer_dir) =
                    allocate_layer_dirs(&self.storage_root, 'S', plan.manifest.version + 1)?;
                std::fs::create_dir_all(&staging_dir)?;
                let start = find_run(&plan.manifest.layers, block).ok_or_else(|| {
                    LayerStackError::Storage(format!(
                        "planned source run for {layer_id} is no longer present in the plan"
                    ))
                })?;
                let sources: Vec<PathBuf> = block
                    .iter()
                    .map(|layer| resolve_layer_path(&self.storage_root, &layer.path))
                    .collect();
                let lower_layers: Vec<PathBuf> = plan.manifest.layers[start + block.len()..]
                    .iter()
                    .map(|layer| resolve_layer_path(&self.storage_root, &layer.path))
                    .collect();
                built.push(BuiltBlock {
                    layer_id,
                    staging_dir: staging_dir.clone(),
                    layer_dir,
                    replaced: block.clone(),
                });
                flatten::flatten_block_into_with_lower(&staging_dir, &sources, &lower_layers)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            for block in &built {
                let _ = remove_path(&block.staging_dir);
            }
            return Err(error);
        }
        Ok(built)
    }

    /// Phase 3 — the commit: one exclusive critical section running
    /// recheck → promote → one `syncfs` → manifest rename → substitution
    /// recording → plan-lease release (the only GC, returning the removed
    /// set). Any pre-rename failure removes promoted S dirs in-process and
    /// leaves the old manifest valid.
    pub(crate) fn commit_squash(
        &mut self,
        plan: &SquashPlan,
        built: &[BuiltBlock],
    ) -> Result<(Manifest, Vec<SquashedBlock>, Vec<LayerRef>), LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let latest = self.read_active_manifest_unlocked()?;
        let mut layers = latest.layers.clone();
        for block in built {
            let Some(start) = find_run(&layers, &block.replaced) else {
                return Err(LayerStackError::Storage(format!(
                    "planned source run for {} is no longer contiguous in the active manifest",
                    block.layer_id
                )));
            };
            layers.splice(
                start..start + block.replaced.len(),
                std::iter::once(LayerRef {
                    layer_id: block.layer_id.clone(),
                    path: format!("{LAYERS_DIR}/{}", block.layer_id),
                }),
            );
        }
        let manifest = Manifest::new(latest.version + 1, layers, latest.schema_version)?;

        let mut promoted: Vec<&BuiltBlock> = Vec::with_capacity(built.len());
        let commit = (|| -> Result<(), LayerStackError> {
            for block in built {
                std::fs::rename(&block.staging_dir, &block.layer_dir)?;
                promoted.push(block);
            }
            syncfs_storage_root(&self.storage_root)?;
            write_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE), &manifest)?;
            Ok(())
        })();
        if let Err(error) = commit {
            for block in promoted {
                let _ = remove_path(&block.layer_dir);
            }
            return Err(error);
        }

        for block in built {
            self.record_substitution(
                LayerRef {
                    layer_id: block.layer_id.clone(),
                    path: format!("{LAYERS_DIR}/{}", block.layer_id),
                },
                block.replaced.clone(),
            );
        }
        let removed = {
            let mut leases = lock_shared_registry(&self.leases)?;
            release_lease_locked(&self.storage_root, &mut leases, &plan.plan_lease_id)?
                .unwrap_or_default()
        };
        let blocks = built
            .iter()
            .map(|block| SquashedBlock {
                squashed_layer: LayerRef {
                    layer_id: block.layer_id.clone(),
                    path: format!("{LAYERS_DIR}/{}", block.layer_id),
                },
                replaced: block.replaced.clone(),
            })
            .collect();
        Ok((manifest, blocks, removed))
    }
}

#[derive(Debug)]
pub(crate) struct SquashPlan {
    pub(crate) manifest: Manifest,
    pub(crate) blocks: Vec<Vec<LayerRef>>,
    pub(crate) plan_lease_id: String,
}

#[derive(Debug)]
pub(crate) struct BuiltBlock {
    pub(crate) layer_id: String,
    pub(crate) staging_dir: PathBuf,
    pub(crate) layer_dir: PathBuf,
    pub(crate) replaced: Vec<LayerRef>,
}

/// Maximal contiguous runs of ≥ 2 layers containing no boundary and no
/// base (`B*`) layer, in manifest (newest-first) order.
pub(crate) fn partition_blocks(
    layers: &[LayerRef],
    boundaries: &BTreeSet<String>,
) -> Vec<Vec<LayerRef>> {
    let mut blocks = Vec::new();
    let mut run: Vec<LayerRef> = Vec::new();
    for layer in layers {
        let breaks =
            boundaries.contains(&layer.layer_id) || layer.layer_id.starts_with(BASE_LAYER_PREFIX);
        if breaks {
            if run.len() >= 2 {
                blocks.push(std::mem::take(&mut run));
            }
            run.clear();
        } else {
            run.push(layer.clone());
        }
    }
    if run.len() >= 2 {
        blocks.push(run);
    }
    blocks
}

fn find_run(haystack: &[LayerRef], needle: &[LayerRef]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn begin_flight(storage_root: &Path) -> Result<SquashFlight, LayerStackError> {
    let busy = flight_flag_for_root(storage_root);
    if busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return Err(LayerStackError::Storage(format!(
            "a squash is already in flight for {}",
            storage_root.display()
        )));
    }
    Ok(SquashFlight { busy })
}

fn flight_flag_for_root(storage_root: &Path) -> Arc<AtomicBool> {
    let key = canonical_key(storage_root);
    flight_flags()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(key)
        .or_default()
        .clone()
}

fn flight_flags() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static FLAGS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    FLAGS.get_or_init(|| Mutex::new(HashMap::new()))
}
