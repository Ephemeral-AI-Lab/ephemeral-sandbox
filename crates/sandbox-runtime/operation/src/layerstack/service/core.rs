use std::path::PathBuf;

use sandbox_observability::Observer;

use crate::layerstack::LayerStackServiceError;

pub struct LayerStackService {
    pub(crate) layer_stack_root: PathBuf,
    pub(crate) obs: Observer,
}

impl LayerStackService {
    pub fn new(layer_stack_root: PathBuf, obs: Observer) -> Result<Self, LayerStackServiceError> {
        sandbox_runtime_layerstack::require_workspace_binding(&layer_stack_root).map_err(
            |error| LayerStackServiceError::Init {
                layer_stack_root: layer_stack_root.clone(),
                error: error.to_string(),
            },
        )?;
        Ok(Self {
            layer_stack_root,
            obs,
        })
    }

    #[must_use]
    pub fn layer_stack_root(&self) -> &std::path::Path {
        &self.layer_stack_root
    }
}
