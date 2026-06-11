use std::path::PathBuf;

use crate::model::{LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};

use crate::error::LayerStackError;
use crate::fs::{allocate_layer_dirs, check_layer_path, fsync_dir, resolve_layer_path};
use crate::{MergedView, LAYERS_DIR};

pub const CHECKPOINT_ID_PREFIX: char = 'B';

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointSegment {
    pub layers: Vec<LayerRef>,
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
pub enum SquashPlanEntry {
    Keep(LayerRef),
    Segment(CheckpointSegment),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquashPlan {
    pub active_version: i64,
    pub active_layers: Vec<LayerRef>,
    pub entries: Vec<SquashPlanEntry>,
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
    pub fn checkpoint_segments(&self) -> Vec<&CheckpointSegment> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                SquashPlanEntry::Segment(s) => Some(s),
                SquashPlanEntry::Keep(_) => None,
            })
            .collect()
    }
}

#[derive(Debug)]
pub struct LayerCheckpointSquasher {
    storage_root: PathBuf,
    view: MergedView,
}

impl LayerCheckpointSquasher {
    #[must_use]
    pub fn new(storage_root: PathBuf) -> Self {
        Self {
            view: MergedView::new(storage_root.clone()),
            storage_root,
        }
    }

    pub fn plan(
        &self,
        active_manifest: &Manifest,
        max_depth: usize,
        lease_head_layers: &[LayerRef],
        min_reduction: usize,
    ) -> Result<Option<SquashPlan>, LayerStackError> {
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
            return Ok(None);
        }

        let entries = segment_around_lease_heads(&active_manifest.layers, lease_head_layers)?;
        if entries.len() >= active_manifest.layers.len() {
            return Ok(None);
        }
        if active_manifest.layers.len() - entries.len() < min_reduction {
            return Ok(None);
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
            return Ok(None);
        }
        Ok(Some(plan))
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

fn segment_around_lease_heads(
    layers: &[LayerRef],
    lease_head_layers: &[LayerRef],
) -> Result<Vec<SquashPlanEntry>, LayerStackError> {
    let mut entries = Vec::new();
    let mut run = Vec::new();
    for layer in layers {
        if lease_head_layers.contains(layer) {
            flush_run(&mut entries, &mut run)?;
            entries.push(SquashPlanEntry::Keep(layer.clone()));
        } else {
            run.push(layer.clone());
        }
    }
    flush_run(&mut entries, &mut run)?;
    Ok(entries)
}

fn flush_run(
    entries: &mut Vec<SquashPlanEntry>,
    run: &mut Vec<LayerRef>,
) -> Result<(), LayerStackError> {
    match run.len() {
        0 => {}
        1 => entries.push(SquashPlanEntry::Keep(run[0].clone())),
        _ => entries.push(SquashPlanEntry::Segment(CheckpointSegment::new(
            std::mem::take(run),
        )?)),
    }
    run.clear();
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/squash.rs"]
mod tests;
