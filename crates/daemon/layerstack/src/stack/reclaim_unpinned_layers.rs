use std::collections::BTreeSet;

use crate::error::LayerStackError;
use crate::model::{LayerRef, Manifest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReclaimUnpinnedLayersCheckpointMode {
    View,
    DeltaRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReclaimingInterval {
    pub(crate) layers: Vec<LayerRef>,
    pub(crate) checkpoint_mode: ReclaimUnpinnedLayersCheckpointMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReclaimUnpinnedLayersPlanEntry {
    KeepProtected(LayerRef),
    KeepUnleased(LayerRef),
    ReclaimingInterval(ReclaimingInterval),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReclaimUnpinnedLayersPlan {
    pub(crate) active_version: i64,
    pub(crate) active_layer_count: usize,
    pub(crate) protected_layer_count: usize,
    pub(crate) kept_unleased_layer_count: usize,
    pub(crate) reclaiming_interval_count: usize,
    pub(crate) reclaiming_layer_count: usize,
    pub(crate) entries: Vec<ReclaimUnpinnedLayersPlanEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimUnpinnedLayersOutcome {
    pub manifest: Option<Manifest>,
    pub protected_layer_count: usize,
    pub planned_reclaiming_interval_count: usize,
    pub view_checkpoint_count: usize,
    pub delta_checkpoint_count: usize,
    pub skipped_delta_interval_count: usize,
    pub removed_layer_count: usize,
    pub active_depth_before: usize,
    pub active_depth_after: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimUnpinnedLayersCopyThroughOutcome {
    pub manifest: Option<Manifest>,
    pub protected_layer_count: usize,
    pub checkpoint_count: usize,
    pub removed_layer_count: usize,
    pub bytes_added: u64,
    pub protected_pinned_bytes: u64,
    pub active_depth_before: usize,
    pub active_depth_after: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseParentCompactionOutcome {
    pub lease_manifest: Option<Manifest>,
    pub active_manifest: Option<Manifest>,
    pub compact_parent_layer: Option<LayerRef>,
    pub compacted_parent_layer_count: usize,
    pub removed_layer_count: usize,
    pub bytes_added: u64,
    pub lease_depth_before: usize,
    pub lease_depth_after: usize,
    pub active_depth_before: usize,
    pub active_depth_after: usize,
}

pub(crate) fn plan_reclaim_unpinned_layers(
    active_manifest: &Manifest,
    protected_layers: &[LayerRef],
    min_reclaiming_interval_layers: usize,
) -> Result<ReclaimUnpinnedLayersPlan, LayerStackError> {
    if min_reclaiming_interval_layers == 0 {
        return Err(LayerStackError::InvalidSquashPlan(
            "min_reclaiming_interval_layers must be positive".to_owned(),
        ));
    }

    let protected = protected_layers.iter().collect::<BTreeSet<_>>();
    let active_protected = active_manifest
        .layers
        .iter()
        .filter(|layer| protected.contains(layer))
        .count();
    let mut entries = Vec::new();
    let mut run = Vec::new();
    let mut kept_unleased_layer_count = 0;
    let mut reclaiming_interval_count = 0;
    let mut reclaiming_layer_count = 0;

    for layer in &active_manifest.layers {
        if protected.contains(layer) {
            flush_unleased_run(
                &mut entries,
                &mut run,
                min_reclaiming_interval_layers,
                true,
                &mut kept_unleased_layer_count,
                &mut reclaiming_interval_count,
                &mut reclaiming_layer_count,
            );
            entries.push(ReclaimUnpinnedLayersPlanEntry::KeepProtected(layer.clone()));
        } else {
            run.push(layer.clone());
        }
    }
    flush_unleased_run(
        &mut entries,
        &mut run,
        min_reclaiming_interval_layers,
        false,
        &mut kept_unleased_layer_count,
        &mut reclaiming_interval_count,
        &mut reclaiming_layer_count,
    );

    Ok(ReclaimUnpinnedLayersPlan {
        active_version: active_manifest.version,
        active_layer_count: active_manifest.layers.len(),
        protected_layer_count: active_protected,
        kept_unleased_layer_count,
        reclaiming_interval_count,
        reclaiming_layer_count,
        entries,
    })
}

fn flush_unleased_run(
    entries: &mut Vec<ReclaimUnpinnedLayersPlanEntry>,
    run: &mut Vec<LayerRef>,
    min_reclaiming_interval_layers: usize,
    has_kept_lower_layer: bool,
    kept_unleased_layer_count: &mut usize,
    reclaiming_interval_count: &mut usize,
    reclaiming_layer_count: &mut usize,
) {
    if run.is_empty() {
        return;
    }
    if run.len() < min_reclaiming_interval_layers {
        *kept_unleased_layer_count += run.len();
        entries.extend(
            run.drain(..)
                .map(ReclaimUnpinnedLayersPlanEntry::KeepUnleased),
        );
        return;
    }

    let layers = std::mem::take(run);
    *reclaiming_interval_count += 1;
    *reclaiming_layer_count += layers.len();
    entries.push(ReclaimUnpinnedLayersPlanEntry::ReclaimingInterval(
        ReclaimingInterval {
            layers,
            checkpoint_mode: if has_kept_lower_layer {
                ReclaimUnpinnedLayersCheckpointMode::DeltaRequired
            } else {
                ReclaimUnpinnedLayersCheckpointMode::View
            },
        },
    ));
}
