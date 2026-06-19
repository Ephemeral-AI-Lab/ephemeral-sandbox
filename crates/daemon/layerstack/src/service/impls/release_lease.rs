use std::path::Path;

use crate::{LayerStack, LayerStackError};

pub fn release_lease(root: &Path, lease_id: &str) -> Result<(), LayerStackError> {
    let _released = LayerStack::open(root.to_path_buf())?.release_lease(lease_id)?;
    Ok(())
}
