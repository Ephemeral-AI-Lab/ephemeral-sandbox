mod git_metadata;
mod ignore;
pub(crate) mod model;
mod opaque_dir;
mod protected_paths;
mod snapshot;

use std::path::Path;

use crate::model::{LayerChange, Manifest};
use crate::MergedView;

use super::error::CommitError;

use git_metadata::command_git_metadata_decision;
use model::{publish_decision, OPAQUE_DIR_EXPANSION_LIMIT};
use protected_paths::is_git_metadata_path;
use snapshot::snapshot_base_hash;

pub(crate) use ignore::{IgnoreSource, ManifestIgnoreSource};
pub(crate) use model::{PublishDecision, Route, ValidationBase};
pub(crate) use opaque_dir::publish_decision_for_opaque_dir;
pub(crate) use protected_paths::route_decision_for_path_from_source;
pub(crate) use snapshot::hash_current;

pub(crate) fn publish_decisions_for_manifest(
    root: &Path,
    manifest: &Manifest,
    changes: &[LayerChange],
) -> Result<Vec<PublishDecision>, CommitError> {
    let view = MergedView::new(root.to_path_buf());
    let source = ManifestIgnoreSource {
        view: &view,
        manifest,
    };
    changes
        .iter()
        .map(|change| {
            if let LayerChange::OpaqueDir { path } = change {
                publish_decision_for_opaque_dir(
                    root,
                    &source,
                    &view,
                    manifest,
                    path,
                    OPAQUE_DIR_EXPANSION_LIMIT,
                )
            } else {
                publish_decision_for_change(&source, &view, manifest, change)
            }
        })
        .collect::<std::result::Result<Vec<_>, CommitError>>()
}

fn publish_decision_for_change(
    source: &impl IgnoreSource,
    view: &MergedView,
    manifest: &Manifest,
    change: &LayerChange,
) -> Result<PublishDecision, CommitError> {
    let path = change.path().clone();
    if is_git_metadata_path(&path) {
        return command_git_metadata_decision(view, manifest, change);
    }

    let (route, drop_reason) = route_decision_for_path_from_source(source, &path)
        .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
    let base_hash = if route == Route::Gated {
        snapshot_base_hash(view, manifest, change)?
    } else {
        None
    };
    Ok(publish_decision(path, route, base_hash, drop_reason))
}
