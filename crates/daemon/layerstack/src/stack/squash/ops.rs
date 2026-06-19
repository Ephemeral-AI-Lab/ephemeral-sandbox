use super::{
    manifest_prefix_before_plan, LayerCheckpointSquasher, SquashPlanDecision, SquashPlanEntry,
};
use crate::error::LayerStackError;
use crate::fs::{next_unique, write_manifest};
use crate::model::{LayerRef, Manifest};
use crate::stack::lease::{lock_shared_registry, release_lease_locked};
use crate::stack::{LayerStack, SquashOutcome};
use crate::ACTIVE_MANIFEST_FILE;

impl LayerStack {
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
}
