use std::path::Path;

use crate::{LayerStack, LayerStackError};

use super::super::model::Snapshot;
use super::super::support::snapshot_from_manifest;

pub fn get_snapshot(root: &Path) -> Result<Snapshot, LayerStackError> {
    let manifest = LayerStack::open(root.to_path_buf())?.read_active_manifest()?;
    Ok(snapshot_from_manifest(root, manifest))
}
