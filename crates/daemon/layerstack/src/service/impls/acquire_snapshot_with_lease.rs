use std::path::Path;

use crate::{LayerStack, LayerStackError};

use super::super::model::LeasedSnapshot;
use super::super::support::snapshot_from_lease;

pub fn acquire_snapshot_with_lease(
    root: &Path,
    request_id: &str,
) -> Result<LeasedSnapshot, LayerStackError> {
    let lease = LayerStack::open(root.to_path_buf())?.acquire_snapshot(request_id)?;
    Ok(snapshot_from_lease(lease))
}
