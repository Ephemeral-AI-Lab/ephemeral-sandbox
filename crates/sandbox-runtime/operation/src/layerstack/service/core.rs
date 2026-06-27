use std::path::PathBuf;

use sandbox_observability::Observer;

use crate::layerstack::LayerStackServiceError;

pub struct LayerStackService {
    pub(crate) layer_stack_root: PathBuf,
    pub(crate) binding: sandbox_runtime_layerstack::WorkspaceBinding,
    pub(crate) obs: Observer,
}

impl LayerStackService {
    pub fn new(layer_stack_root: PathBuf, obs: Observer) -> Result<Self, LayerStackServiceError> {
        let binding = sandbox_runtime_layerstack::require_workspace_binding(&layer_stack_root)
            .map_err(|error| LayerStackServiceError::Init {
                layer_stack_root: layer_stack_root.clone(),
                error: error.to_string(),
            })?;
        Ok(Self {
            layer_stack_root,
            binding,
            obs,
        })
    }

    #[must_use]
    pub fn layer_stack_root(&self) -> &std::path::Path {
        &self.layer_stack_root
    }

    #[must_use]
    pub fn binding(&self) -> &sandbox_runtime_layerstack::WorkspaceBinding {
        &self.binding
    }
}
