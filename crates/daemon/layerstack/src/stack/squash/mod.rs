use std::path::PathBuf;

use crate::model::{LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};

use crate::error::LayerStackError;
use crate::fs::{allocate_layer_dirs, check_layer_path, fsync_dir, resolve_layer_path};
use crate::{MergedView, LAYERS_DIR};

mod auto_squash;
mod ops;
mod planning;

pub(crate) use auto_squash::{run_auto_squash, AutoSquashTrace};
use planning::segment_around_lease_heads;

pub(crate) const CHECKPOINT_ID_PREFIX: char = 'B';

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckpointSegment {
    pub(crate) layers: Vec<LayerRef>,
}

impl CheckpointSegment {
    pub(crate) fn new(layers: Vec<LayerRef>) -> Result<Self, LayerStackError> {
        if layers.len() <= 1 {
            return Err(LayerStackError::InvalidSquashPlan(
                "checkpoint segments must contain at least two layers".to_owned(),
            ));
        }
        Ok(Self { layers })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SquashPlanEntry {
    Keep(LayerRef),
    Segment(CheckpointSegment),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SquashPlan {
    pub(crate) active_version: i64,
    pub(crate) active_layers: Vec<LayerRef>,
    pub(crate) entries: Vec<SquashPlanEntry>,
}

impl SquashPlan {
    pub(crate) fn new(
        active_version: i64,
        active_layers: Vec<LayerRef>,
        entries: Vec<SquashPlanEntry>,
    ) -> Result<Self, LayerStackError> {
        if active_layers.is_empty() {
            return Err(LayerStackError::InvalidSquashPlan(
                "active_layers must not be empty".to_owned(),
            ));
        }
        if entries.is_empty() {
            return Err(LayerStackError::InvalidSquashPlan(
                "entries must not be empty".to_owned(),
            ));
        }
        if !entries
            .iter()
            .any(|e| matches!(e, SquashPlanEntry::Segment(_)))
        {
            return Err(LayerStackError::InvalidSquashPlan(
                "squash plans must include at least one checkpoint segment".to_owned(),
            ));
        }
        Ok(Self {
            active_version,
            active_layers,
            entries,
        })
    }

    #[must_use]
    pub(crate) fn checkpoint_segments(&self) -> Vec<&CheckpointSegment> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                SquashPlanEntry::Segment(s) => Some(s),
                SquashPlanEntry::Keep(_) => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SquashPlanSkipReason {
    TooShallow,
    LeaseBlocked,
    MinReductionUnmet,
    MaxDepthStillExceeded,
}

impl SquashPlanSkipReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::TooShallow => "too_shallow",
            Self::LeaseBlocked => "lease_blocked",
            Self::MinReductionUnmet => "min_reduction_unmet",
            Self::MaxDepthStillExceeded => "max_depth_still_exceeded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SquashPlanDecision {
    pub(crate) plan: Option<SquashPlan>,
    pub(crate) skip_reason: Option<SquashPlanSkipReason>,
}

#[derive(Debug)]
pub(crate) struct LayerCheckpointSquasher {
    storage_root: PathBuf,
    view: MergedView,
}

impl LayerCheckpointSquasher {
    #[must_use]
    pub(crate) fn new(storage_root: PathBuf) -> Self {
        Self {
            view: MergedView::new(storage_root.clone()),
            storage_root,
        }
    }

    pub(crate) fn plan(
        &self,
        active_manifest: &Manifest,
        max_depth: usize,
        lease_head_layers: &[LayerRef],
        min_reduction: usize,
    ) -> Result<Option<SquashPlan>, LayerStackError> {
        Ok(self
            .plan_decision(active_manifest, max_depth, lease_head_layers, min_reduction)?
            .plan)
    }

    pub(crate) fn plan_decision(
        &self,
        active_manifest: &Manifest,
        max_depth: usize,
        lease_head_layers: &[LayerRef],
        min_reduction: usize,
    ) -> Result<SquashPlanDecision, LayerStackError> {
        if max_depth == 0 {
            return Err(LayerStackError::InvalidSquashPlan(
                "max_depth must be positive".to_owned(),
            ));
        }
        if min_reduction == 0 {
            return Err(LayerStackError::InvalidSquashPlan(
                "min_reduction must be positive".to_owned(),
            ));
        }
        if active_manifest.layers.len() <= max_depth {
            return Ok(SquashPlanDecision {
                plan: None,
                skip_reason: Some(SquashPlanSkipReason::TooShallow),
            });
        }

        let entries = segment_around_lease_heads(&active_manifest.layers, lease_head_layers)?;
        if entries.len() >= active_manifest.layers.len() {
            return Ok(SquashPlanDecision {
                plan: None,
                skip_reason: Some(if lease_head_layers.is_empty() {
                    SquashPlanSkipReason::MinReductionUnmet
                } else {
                    SquashPlanSkipReason::LeaseBlocked
                }),
            });
        }
        if active_manifest.layers.len() - entries.len() < min_reduction {
            return Ok(SquashPlanDecision {
                plan: None,
                skip_reason: Some(SquashPlanSkipReason::MinReductionUnmet),
            });
        }
        let plan = SquashPlan::new(
            active_manifest.version,
            active_manifest.layers.clone(),
            entries,
        )?;
        if plan.entries.len() > max_depth
            && plan
                .checkpoint_segments()
                .iter()
                .all(|segment| segment.layers.len() <= max_depth)
        {
            return Ok(SquashPlanDecision {
                plan: None,
                skip_reason: Some(SquashPlanSkipReason::MaxDepthStillExceeded),
            });
        }
        Ok(SquashPlanDecision {
            plan: Some(plan),
            skip_reason: None,
        })
    }

    pub(crate) fn build_checkpoint(
        &self,
        segment: &CheckpointSegment,
        active_version: i64,
    ) -> Result<LayerRef, LayerStackError> {
        let (layer_id, staging_dir, layer_dir) =
            allocate_layer_dirs(&self.storage_root, CHECKPOINT_ID_PREFIX, active_version + 1)?;
        let segment_manifest = Manifest::new(
            active_version,
            segment.layers.clone(),
            MANIFEST_SCHEMA_VERSION,
        )
        .map_err(LayerStackError::from)?;
        if let Err(err) = self.view.project(&staging_dir, &segment_manifest) {
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
        Ok(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        })
    }

    pub(crate) fn relabel_checkpoint(
        &self,
        checkpoint: &LayerRef,
        manifest_version: i64,
    ) -> Result<LayerRef, LayerStackError> {
        let current = self.layer_path(checkpoint)?;
        if !current.exists() {
            return Err(LayerStackError::Storage(format!(
                "checkpoint layer is missing: {}",
                checkpoint.layer_id
            )));
        }
        let (layer_id, _staging_dir, layer_dir) =
            allocate_layer_dirs(&self.storage_root, CHECKPOINT_ID_PREFIX, manifest_version)?;
        if let Some(parent) = layer_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(current, &layer_dir)?;
        if let Some(parent) = layer_dir.parent() {
            fsync_dir(parent)?;
        }
        Ok(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        })
    }

    pub(crate) fn discard_checkpoint(&self, checkpoint: &LayerRef) -> Result<(), LayerStackError> {
        let path = self.layer_path(checkpoint)?;
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    fn layer_path(&self, layer: &LayerRef) -> Result<PathBuf, LayerStackError> {
        check_layer_path(&layer.path)?;
        Ok(resolve_layer_path(&self.storage_root, &layer.path))
    }
}

#[must_use]
pub(crate) fn manifest_prefix_before_plan<'m>(
    manifest: &'m Manifest,
    plan: &SquashPlan,
) -> Option<&'m [LayerRef]> {
    let planned_depth = plan.active_layers.len();
    if planned_depth > manifest.layers.len() {
        return None;
    }
    let split = manifest.layers.len() - planned_depth;
    if manifest.layers[split..] != plan.active_layers {
        return None;
    }
    Some(&manifest.layers[..split])
}
