use std::collections::BTreeMap;

use super::layer_read::read_layer_dir;
use super::layer_write::write_layer_changes;
use super::lease_cleanup::remove_unreferenced_layer_candidates_locked;
use super::leases::lock_shared_registry;
use super::reclaim_unpinned_layers::{
    plan_reclaim_unpinned_layers, LeaseParentCompactionOutcome,
    ReclaimUnpinnedLayersCheckpointMode, ReclaimUnpinnedLayersCopyThroughOutcome,
    ReclaimUnpinnedLayersOutcome, ReclaimUnpinnedLayersPlanEntry, ReclaimingInterval,
};
use super::squash::{CheckpointSegment, LayerCheckpointSquasher};
use super::view::layer_has_boundary_markers;
use super::LayerStack;
use crate::error::LayerStackError;
use crate::fs::{
    allocate_layer_dirs, fsync_dir, fsync_tree_files, layer_digest_path, remove_path,
    resolve_layer_path, storage_bytes, write_layer_digest, write_manifest,
};
use crate::model::{layer_digest, LayerChange, LayerPath, LayerRef, Manifest};
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR};

impl LayerStack {
    #[doc(hidden)]
    pub fn reclaim_unpinned_layers_with_view_checkpoints(
        &mut self,
        min_reclaiming_interval_layers: usize,
    ) -> Result<ReclaimUnpinnedLayersOutcome, LayerStackError> {
        self.reclaim_unpinned_layers_inner(min_reclaiming_interval_layers, false)
    }

    #[doc(hidden)]
    pub fn reclaim_unpinned_layers(
        &mut self,
        min_reclaiming_interval_layers: usize,
    ) -> Result<ReclaimUnpinnedLayersOutcome, LayerStackError> {
        self.reclaim_unpinned_layers_inner(min_reclaiming_interval_layers, true)
    }

    fn reclaim_unpinned_layers_inner(
        &mut self,
        min_reclaiming_interval_layers: usize,
        allow_delta_checkpoints: bool,
    ) -> Result<ReclaimUnpinnedLayersOutcome, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        let protected_layers = {
            let leases = lock_shared_registry(&self.leases)?;
            leases.leased_layers()
        };
        let plan = plan_reclaim_unpinned_layers(
            &active,
            &protected_layers,
            min_reclaiming_interval_layers,
        )?;
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
                    ReclaimUnpinnedLayersPlanEntry::KeepProtected(layer)
                    | ReclaimUnpinnedLayersPlanEntry::KeepUnleased(layer) => {
                        new_layers.push(layer.clone());
                    }
                    ReclaimUnpinnedLayersPlanEntry::ReclaimingInterval(interval)
                        if self.interval_can_use_view_checkpoint(interval)? =>
                    {
                        let segment = CheckpointSegment::new(interval.layers.clone())?;
                        let checkpoint = squasher.build_checkpoint(&segment, active.version)?;
                        checkpoints.push(checkpoint.clone());
                        new_layers.push(checkpoint);
                        removable_candidates.extend(interval.layers.clone());
                        view_checkpoint_count += 1;
                    }
                    ReclaimUnpinnedLayersPlanEntry::ReclaimingInterval(interval)
                        if allow_delta_checkpoints =>
                    {
                        let checkpoint = self.build_delta_checkpoint(interval, active.version)?;
                        checkpoints.push(checkpoint.clone());
                        new_layers.push(checkpoint);
                        removable_candidates.extend(interval.layers.clone());
                        delta_checkpoint_count += 1;
                    }
                    ReclaimUnpinnedLayersPlanEntry::ReclaimingInterval(interval) => {
                        skipped_delta_interval_count += 1;
                        new_layers.extend(interval.layers.clone());
                    }
                }
            }

            if view_checkpoint_count + delta_checkpoint_count == 0 {
                return Ok(ReclaimUnpinnedLayersOutcome {
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

            Ok(ReclaimUnpinnedLayersOutcome {
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
            let changes = read_layer_dir(&layer_dir).map_err(|err| {
                LayerStackError::Storage(format!(
                    "failed to read stored layer {} for delta checkpoint: {err}",
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
        if interval.checkpoint_mode == ReclaimUnpinnedLayersCheckpointMode::View {
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
    ) -> Result<ReclaimUnpinnedLayersCopyThroughOutcome, LayerStackError> {
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
            return Ok(ReclaimUnpinnedLayersCopyThroughOutcome {
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
        Ok(ReclaimUnpinnedLayersCopyThroughOutcome {
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
