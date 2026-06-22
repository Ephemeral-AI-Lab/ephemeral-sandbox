use crate::layerstack::{
    LayerStackRevision, LayerStackService, LayerStackServiceError, PublishChangesRequest,
    PublishChangesResult,
};
use tracing::{field, Span};

impl LayerStackService {
    pub fn publish_changes(
        &self,
        request: PublishChangesRequest,
    ) -> Result<PublishChangesResult, LayerStackServiceError> {
        let span = tracing::info_span!(
            "layerstack.publish_changes",
            status = field::Empty,
            error_kind = field::Empty,
            expected_manifest_version = request.expected_base.manifest_version,
            expected_layer_count = request.expected_base.layer_count as u64,
            base_manifest_version = request.base_manifest.version,
            base_layer_count = request.base_manifest.layers.len() as u64,
            change_count = request.changes.len() as u64,
            protected_drop_count = request.protected_drops.len() as u64,
            root_hash_matched = field::Empty,
            no_op = field::Empty,
            route_source_count = field::Empty,
            route_ignored_count = field::Empty,
            result_manifest_version = field::Empty,
            result_layer_count = field::Empty,
            rejection_reason = field::Empty,
            rejection_has_path = field::Empty,
            rejection_source_conflict = field::Empty,
            rejection_protected_drop = field::Empty,
            expected_fingerprint_kind = field::Empty,
            actual_fingerprint_kind = field::Empty,
            protected_drop_reason = field::Empty,
        );
        let _span_guard = span.enter();
        let result = self.publish_changes_inner(request);
        record_publish_result(&span, &result);
        result
    }

    fn publish_changes_inner(
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
                    root_hash: base.root_hash,
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
        let published = stack
            .publish_validated_changes(publish_request)
            .map_err(map_publish_error)?;
        Ok(PublishChangesResult {
            revision: revision_from_manifest(&published.manifest),
            manifest: published.manifest.clone(),
            layer_paths: layer_paths(&self.layer_stack_root, &published.manifest),
            route_summary: published.route_summary,
            no_op: published.no_op,
        })
    }
}

fn record_publish_result(
    span: &Span,
    result: &Result<PublishChangesResult, LayerStackServiceError>,
) {
    match result {
        Ok(result) => {
            span.record("status", "ok");
            span.record("no_op", result.no_op);
            span.record(
                "route_source_count",
                result.route_summary.source_count as u64,
            );
            span.record(
                "route_ignored_count",
                result.route_summary.ignored_count as u64,
            );
            span.record("result_manifest_version", result.revision.manifest_version);
            span.record("result_layer_count", result.revision.layer_count as u64);
        }
        Err(error) => {
            span.record("status", "error");
            span.record("error_kind", error.kind());
            record_publish_error_fields(span, error);
        }
    }
}

fn record_publish_error_fields(span: &Span, error: &LayerStackServiceError) {
    match error {
        LayerStackServiceError::InvalidBaseRevision { expected, base } => {
            span.record("root_hash_matched", expected.root_hash == base.root_hash);
        }
        LayerStackServiceError::PublishRejected { rejection } => {
            span.record("rejection_reason", publish_reject_reason(rejection.reason));
            span.record("rejection_has_path", rejection.path.is_some());
            span.record(
                "rejection_source_conflict",
                rejection.source_conflict.is_some(),
            );
            span.record(
                "rejection_protected_drop",
                rejection.protected_drop.is_some(),
            );
            if let Some(conflict) = rejection.source_conflict.as_ref() {
                span.record(
                    "expected_fingerprint_kind",
                    fingerprint_kind(&conflict.expected),
                );
                span.record(
                    "actual_fingerprint_kind",
                    fingerprint_kind(&conflict.actual),
                );
            }
            if let Some(protected_drop) = rejection.protected_drop.as_ref() {
                span.record(
                    "protected_drop_reason",
                    protected_drop_reason(protected_drop.reason),
                );
            }
        }
        LayerStackServiceError::Init { .. } | LayerStackServiceError::LayerStack { .. } => {}
    }
}

fn publish_reject_reason(reason: sandbox_runtime_layerstack::PublishRejectReason) -> &'static str {
    match reason {
        sandbox_runtime_layerstack::PublishRejectReason::InvalidBaseRevision => {
            "invalid_base_revision"
        }
        sandbox_runtime_layerstack::PublishRejectReason::GitMutationForbidden => {
            "git_mutation_forbidden"
        }
        sandbox_runtime_layerstack::PublishRejectReason::ProtectedPath => "protected_path",
        sandbox_runtime_layerstack::PublishRejectReason::SourceConflict => "source_conflict",
        sandbox_runtime_layerstack::PublishRejectReason::OpaqueDirProtectedDescendant => {
            "opaque_dir_protected_descendant"
        }
        sandbox_runtime_layerstack::PublishRejectReason::OpaqueDirMixedRoutes => {
            "opaque_dir_mixed_routes"
        }
        sandbox_runtime_layerstack::PublishRejectReason::OpaqueDirExpansionLimit => {
            "opaque_dir_expansion_limit"
        }
        sandbox_runtime_layerstack::PublishRejectReason::RoutePreparationFailed => {
            "route_preparation_failed"
        }
    }
}

fn fingerprint_kind(fingerprint: &sandbox_runtime_layerstack::ContentFingerprint) -> &'static str {
    match fingerprint {
        sandbox_runtime_layerstack::ContentFingerprint::Absent => "absent",
        sandbox_runtime_layerstack::ContentFingerprint::File { .. } => "file",
        sandbox_runtime_layerstack::ContentFingerprint::Symlink { .. } => "symlink",
        sandbox_runtime_layerstack::ContentFingerprint::Directory => "directory",
    }
}

fn protected_drop_reason(
    reason: sandbox_runtime_layerstack::LayerProtectedDropReason,
) -> &'static str {
    match reason {
        sandbox_runtime_layerstack::LayerProtectedDropReason::UnsupportedSpecialFile => {
            "unsupported_special_file"
        }
        sandbox_runtime_layerstack::LayerProtectedDropReason::InvalidLayerPath => {
            "invalid_layer_path"
        }
        sandbox_runtime_layerstack::LayerProtectedDropReason::CommandScratchPath => {
            "command_scratch_path"
        }
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
