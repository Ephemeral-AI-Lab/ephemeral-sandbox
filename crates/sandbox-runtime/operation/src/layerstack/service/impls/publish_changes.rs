use std::sync::PoisonError;

use sandbox_observability_telemetry::record::names;

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

        let base_version = base.manifest_version;
        let bytes = sandbox_runtime_layerstack::published_layer_bytes(&request.changes);
        let publication_id = request.publication_id;
        let owner = request.owner;
        let committed_changes = request.changes.clone();
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
        // Serialize the commit with the audit append so two publishes to one
        // path append in commit order (latest-event-wins stays correct, §13).
        let _audit_gate = self
            .audit_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let published = match self.obs.scope(names::LAYERSTACK_PUBLISH, |span| {
            span.attr("base", base_version).attr("bytes", bytes);
            let result = stack.publish_validated_changes(publish_request);
            match &result {
                Ok(published) => {
                    span.attr("revision", published.manifest.version)
                        .attr("no_op", published.no_op)
                        .attr("layers_added", if published.no_op { 0 } else { 1 });
                }
                Err(sandbox_runtime_layerstack::LayerStackError::ManifestConflict { .. }) => {
                    span.attr("reason", "manifest_conflict");
                }
                Err(_) => {}
            }
            result
        }) {
            Ok(published) => published,
            Err(error) => return Err(map_publish_error(error)),
        };
        // After the layer commits: map each resolved line's origin to an owner
        // string and append one audit event per path (G3 — never before commit).
        if !published.origin.is_empty() {
            self.file
                .record_layer_publish(&owner, &published.origin, &committed_changes);
        }
        drop(_audit_gate);
        if !published.no_op {
            self.enqueue_hidden_validation(
                &mut stack,
                publication_id,
                committed_changes,
                &published.manifest,
                bytes,
            );
        }
        Ok(PublishChangesResult {
            revision: revision_from_manifest(&published.manifest),
            manifest: published.manifest.clone(),
            layer_paths: layer_paths(&self.layer_stack_root, &published.manifest),
            route_summary: published.route_summary,
            no_op: published.no_op,
        })
    }

    fn enqueue_hidden_validation(
        &self,
        stack: &mut sandbox_runtime_layerstack::LayerStack,
        publication_id: [u8; 16],
        changes: Vec<sandbox_runtime_layerstack::LayerChange>,
        manifest: &sandbox_runtime_layerstack::Manifest,
        bytes: u64,
    ) {
        let Some(worker) = &self.hidden_validation else {
            return;
        };
        let Some(source_layer) = manifest.layers.first() else {
            worker.record_fallback();
            return;
        };
        let source_lease = match stack.acquire_snapshot(&format!(
            "hidden-validation:{}",
            correlation_id(publication_id)
        )) {
            Ok(lease) => lease,
            Err(_) => {
                worker.record_fallback();
                return;
            }
        };
        let source_lease_id = source_lease.lease_id;
        let publication = sandbox_runtime_layerstack::HiddenValidationPublication {
            publication_id,
            changes,
            source_layer_dir: self.layer_stack_root.join(&source_layer.path),
            public_root_hash: sandbox_runtime_layerstack::manifest_root_hash(manifest),
        };
        if let Err(source_lease_id) = worker.submit(publication, source_lease_id, bytes) {
            if stack.release_lease(&source_lease_id).is_err() {
                worker.record_fallback();
            }
        }
    }
}

fn correlation_id(publication_id: [u8; 16]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(32);
    for byte in publication_id {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
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
