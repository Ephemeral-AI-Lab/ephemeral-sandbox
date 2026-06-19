use crate::error::LayerStackError;
use crate::model::LayerRef;

use super::{CheckpointSegment, SquashPlanEntry};

pub(super) fn segment_around_lease_heads(
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
