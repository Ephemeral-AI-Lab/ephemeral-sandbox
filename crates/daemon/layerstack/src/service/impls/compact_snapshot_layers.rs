use crate::commit::CommitError;
use crate::{LayerStack, Manifest};

use super::super::model::{CompactSnapshotLayersRequest, CompactSnapshotLayersResult};
use super::super::support::snapshot_manifest_preserving_layer_ids;

pub fn compact_snapshot_layers(
    request: CompactSnapshotLayersRequest<'_>,
) -> Result<CompactSnapshotLayersResult, CommitError> {
    let manifest = snapshot_manifest_preserving_layer_ids(
        request.root,
        request.snapshot_manifest_version,
        request.snapshot_layer_paths,
    )?;
    let mut stack = LayerStack::open(request.root.to_path_buf())?;
    let layer = stack.build_compaction_checkpoint(&manifest)?;
    let compact_manifest = Manifest::new(
        manifest.version,
        vec![layer.clone()],
        manifest.schema_version,
    )?;
    let layer_path = crate::fs::resolve_layer_path(request.root, &layer.path);
    Ok(CompactSnapshotLayersResult {
        manifest: compact_manifest,
        layer_paths: vec![layer_path],
        before_layer_count: manifest.layers.len(),
        after_layer_count: 1,
    })
}
