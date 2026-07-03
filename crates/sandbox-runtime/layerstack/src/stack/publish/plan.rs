use crate::error::LayerStackError;
use crate::model::{manifest_root_hash, LayerChange, LayerPath};
use crate::stack::projection::MergedEntry;
use crate::stack::MergedView;

use super::fingerprint::content_fingerprint;
use super::gitignore::GitignoreOracle;
use super::model::{
    ContentFingerprint, PublishReject, PublishRejectReason, PublishRouteSummary,
    PublishValidatedChangesRequest,
};
use super::opaque_dir::{hidden_descendants, OPAQUE_DIR_EXPANSION_LIMIT};
use super::route::{forbidden_path, RouteKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceValidation {
    pub(crate) path: LayerPath,
    pub(crate) expected: ContentFingerprint,
}

/// One accepted change paired with the route it took, so the resolver knows
/// which writes are source-validated (and thus merge/line-origin eligible)
/// versus wholesale-owned ignored writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcceptedChange {
    pub(crate) change: LayerChange,
    pub(crate) route: RouteKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishPlan {
    accepted: Vec<AcceptedChange>,
    source_validations: Vec<SourceValidation>,
    route_summary: PublishRouteSummary,
}

impl PublishPlan {
    pub(crate) fn accepted(&self) -> &[AcceptedChange] {
        &self.accepted
    }

    pub(crate) fn source_validations(&self) -> &[SourceValidation] {
        &self.source_validations
    }

    pub(crate) fn route_summary(&self) -> PublishRouteSummary {
        self.route_summary.clone()
    }
}

pub(crate) fn plan_publish(
    view: &MergedView,
    request: &PublishValidatedChangesRequest,
) -> Result<PublishPlan, LayerStackError> {
    validate_base_revision(request)?;
    if let Some(drop) = request
        .protected_drops
        .iter()
        .find(|drop| drop.reason != super::model::LayerProtectedDropReason::UnsupportedSpecialFile)
    {
        return Err(LayerStackError::PublishRejected(Box::new(
            PublishReject::protected_drop(drop.clone()),
        )));
    }

    let oracle = GitignoreOracle::new(view, &request.base.manifest);
    let mut accepted = Vec::with_capacity(request.changes.len());
    let mut source_validations = Vec::new();
    let mut route_summary = PublishRouteSummary::default();

    for change in &request.changes {
        let route = match change {
            LayerChange::OpaqueDir { path } => {
                plan_opaque_dir(view, &oracle, request, path, &mut source_validations)?
            }
            _ => {
                let route = route_change(view, &oracle, request, change)?;
                if route == RouteKind::Source {
                    source_validations.push(SourceValidation {
                        path: change.path().clone(),
                        expected: content_fingerprint(view, &request.base.manifest, change.path())?,
                    });
                }
                route
            }
        };
        match route {
            RouteKind::Source => route_summary.source_count += 1,
            RouteKind::Ignored => route_summary.ignored_count += 1,
        }
        accepted.push(AcceptedChange {
            change: change.clone(),
            route,
        });
    }

    Ok(PublishPlan {
        accepted,
        source_validations,
        route_summary,
    })
}

fn validate_base_revision(request: &PublishValidatedChangesRequest) -> Result<(), LayerStackError> {
    let base = &request.base;
    let actual_hash = manifest_root_hash(&base.manifest);
    if base.revision.manifest_version != base.manifest.version
        || base.revision.root_hash != actual_hash
        || base.revision.layer_count != base.manifest.layers.len()
    {
        return Err(LayerStackError::PublishRejected(Box::new(
            PublishReject::with_message(
                PublishRejectReason::InvalidBaseRevision,
                format!(
                    "base revision does not match base manifest: revision=({}, {}, {} layers), manifest=({}, {}, {} layers)",
                    base.revision.manifest_version,
                    base.revision.root_hash,
                    base.revision.layer_count,
                    base.manifest.version,
                    actual_hash,
                    base.manifest.layers.len()
                ),
            ),
        )));
    }
    Ok(())
}

fn route_change(
    view: &MergedView,
    oracle: &GitignoreOracle<'_>,
    request: &PublishValidatedChangesRequest,
    change: &LayerChange,
) -> Result<RouteKind, LayerStackError> {
    let is_dir = match change {
        LayerChange::Delete { path } => {
            matches!(
                view.read_entry(path.as_str(), &request.base.manifest)?,
                MergedEntry::Directory
            )
        }
        LayerChange::OpaqueDir { .. } => true,
        LayerChange::Write { .. } | LayerChange::WriteFile { .. } | LayerChange::Symlink { .. } => {
            false
        }
    };
    route_path(oracle, change.path(), is_dir)
}

fn route_path(
    oracle: &GitignoreOracle<'_>,
    path: &LayerPath,
    is_dir: bool,
) -> Result<RouteKind, LayerStackError> {
    if let Some(reason) = forbidden_path(path) {
        return Err(LayerStackError::PublishRejected(Box::new(
            PublishReject::at_path(path.clone(), reason),
        )));
    }
    Ok(if oracle.is_ignored(path, is_dir)? {
        RouteKind::Ignored
    } else {
        RouteKind::Source
    })
}

fn plan_opaque_dir(
    view: &MergedView,
    oracle: &GitignoreOracle<'_>,
    request: &PublishValidatedChangesRequest,
    path: &LayerPath,
    source_validations: &mut Vec<SourceValidation>,
) -> Result<RouteKind, LayerStackError> {
    if let Some(reason) = forbidden_path(path) {
        return Err(LayerStackError::PublishRejected(Box::new(
            PublishReject::at_path(path.clone(), reason),
        )));
    }

    let descendants = hidden_descendants(view, &request.base.manifest, path).map_err(|err| {
        LayerStackError::PublishRejected(Box::new(PublishReject::with_message(
            PublishRejectReason::RoutePreparationFailed,
            err.to_string(),
        )))
    })?;
    if descendants.len() > OPAQUE_DIR_EXPANSION_LIMIT {
        return Err(LayerStackError::PublishRejected(Box::new(
            PublishReject::at_path(path.clone(), PublishRejectReason::OpaqueDirExpansionLimit),
        )));
    }

    if descendants.is_empty() {
        let route = route_path(oracle, path, true)?;
        if route == RouteKind::Source {
            source_validations.push(SourceValidation {
                path: path.clone(),
                expected: content_fingerprint(view, &request.base.manifest, path)?,
            });
        }
        return Ok(route);
    }

    let mut saw_source = false;
    let mut saw_ignored = false;
    for descendant in descendants {
        if forbidden_path(&descendant).is_some() {
            return Err(LayerStackError::PublishRejected(Box::new(
                PublishReject::at_path(
                    descendant,
                    PublishRejectReason::OpaqueDirProtectedDescendant,
                ),
            )));
        }
        let descendant_is_dir = matches!(
            view.read_entry(descendant.as_str(), &request.base.manifest)?,
            MergedEntry::Directory
        );
        if oracle.is_ignored(&descendant, descendant_is_dir)? {
            saw_ignored = true;
        } else {
            saw_source = true;
            source_validations.push(SourceValidation {
                path: descendant.clone(),
                expected: content_fingerprint(view, &request.base.manifest, &descendant)?,
            });
        }
        if saw_source && saw_ignored {
            return Err(LayerStackError::PublishRejected(Box::new(
                PublishReject::at_path(path.clone(), PublishRejectReason::OpaqueDirMixedRoutes),
            )));
        }
    }

    Ok(if saw_ignored {
        RouteKind::Ignored
    } else {
        RouteKind::Source
    })
}
