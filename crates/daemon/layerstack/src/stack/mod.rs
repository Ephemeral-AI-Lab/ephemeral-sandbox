use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::lease_aware::{
    plan_lease_aware_gaps, LeaseAwareCheckpointMode, LeaseAwareCopyThroughOutcome,
    LeaseAwarePlanEntry, LeaseAwareReclaimOutcome, LeaseParentCompactionOutcome,
    ReclaimingInterval,
};
use crate::model::{
    layer_digest, manifest_root_hash, try_layer_digest, LayerChange, LayerPath, LayerRef, Manifest,
};

use crate::error::LayerStackError;
use crate::fs::{
    allocate_layer_dirs, count_dirs, fsync_dir, fsync_tree_files, layer_digest_path, next_unique,
    read_manifest, record_elapsed, remove_path, replace_workspace_contents, resolve_layer_path,
    storage_bytes, write_layer_digest, write_manifest,
};
use crate::lock::StorageWriterLockLease;
use crate::squash::{
    manifest_prefix_before_plan, CheckpointSegment, LayerCheckpointSquasher, SquashPlanDecision,
    SquashPlanEntry,
};
use crate::workspace::build_workspace_base_from_snapshot;
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, LAYER_METADATA_DIR, STAGING_DIR};

mod layer_read;
mod layer_write;
mod lease_cleanup;
mod leases;
mod view;
mod workspace_commit;

use layer_read::capture_layer_dir_unbounded;
use layer_write::write_layer_changes;
use lease_cleanup::{
    release_lease_locked, remove_unreferenced_layer_candidates_locked, retarget_lease_locked,
};
pub(crate) use leases::reset_shared_registries_for_tests;
use leases::{
    lock_shared_registry, lock_shared_registry_recover, shared_registry_for_root,
    SharedLeaseRegistry,
};
use view::layer_has_boundary_markers;
pub use view::MergedView;
pub(crate) use workspace_commit::*;

const FAIL_NEXT_PUBLISH_MARKER_FILE: &str = "fail-next-publish";
const ENABLE_TEST_FAILPOINTS_ENV: &str = "EOS_LAYERSTACK_ENABLE_TEST_FAILPOINTS";

#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub lease_id: String,
    pub manifest_version: i64,
    pub root_hash: String,
    pub manifest: Manifest,
    pub layer_paths: Vec<String>,
}

#[derive(Debug)]
pub struct SquashOutcome {
    pub manifest: Option<Manifest>,
    pub lease_release_error: Option<LayerStackError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerStackStorageMetrics {
    pub layer_dirs: usize,
    pub staging_dirs: usize,
    pub storage_bytes: u64,
}

#[derive(Debug)]
pub struct LayerStack {
    storage_root: PathBuf,
    pub(crate) writer_lock: StorageWriterLockLease,
    leases: SharedLeaseRegistry,
    view: MergedView,
}

impl LayerStack {
    pub fn open(storage_root: PathBuf) -> Result<Self, LayerStackError> {
        std::fs::create_dir_all(storage_root.join(LAYERS_DIR))?;
        std::fs::create_dir_all(storage_root.join(STAGING_DIR))?;
        let writer_lock = StorageWriterLockLease::acquire(&storage_root)?;
        recover_commit_to_workspace(&storage_root)?;
        let leases = shared_registry_for_root(&storage_root)?;
        let view = MergedView::new(storage_root.clone());
        Ok(Self {
            storage_root,
            writer_lock,
            leases,
            view,
        })
    }

    pub fn read_active_manifest(&self) -> Result<Manifest, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        self.read_active_manifest_unlocked()
    }

    fn read_active_manifest_unlocked(&self) -> Result<Manifest, LayerStackError> {
        read_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE))
    }

    pub(crate) fn with_active_manifest<T>(
        &self,
        f: impl FnOnce(&Manifest) -> Result<T, LayerStackError>,
    ) -> Result<T, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest_unlocked()?;
        f(&manifest)
    }

    pub fn acquire_snapshot(&self, owner_request_id: &str) -> Result<Lease, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest_unlocked()?;
        let lease = {
            let mut leases = lock_shared_registry(&self.leases)?;
            leases.acquire(manifest.clone(), owner_request_id)?
        };
        let layer_paths = manifest
            .layers
            .iter()
            .map(|layer| resolve_layer_path(&self.storage_root, &layer.path))
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        Ok(Lease {
            lease_id: lease.lease_id,
            manifest_version: manifest.version,
            root_hash: manifest_root_hash(&manifest),
            manifest,
            layer_paths,
        })
    }

    pub fn release_lease(&mut self, lease_id: &str) -> Result<bool, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let mut leases = lock_shared_registry(&self.leases)?;
        release_lease_locked(&self.storage_root, &mut leases, lease_id)
    }

    #[doc(hidden)]
    pub fn retarget_lease_manifest(
        &mut self,
        lease_id: &str,
        manifest: Manifest,
    ) -> Result<bool, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let mut leases = lock_shared_registry(&self.leases)?;
        retarget_lease_locked(&self.storage_root, &mut leases, lease_id, manifest)
    }

    #[doc(hidden)]
    pub fn build_compaction_checkpoint(
        &mut self,
        manifest: &Manifest,
    ) -> Result<LayerRef, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        if manifest.layers.is_empty() {
            return Err(LayerStackError::InvalidSquashPlan(
                "compaction checkpoint requires at least one layer".to_owned(),
            ));
        }
        self.build_projected_checkpoint(manifest)
    }

    pub fn can_squash(&self, max_depth: usize) -> Result<bool, LayerStackError> {
        let active = self.read_active_manifest()?;
        let squasher = LayerCheckpointSquasher::new(self.storage_root.clone());
        let lease_head_layers = lock_shared_registry(&self.leases)?.lease_head_layers();
        Ok(squasher
            .plan(&active, max_depth, &lease_head_layers, 2)?
            .is_some())
    }

    pub(crate) fn squash_plan_decision(
        &self,
        max_depth: usize,
        min_reduction: usize,
    ) -> Result<(usize, SquashPlanDecision), LayerStackError> {
        let active = self.read_active_manifest()?;
        let depth = active.depth();
        let squasher = LayerCheckpointSquasher::new(self.storage_root.clone());
        let lease_head_layers = lock_shared_registry(&self.leases)?.lease_head_layers();
        let decision =
            squasher.plan_decision(&active, max_depth, &lease_head_layers, min_reduction)?;
        Ok((depth, decision))
    }

    pub fn squash(&mut self, max_depth: usize) -> Result<SquashOutcome, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        let squasher = LayerCheckpointSquasher::new(self.storage_root.clone());
        let lease_head_layers = {
            let leases = lock_shared_registry(&self.leases)?;
            leases.lease_head_layers()
        };
        let Some(plan) = squasher.plan(&active, max_depth, &lease_head_layers, 1)? else {
            return Ok(SquashOutcome {
                manifest: None,
                lease_release_error: None,
            });
        };
        let squash_lease = {
            let mut leases = lock_shared_registry(&self.leases)?;
            leases.acquire(active, &format!("squash-{}", next_unique()))?
        };

        let mut checkpoints = Vec::new();
        let mut committed = false;
        let outcome = (|| {
            for segment in plan.checkpoint_segments() {
                checkpoints.push(squasher.build_checkpoint(segment, plan.active_version)?);
            }

            let current = self.read_active_manifest_unlocked()?;
            let Some(live_prefix) = manifest_prefix_before_plan(&current, &plan) else {
                return Ok(None);
            };
            let next_version = current.version + 1;
            let mut checkpoint_index = 0;
            let mut new_layers = live_prefix.to_vec();
            for entry in &plan.entries {
                match entry {
                    SquashPlanEntry::Keep(layer) => new_layers.push(layer.clone()),
                    SquashPlanEntry::Segment(_) => {
                        let mut checkpoint = checkpoints[checkpoint_index].clone();
                        let expected_prefix = format!("B{next_version:06}-");
                        if !checkpoint.layer_id.starts_with(&expected_prefix) {
                            checkpoint = squasher.relabel_checkpoint(&checkpoint, next_version)?;
                            checkpoints[checkpoint_index] = checkpoint.clone();
                        }
                        new_layers.push(checkpoint);
                        checkpoint_index += 1;
                    }
                }
            }
            let manifest = Manifest::new(next_version, new_layers, current.schema_version)
                .map_err(LayerStackError::from)?;
            write_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE), &manifest)?;
            committed = true;
            Ok(Some(manifest))
        })();

        if !committed {
            for checkpoint in &checkpoints {
                let _ = squasher.discard_checkpoint(checkpoint);
            }
        }
        let release = {
            let mut leases = lock_shared_registry(&self.leases)?;
            release_lease_locked(&self.storage_root, &mut leases, &squash_lease.lease_id)
        };
        match (outcome, release) {
            (Err(err), _) => Err(err),
            (Ok(manifest), Ok(_)) => Ok(SquashOutcome {
                manifest,
                lease_release_error: None,
            }),
            (Ok(manifest), Err(release_err)) => {
                if committed {
                    Ok(SquashOutcome {
                        manifest,
                        lease_release_error: Some(release_err),
                    })
                } else {
                    Err(release_err)
                }
            }
        }
    }

    #[doc(hidden)]
    pub fn reclaim_lease_aware_view_checkpoints(
        &mut self,
        min_reclaiming_interval_layers: usize,
    ) -> Result<LeaseAwareReclaimOutcome, LayerStackError> {
        self.reclaim_lease_aware_checkpoints_inner(min_reclaiming_interval_layers, false)
    }

    #[doc(hidden)]
    pub fn reclaim_lease_aware_checkpoints(
        &mut self,
        min_reclaiming_interval_layers: usize,
    ) -> Result<LeaseAwareReclaimOutcome, LayerStackError> {
        self.reclaim_lease_aware_checkpoints_inner(min_reclaiming_interval_layers, true)
    }

    fn reclaim_lease_aware_checkpoints_inner(
        &mut self,
        min_reclaiming_interval_layers: usize,
        allow_delta_checkpoints: bool,
    ) -> Result<LeaseAwareReclaimOutcome, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        let protected_layers = {
            let leases = lock_shared_registry(&self.leases)?;
            leases.leased_layers()
        };
        let plan =
            plan_lease_aware_gaps(&active, &protected_layers, min_reclaiming_interval_layers)?;
        let squasher = LayerCheckpointSquasher::new(self.storage_root.clone());
        let mut checkpoints = Vec::new();
        let mut new_layers = Vec::with_capacity(active.layers.len());
        let mut removable_candidates = Vec::new();
        let mut view_checkpoint_count = 0;
        let mut delta_checkpoint_count = 0;
        let mut skipped_delta_interval_count = 0;
        let mut committed = false;

        let outcome = (|| {
            for entry in &plan.entries {
                match entry {
                    LeaseAwarePlanEntry::KeepProtected(layer)
                    | LeaseAwarePlanEntry::KeepUnleased(layer) => {
                        new_layers.push(layer.clone());
                    }
                    LeaseAwarePlanEntry::ReclaimingInterval(interval)
                        if self.interval_can_use_view_checkpoint(interval)? =>
                    {
                        let segment = CheckpointSegment::new(interval.layers.clone())?;
                        let checkpoint = squasher.build_checkpoint(&segment, active.version)?;
                        checkpoints.push(checkpoint.clone());
                        new_layers.push(checkpoint);
                        removable_candidates.extend(interval.layers.clone());
                        view_checkpoint_count += 1;
                    }
                    LeaseAwarePlanEntry::ReclaimingInterval(interval)
                        if allow_delta_checkpoints =>
                    {
                        let checkpoint = self.build_delta_checkpoint(interval, active.version)?;
                        checkpoints.push(checkpoint.clone());
                        new_layers.push(checkpoint);
                        removable_candidates.extend(interval.layers.clone());
                        delta_checkpoint_count += 1;
                    }
                    LeaseAwarePlanEntry::ReclaimingInterval(interval) => {
                        skipped_delta_interval_count += 1;
                        new_layers.extend(interval.layers.clone());
                    }
                }
            }

            if view_checkpoint_count + delta_checkpoint_count == 0 {
                return Ok(LeaseAwareReclaimOutcome {
                    manifest: None,
                    protected_layer_count: plan.protected_layer_count,
                    planned_reclaiming_interval_count: plan.reclaiming_interval_count,
                    view_checkpoint_count,
                    delta_checkpoint_count,
                    skipped_delta_interval_count,
                    removed_layer_count: 0,
                    active_depth_before: active.depth(),
                    active_depth_after: active.depth(),
                });
            }

            let current = self.read_active_manifest_unlocked()?;
            if current != active {
                return Err(LayerStackError::ManifestConflict {
                    expected: active.version,
                    found: current.version,
                });
            }
            let manifest = Manifest::new(current.version + 1, new_layers, current.schema_version)
                .map_err(LayerStackError::from)?;
            write_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE), &manifest)?;
            committed = true;

            let removed = {
                let leases = lock_shared_registry(&self.leases)?;
                remove_unreferenced_layer_candidates_locked(
                    &self.storage_root,
                    &leases,
                    &removable_candidates,
                )?
            };

            Ok(LeaseAwareReclaimOutcome {
                manifest: Some(manifest.clone()),
                protected_layer_count: plan.protected_layer_count,
                planned_reclaiming_interval_count: plan.reclaiming_interval_count,
                view_checkpoint_count,
                delta_checkpoint_count,
                skipped_delta_interval_count,
                removed_layer_count: removed.len(),
                active_depth_before: active.depth(),
                active_depth_after: manifest.depth(),
            })
        })();

        if !committed {
            for checkpoint in &checkpoints {
                let _ = squasher.discard_checkpoint(checkpoint);
            }
        }

        outcome
    }

    fn build_delta_checkpoint(
        &self,
        interval: &ReclaimingInterval,
        active_version: i64,
    ) -> Result<LayerRef, LayerStackError> {
        let changes = self.delta_changes_for_interval(interval)?;
        let (layer_id, staging_dir, layer_dir) =
            allocate_layer_dirs(&self.storage_root, 'B', active_version + 1)?;
        std::fs::create_dir_all(&staging_dir)?;
        if let Err(err) = write_layer_changes(&staging_dir, &changes)
            .and_then(|()| fsync_tree_files(&staging_dir))
            .and_then(|()| fsync_dir(&staging_dir))
        {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err);
        }
        if let Some(parent) = layer_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(err) = std::fs::rename(&staging_dir, &layer_dir) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err.into());
        }
        if let Some(parent) = layer_dir.parent() {
            fsync_dir(parent)?;
        }
        write_layer_digest(&self.storage_root, &layer_id, &layer_digest(&changes))?;
        Ok(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        })
    }

    fn delta_changes_for_interval(
        &self,
        interval: &ReclaimingInterval,
    ) -> Result<Vec<LayerChange>, LayerStackError> {
        let mut by_path = BTreeMap::new();
        for layer in interval.layers.iter().rev() {
            let layer_dir = resolve_layer_path(&self.storage_root, &layer.path);
            let changes = capture_layer_dir_unbounded(&layer_dir).map_err(|err| {
                LayerStackError::Storage(format!(
                    "failed to capture stored layer {} for delta checkpoint: {err}",
                    layer.layer_id
                ))
            })?;
            for change in changes {
                apply_delta_change(&mut by_path, change);
            }
        }
        Ok(by_path.into_values().collect())
    }

    fn interval_can_use_view_checkpoint(
        &self,
        interval: &ReclaimingInterval,
    ) -> Result<bool, LayerStackError> {
        if interval.checkpoint_mode == LeaseAwareCheckpointMode::View {
            return Ok(true);
        }
        for layer in &interval.layers {
            let layer_dir = resolve_layer_path(&self.storage_root, &layer.path);
            if layer_has_boundary_markers(&layer_dir)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[doc(hidden)]
    pub fn compact_leased_parent_for_remount(
        &mut self,
        lease_id: &str,
        min_parent_layers: usize,
    ) -> Result<LeaseParentCompactionOutcome, LayerStackError> {
        if min_parent_layers == 0 {
            return Err(LayerStackError::InvalidSquashPlan(
                "min_parent_layers must be positive".to_owned(),
            ));
        }
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        let lease_manifest = {
            let leases = lock_shared_registry(&self.leases)?;
            leases.manifest(lease_id).ok_or_else(|| {
                LayerStackError::InvalidLeaseOwner(format!("lease not found: {lease_id}"))
            })?
        };
        let lease_depth_before = lease_manifest.depth();
        let active_depth_before = active.depth();
        if lease_manifest.layers.len() <= 1
            || lease_manifest.layers.len().saturating_sub(1) < min_parent_layers
        {
            return Ok(LeaseParentCompactionOutcome {
                lease_manifest: None,
                active_manifest: None,
                compact_parent_layer: None,
                compacted_parent_layer_count: 0,
                removed_layer_count: 0,
                bytes_added: 0,
                lease_depth_before,
                lease_depth_after: lease_depth_before,
                active_depth_before,
                active_depth_after: active_depth_before,
            });
        }
        let Some(lease_start) = find_layer_sequence(&active.layers, &lease_manifest.layers) else {
            return Err(LayerStackError::InvalidSquashPlan(format!(
                "lease {lease_id} manifest is not a contiguous active-manifest suffix"
            )));
        };
        if lease_start + lease_manifest.layers.len() != active.layers.len() {
            return Err(LayerStackError::InvalidSquashPlan(format!(
                "lease {lease_id} manifest is not an active-manifest suffix"
            )));
        }

        let protected_head = lease_manifest.layers[0].clone();
        let parent_layers = lease_manifest.layers[1..].to_vec();
        let parent_manifest = Manifest::new(
            lease_manifest.version,
            parent_layers.clone(),
            lease_manifest.schema_version,
        )
        .map_err(LayerStackError::from)?;
        let compact_parent_layer = self.build_copy_through_checkpoint(&parent_manifest)?;
        let compact_parent_path =
            resolve_layer_path(&self.storage_root, &compact_parent_layer.path);
        let bytes_added = storage_bytes(&compact_parent_path)?;

        let lease_manifest_after = Manifest::new(
            lease_manifest.version,
            vec![protected_head.clone(), compact_parent_layer.clone()],
            lease_manifest.schema_version,
        )
        .map_err(LayerStackError::from)?;

        let mut active_layers_after =
            Vec::with_capacity(active.layers.len() - parent_layers.len() + 1);
        active_layers_after.extend_from_slice(&active.layers[..lease_start]);
        active_layers_after.push(protected_head);
        active_layers_after.push(compact_parent_layer.clone());
        active_layers_after
            .extend_from_slice(&active.layers[lease_start + lease_manifest.layers.len()..]);
        let active_manifest_after = Manifest::new(
            active.version + 1,
            active_layers_after,
            active.schema_version,
        )
        .map_err(LayerStackError::from)?;

        let current = self.read_active_manifest_unlocked()?;
        if current != active {
            let _ = remove_path(&compact_parent_path);
            let _ = std::fs::remove_file(layer_digest_path(
                &self.storage_root,
                &compact_parent_layer.layer_id,
            ));
            return Err(LayerStackError::ManifestConflict {
                expected: active.version,
                found: current.version,
            });
        }
        if let Err(err) = write_manifest(
            self.storage_root.join(ACTIVE_MANIFEST_FILE),
            &active_manifest_after,
        ) {
            let _ = remove_path(&compact_parent_path);
            let _ = std::fs::remove_file(layer_digest_path(
                &self.storage_root,
                &compact_parent_layer.layer_id,
            ));
            return Err(err);
        }

        let removed = {
            let mut leases = lock_shared_registry(&self.leases)?;
            let Some(old_lease) = leases.retarget(lease_id, lease_manifest_after.clone()) else {
                return Err(LayerStackError::InvalidLeaseOwner(format!(
                    "lease not found after parent compaction: {lease_id}"
                )));
            };
            remove_unreferenced_layer_candidates_locked(
                &self.storage_root,
                &leases,
                &old_lease.manifest.layers,
            )?
        };

        Ok(LeaseParentCompactionOutcome {
            lease_manifest: Some(lease_manifest_after),
            active_manifest: Some(active_manifest_after.clone()),
            compact_parent_layer: Some(compact_parent_layer),
            compacted_parent_layer_count: parent_layers.len(),
            removed_layer_count: removed.len(),
            bytes_added,
            lease_depth_before,
            lease_depth_after: 2,
            active_depth_before,
            active_depth_after: active_manifest_after.depth(),
        })
    }

    #[doc(hidden)]
    pub fn copy_through_active_for_depth_guard(
        &mut self,
        max_depth: usize,
    ) -> Result<LeaseAwareCopyThroughOutcome, LayerStackError> {
        if max_depth == 0 {
            return Err(LayerStackError::InvalidSquashPlan(
                "max_depth must be positive".to_owned(),
            ));
        }
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        let active_depth_before = active.depth();
        let protected_layers = {
            let leases = lock_shared_registry(&self.leases)?;
            leases.leased_layers()
        };
        let protected_pinned_bytes = self.layer_payload_sum(&protected_layers)?;
        if active_depth_before <= max_depth || active.layers.is_empty() {
            return Ok(LeaseAwareCopyThroughOutcome {
                manifest: None,
                protected_layer_count: protected_layers.len(),
                checkpoint_count: 0,
                removed_layer_count: 0,
                bytes_added: 0,
                protected_pinned_bytes,
                active_depth_before,
                active_depth_after: active_depth_before,
            });
        }

        let checkpoint = self.build_copy_through_checkpoint(&active)?;
        let bytes_added = storage_bytes(&resolve_layer_path(&self.storage_root, &checkpoint.path))?;
        let manifest = Manifest::new(
            active.version + 1,
            vec![checkpoint.clone()],
            active.schema_version,
        )
        .map_err(LayerStackError::from)?;
        if let Err(err) = write_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE), &manifest) {
            let _ = remove_path(&resolve_layer_path(&self.storage_root, &checkpoint.path));
            let _ =
                std::fs::remove_file(layer_digest_path(&self.storage_root, &checkpoint.layer_id));
            return Err(err);
        }
        let removed = {
            let leases = lock_shared_registry(&self.leases)?;
            remove_unreferenced_layer_candidates_locked(
                &self.storage_root,
                &leases,
                &active.layers,
            )?
        };
        Ok(LeaseAwareCopyThroughOutcome {
            manifest: Some(manifest),
            protected_layer_count: protected_layers.len(),
            checkpoint_count: 1,
            removed_layer_count: removed.len(),
            bytes_added,
            protected_pinned_bytes,
            active_depth_before,
            active_depth_after: 1,
        })
    }

    fn build_copy_through_checkpoint(
        &self,
        manifest: &Manifest,
    ) -> Result<LayerRef, LayerStackError> {
        self.build_projected_checkpoint(manifest)
    }

    fn build_projected_checkpoint(&self, manifest: &Manifest) -> Result<LayerRef, LayerStackError> {
        let next_version = manifest.version.saturating_add(1);
        let (layer_id, staging_dir, layer_dir) =
            allocate_layer_dirs(&self.storage_root, 'B', next_version)?;
        if let Err(err) = self.view.project(&staging_dir, manifest) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err);
        }
        if let Some(parent) = layer_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(err) = std::fs::rename(&staging_dir, &layer_dir) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err.into());
        }
        fsync_tree_files(&layer_dir)?;
        if let Some(parent) = layer_dir.parent() {
            fsync_dir(parent)?;
        }
        write_layer_digest(&self.storage_root, &layer_id, &manifest_root_hash(manifest))?;
        Ok(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        })
    }

    fn layer_payload_sum(&self, layers: &[LayerRef]) -> Result<u64, LayerStackError> {
        layers.iter().try_fold(0_u64, |total, layer| {
            Ok(total + storage_bytes(&resolve_layer_path(&self.storage_root, &layer.path))?)
        })
    }

    #[must_use]
    pub fn leased_layers(&self) -> Vec<LayerRef> {
        lock_shared_registry_recover(&self.leases).leased_layers()
    }

    #[must_use]
    pub fn active_lease_count(&self) -> usize {
        lock_shared_registry_recover(&self.leases).active_count()
    }

    pub fn storage_metrics(&self) -> Result<LayerStackStorageMetrics, LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let root = &self.storage_root;
        Ok(LayerStackStorageMetrics {
            layer_dirs: count_dirs(&root.join(LAYERS_DIR))?,
            staging_dirs: count_dirs(&root.join(STAGING_DIR))?,
            storage_bytes: storage_bytes(root)?,
        })
    }

    pub fn commit_to_workspace(
        &mut self,
        workspace_root: &Path,
    ) -> Result<(Manifest, BTreeMap<String, f64>), LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let total_start = Instant::now();
        if !workspace_root.is_dir() {
            return Err(LayerStackError::WorkspaceBinding(format!(
                "workspace_root does not exist: {}",
                workspace_root.display()
            )));
        }
        if lock_shared_registry(&self.leases)?.active_count() > 0 {
            return Err(LayerStackError::Storage(
                "commit_to_workspace blocked by active leases".to_owned(),
            ));
        }

        let active = self.read_active_manifest_unlocked()?;
        let projection = self.commit_projection_dir()?;
        let staged_storage = self.commit_staged_storage_dir()?;
        let mut timings = BTreeMap::new();
        let storage_root = self.storage_root.clone();
        let view = &mut self.view;
        let mut journal_requires_recovery = false;
        let outcome = (|| {
            let workspace_root_for_journal = workspace_root
                .canonicalize()
                .unwrap_or_else(|_| workspace_root.to_path_buf());
            let project_start = Instant::now();
            view.project(&projection, &active)?;
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.project_s",
                project_start,
            );

            let rebuild_start = Instant::now();
            let _ = build_workspace_base_from_snapshot(
                &staged_storage,
                &storage_root,
                workspace_root,
                &projection,
                false,
            )?;
            write_commit_workspace_journal(
                &storage_root,
                CommitWorkspacePhase::Staged,
                &staged_storage,
            )?;

            write_commit_workspace_journal(
                &storage_root,
                CommitWorkspacePhase::ReplacingWorkspace {
                    workspace_root: workspace_root_for_journal.to_string_lossy().into_owned(),
                },
                &staged_storage,
            )?;
            journal_requires_recovery = true;
            let replace_start = Instant::now();
            replace_workspace_contents(workspace_root, &projection)?;
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.replace_workspace_s",
                replace_start,
            );
            write_commit_workspace_journal(
                &storage_root,
                CommitWorkspacePhase::WorkspaceReplaced,
                &staged_storage,
            )?;
            journal_requires_recovery = true;

            install_staged_workspace_commit(&storage_root, &staged_storage)?;
            journal_requires_recovery = false;
            *view = MergedView::new(storage_root.clone());
            let new_manifest = read_manifest(storage_root.join(ACTIVE_MANIFEST_FILE))?;
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.rebuild_base_s",
                rebuild_start,
            );
            record_elapsed(
                &mut timings,
                "layer_stack.commit_to_workspace.total_s",
                total_start,
            );
            Ok(new_manifest)
        })();
        let _ = remove_path(&projection);
        if outcome.is_err() && !journal_requires_recovery {
            let _ = remove_path(&staged_storage);
            let _ = remove_path(&commit_workspace_journal_path(&storage_root));
        }
        outcome.map(|manifest| (manifest, timings))
    }

    pub fn read_bytes(&self, path: &str) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        self.read_bytes_limited(path, usize::MAX)
    }

    pub fn read_bytes_limited(
        &self,
        path: &str,
        max_bytes: usize,
    ) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest_unlocked()?;
        self.view.read_bytes_limited(path, &manifest, max_bytes)
    }

    pub fn read_text(&self, path: &str) -> Result<(String, bool), LayerStackError> {
        let (bytes, exists) = self.read_bytes(path)?;
        if !exists {
            return Ok((String::new(), false));
        }
        let bytes = bytes.unwrap_or_default();
        let text =
            String::from_utf8(bytes).map_err(|err| LayerStackError::Storage(err.to_string()))?;
        Ok((text, true))
    }

    pub fn publish_layer(&mut self, changes: &[LayerChange]) -> Result<Manifest, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        if changes.is_empty() {
            return Ok(active);
        }

        let digest = try_layer_digest(changes)?;
        if self.head_layer_digest(&active)? == Some(digest.clone()) {
            return Ok(active);
        }

        self.take_publish_failpoint_marker()?;

        let next_version = active.version + 1;
        let (layer_id, staging_dir, layer_dir) =
            allocate_layer_dirs(&self.storage_root, 'L', next_version)?;
        std::fs::create_dir_all(&staging_dir)?;
        if let Err(err) = write_layer_changes(&staging_dir, changes)
            .and_then(|()| fsync_tree_files(&staging_dir))
            .and_then(|()| fsync_dir(&staging_dir))
        {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err);
        }

        if let Err(err) = std::fs::rename(&staging_dir, &layer_dir) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err.into());
        }
        if let Some(parent) = layer_dir.parent() {
            fsync_dir(parent)?;
        }

        if let Err(err) = write_layer_digest(&self.storage_root, &layer_id, &digest) {
            let _ = remove_path(&layer_dir);
            return Err(err);
        }

        let latest = self.read_active_manifest_unlocked()?;
        if latest != active {
            let _ = remove_path(&layer_dir);
            let _ = std::fs::remove_file(layer_digest_path(&self.storage_root, &layer_id));
            return Err(LayerStackError::ManifestConflict {
                expected: active.version,
                found: latest.version,
            });
        }

        let mut layers = Vec::with_capacity(active.layers.len() + 1);
        layers.push(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        });
        layers.extend(active.layers);
        let manifest = Manifest::new(next_version, layers, active.schema_version)
            .map_err(LayerStackError::from)?;
        if let Err(err) = write_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE), &manifest) {
            let _ = remove_path(&layer_dir);
            let _ = std::fs::remove_file(layer_digest_path(&self.storage_root, &layer_id));
            return Err(err);
        }
        Ok(manifest)
    }

    fn head_layer_digest(&self, manifest: &Manifest) -> Result<Option<String>, LayerStackError> {
        let Some(head) = manifest.layers.first() else {
            return Ok(None);
        };
        let path = layer_digest_path(&self.storage_root, &head.layer_id);
        match std::fs::read_to_string(path) {
            Ok(value) => Ok(Some(value)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn take_publish_failpoint_marker(&self) -> Result<(), LayerStackError> {
        if std::env::var(ENABLE_TEST_FAILPOINTS_ENV).ok().as_deref() != Some("1") {
            return Ok(());
        }
        let marker = self
            .storage_root
            .join(LAYER_METADATA_DIR)
            .join(FAIL_NEXT_PUBLISH_MARKER_FILE);
        match std::fs::remove_file(&marker) {
            Ok(()) => Err(LayerStackError::Storage(
                "injected layerstack publish failure".to_owned(),
            )),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    fn commit_projection_dir(&self) -> Result<PathBuf, LayerStackError> {
        allocate_commit_projection_dir(&self.storage_root, "projected")
    }

    pub(crate) fn commit_staged_storage_dir(&self) -> Result<PathBuf, LayerStackError> {
        let parent = self.storage_root.parent().ok_or_else(|| {
            LayerStackError::Storage(format!(
                "storage root has no parent: {}",
                self.storage_root.display()
            ))
        })?;
        std::fs::create_dir_all(parent)?;
        let prefix = staged_storage_name_prefix(&self.storage_root);
        for _ in 0..100 {
            let candidate =
                parent.join(format!("{prefix}{}-{}", std::process::id(), next_unique()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(candidate),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err.into()),
            }
        }
        Err(LayerStackError::Storage(
            "could not allocate staged commit storage directory".to_owned(),
        ))
    }
}

fn apply_delta_change(by_path: &mut BTreeMap<LayerPath, LayerChange>, change: LayerChange) {
    let path = change.path().clone();
    by_path.retain(|candidate, _| !is_strict_descendant(candidate, &path));
    if matches!(change, LayerChange::OpaqueDir { .. }) {
        by_path.retain(|candidate, _| !is_same_or_descendant(candidate, &path));
    }
    by_path.insert(path, change);
}

fn find_layer_sequence(haystack: &[LayerRef], needle: &[LayerRef]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn is_same_or_descendant(candidate: &LayerPath, ancestor: &LayerPath) -> bool {
    candidate == ancestor || is_strict_descendant(candidate, ancestor)
}

fn is_strict_descendant(candidate: &LayerPath, ancestor: &LayerPath) -> bool {
    let candidate = candidate.as_str();
    let ancestor = ancestor.as_str();
    candidate
        .strip_prefix(ancestor)
        .is_some_and(|suffix| suffix.starts_with('/'))
}
