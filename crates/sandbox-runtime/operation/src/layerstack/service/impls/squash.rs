use crate::layerstack::{LayerStackService, LayerStackServiceError, SquashLayerStackResult};
use crate::observability::{measure_optional_if, span_keys, OperationTrace};

use super::publish_changes::{layer_paths, revision_from_manifest};

impl LayerStackService {
    pub fn squash(
        &self,
        trace: Option<&OperationTrace>,
    ) -> Result<SquashLayerStackResult, LayerStackServiceError> {
        let mut stack = measure_optional_if(trace, span_keys::LAYERSTACK_SQUASH_OPEN_STACK, || {
            sandbox_runtime_layerstack::LayerStack::open(self.layer_stack_root.clone())
        })
        .map_err(|error| LayerStackServiceError::LayerStack {
            operation: "open",
            error,
        })?;
        let outcome =
            measure_optional_if(trace, span_keys::LAYERSTACK_SQUASH_COMPACT_STACK, || {
                stack.squash()
            })
            .map_err(|error| LayerStackServiceError::LayerStack {
                operation: "squash",
                error,
            })?;
        let Some(manifest) = outcome.manifest else {
            return Ok(SquashLayerStackResult {
                squashed: false,
                revision: None,
                layer_paths: Vec::new(),
                lease_release_error: outcome.lease_release_error.map(|err| err.to_string()),
            });
        };
        Ok(SquashLayerStackResult {
            squashed: true,
            revision: Some(revision_from_manifest(&manifest)),
            layer_paths: layer_paths(&self.layer_stack_root, &manifest),
            lease_release_error: outcome.lease_release_error.map(|err| err.to_string()),
        })
    }
}
