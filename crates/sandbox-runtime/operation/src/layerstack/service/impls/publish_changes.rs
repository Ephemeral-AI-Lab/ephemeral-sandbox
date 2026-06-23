use crate::layerstack::{
    LayerStackRevision, LayerStackService, LayerStackServiceError, PublishChangesRequest,
    PublishChangesResult,
};

impl LayerStackService {
    pub fn publish_changes(
        &self,
        request: PublishChangesRequest,
    ) -> Result<PublishChangesResult, LayerStackServiceError> {
        let base = revision_from_manifest(&request.base_manifest);
        if request.expected_base != base {
            return Err(LayerStackServiceError::InvalidBaseRevision {
                expected: request.expected_base,
                base,
            });
        }

        let publish_request = sandbox_runtime_layerstack::PublishValidatedChangesRequest {
            base: sandbox_runtime_layerstack::PublishBase {
                manifest: request.base_manifest,
                revision: sandbox_runtime_layerstack::PublishBaseRevision {
                    manifest_version: base.manifest_version,
                    root_hash: base.root_hash.clone(),
                    layer_count: base.layer_count,
                },
            },
            changes: request.changes,
            protected_drops: request.protected_drops,
        };
        let mut stack = sandbox_runtime_layerstack::LayerStack::open(self.layer_stack_root.clone())
            .map_err(|error| LayerStackServiceError::LayerStack {
                operation: "open",
                error,
            })?;
        let published = match stack.publish_validated_changes(publish_request) {
            Ok(published) => published,
            Err(error) => return Err(map_publish_error(error)),
        };
        Ok(PublishChangesResult {
            revision: revision_from_manifest(&published.manifest),
            manifest: published.manifest.clone(),
            layer_paths: layer_paths(&self.layer_stack_root, &published.manifest),
            route_summary: published.route_summary,
            no_op: published.no_op,
        })
    }
}

fn map_publish_error(error: sandbox_runtime_layerstack::LayerStackError) -> LayerStackServiceError {
    match error {
        sandbox_runtime_layerstack::LayerStackError::PublishRejected(rejection) => {
            LayerStackServiceError::PublishRejected { rejection }
        }
        error => LayerStackServiceError::LayerStack {
            operation: "publish",
            error,
        },
    }
}

pub(crate) fn revision_from_manifest(
    manifest: &sandbox_runtime_layerstack::Manifest,
) -> LayerStackRevision {
    LayerStackRevision {
        manifest_version: manifest.version,
        root_hash: sandbox_runtime_layerstack::manifest_root_hash(manifest),
        layer_count: manifest.layers.len(),
    }
}

pub(crate) fn layer_paths(
    layer_stack_root: &std::path::Path,
    manifest: &sandbox_runtime_layerstack::Manifest,
) -> Vec<std::path::PathBuf> {
    manifest
        .layers
        .iter()
        .map(|layer| layer_stack_root.join(&layer.path))
        .collect()
}
