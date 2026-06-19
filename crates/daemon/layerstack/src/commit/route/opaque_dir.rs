use std::collections::BTreeSet;
use std::io;
use std::path::Path;

use crate::error::LayerStackError;
use crate::fs::resolve_layer_path;
use crate::model::LayerPath;
use crate::whiteout::{is_kernel_whiteout_meta, LOGICAL_WHITEOUT_PREFIX, OPAQUE_MARKER};
use crate::{Manifest, MergedView};

use super::super::error::CommitError;
use super::ignore::IgnoreSource;
use super::model::{
    publish_decision, rejected_drop_decision, PublishDecision, Route, RouteDropReason,
};
use super::protected_paths::{
    is_git_metadata_path, route_decision_for_path_from_source, route_for_path_from_source,
};
use super::snapshot::snapshot_base_hash_for_path;

pub(crate) fn publish_decision_for_opaque_dir(
    root: &Path,
    source: &impl IgnoreSource,
    view: &MergedView,
    manifest: &Manifest,
    path: &LayerPath,
    expansion_limit: usize,
) -> Result<PublishDecision, CommitError> {
    if is_git_metadata_path(path) {
        return Ok(rejected_drop_decision(
            path.clone(),
            RouteDropReason::GitMetadataOpaqueReplace,
        ));
    }

    let hidden = match visible_paths_hidden_by_opaque_dir(root, manifest, path, expansion_limit)
        .map_err(|err| CommitError::RoutePreparation(err.to_string()))?
    {
        OpaqueDirExpansion::Complete(paths) => paths,
        OpaqueDirExpansion::LimitExceeded => {
            return Ok(rejected_drop_decision(
                path.clone(),
                RouteDropReason::OpaqueDirExpansionLimit,
            ));
        }
    };

    if hidden.is_empty() {
        let (route, drop_reason) = route_decision_for_path_from_source(source, path)
            .map_err(|err| CommitError::RoutePreparation(err.to_string()))?;
        return Ok(if route == Route::Gated {
            PublishDecision::gated_paths(path.clone(), Vec::new())
        } else {
            publish_decision(path.clone(), route, None, drop_reason)
        });
    }

    let mut gated_paths = Vec::new();
    let mut direct_paths = Vec::new();
    for hidden_path in &hidden {
        match route_for_path_from_source(source, hidden_path)
            .map_err(|err| CommitError::RoutePreparation(err.to_string()))?
        {
            Route::Drop => {
                return Ok(rejected_drop_decision(
                    path.clone(),
                    RouteDropReason::OpaqueDirProtectedDescendant,
                ));
            }
            Route::Gated => gated_paths.push(hidden_path.clone()),
            Route::Direct => direct_paths.push(hidden_path.clone()),
        }
    }

    if !gated_paths.is_empty() && !direct_paths.is_empty() {
        return Ok(rejected_drop_decision(
            path.clone(),
            RouteDropReason::OpaqueDirMixedRoutes,
        ));
    }

    if !direct_paths.is_empty() {
        return Ok(publish_decision(path.clone(), Route::Direct, None, None));
    }

    let validation_base_hashes = gated_paths
        .iter()
        .map(|hidden_path| {
            Ok((
                hidden_path.clone(),
                snapshot_base_hash_for_path(view, manifest, hidden_path)?,
            ))
        })
        .collect::<Result<Vec<_>, CommitError>>()?;
    Ok(PublishDecision::gated_paths(
        path.clone(),
        validation_base_hashes,
    ))
}

enum OpaqueDirExpansion {
    Complete(Vec<LayerPath>),
    LimitExceeded,
}

fn visible_paths_hidden_by_opaque_dir(
    root: &Path,
    manifest: &Manifest,
    opaque_path: &LayerPath,
    expansion_limit: usize,
) -> Result<OpaqueDirExpansion, LayerStackError> {
    let mut visible = BTreeSet::new();
    let mut blockers = Vec::<String>::new();
    for layer in &manifest.layers {
        let layer_dir = resolve_layer_path(root, &layer.path);
        if !layer_dir.is_dir() {
            return Err(LayerStackError::Storage(format!(
                "manifest references missing layer {}: {}",
                layer.layer_id, layer.path
            )));
        }
        let mut layer_blockers = Vec::new();
        collect_opaque_hidden_paths_from_layer(
            &layer_dir,
            opaque_path.as_str(),
            &blockers,
            &mut visible,
            &mut layer_blockers,
        )?;
        if visible.len() > expansion_limit {
            return Ok(OpaqueDirExpansion::LimitExceeded);
        }
        blockers.extend(layer_blockers);
    }
    Ok(OpaqueDirExpansion::Complete(visible.into_iter().collect()))
}

fn collect_opaque_hidden_paths_from_layer(
    layer_dir: &Path,
    opaque_path: &str,
    older_blockers: &[String],
    visible: &mut BTreeSet<LayerPath>,
    layer_blockers: &mut Vec<String>,
) -> Result<(), LayerStackError> {
    if path_is_blocked(opaque_path, older_blockers) {
        return Ok(());
    }
    collect_logical_whiteout_for_exact_path(layer_dir, opaque_path, layer_blockers);

    let target = resolve_layer_path(layer_dir, opaque_path);
    let Ok(meta) = std::fs::symlink_metadata(&target) else {
        return Ok(());
    };
    if is_kernel_whiteout_meta(&target, &meta) {
        layer_blockers.push(opaque_path.to_owned());
        return Ok(());
    }
    if meta.file_type().is_symlink() || meta.is_file() {
        visible.insert(LayerPath::parse(opaque_path)?);
        layer_blockers.push(opaque_path.to_owned());
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(());
    }

    let mut stack = vec![target];
    while let Some(dir) = stack.pop() {
        for entry in read_sorted_dir(&dir)? {
            let path = entry.path();
            let rel = layer_relative_string(layer_dir, &path)?;
            if !is_equal_or_descendant(&rel, opaque_path) {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let meta = std::fs::symlink_metadata(&path)?;
            if name == OPAQUE_MARKER {
                if let Some(target) = parent_rel(&rel) {
                    layer_blockers.push(target);
                }
                continue;
            }
            if let Some(target) = logical_whiteout_target(&rel, name) {
                layer_blockers.push(target);
                continue;
            }
            if is_kernel_whiteout_meta(&path, &meta) {
                layer_blockers.push(rel);
                continue;
            }
            if path_is_blocked(&rel, older_blockers) {
                continue;
            }
            if meta.file_type().is_symlink() || meta.is_file() {
                visible.insert(LayerPath::parse(&rel)?);
                layer_blockers.push(rel);
            } else if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    Ok(())
}

fn collect_logical_whiteout_for_exact_path(
    layer_dir: &Path,
    path: &str,
    layer_blockers: &mut Vec<String>,
) {
    let Some((parent, name)) = path.rsplit_once('/') else {
        let whiteout = resolve_layer_path(layer_dir, &format!("{LOGICAL_WHITEOUT_PREFIX}{path}"));
        if whiteout.exists() {
            layer_blockers.push(path.to_owned());
        }
        return;
    };
    let whiteout = resolve_layer_path(
        layer_dir,
        &format!("{parent}/{LOGICAL_WHITEOUT_PREFIX}{name}"),
    );
    if whiteout.exists() {
        layer_blockers.push(path.to_owned());
    }
}

fn read_sorted_dir(dir: &Path) -> Result<Vec<std::fs::DirEntry>, LayerStackError> {
    let mut entries = std::fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);
    Ok(entries)
}

fn layer_relative_string(layer_dir: &Path, path: &Path) -> Result<String, LayerStackError> {
    let rel = path
        .strip_prefix(layer_dir)
        .map_err(|err| LayerStackError::Storage(err.to_string()))?;
    let mut parts = Vec::new();
    for component in rel.components() {
        let part = component.as_os_str().to_str().ok_or_else(|| {
            LayerStackError::Storage(format!(
                "layer path component is not valid UTF-8: {:?}",
                component.as_os_str().as_encoded_bytes()
            ))
        })?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn logical_whiteout_target(rel: &str, name: &str) -> Option<String> {
    if !name.starts_with(LOGICAL_WHITEOUT_PREFIX) || name == OPAQUE_MARKER {
        return None;
    }
    let target_name = name.strip_prefix(LOGICAL_WHITEOUT_PREFIX)?;
    Some(match rel.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{target_name}"),
        None => target_name.to_owned(),
    })
}

fn parent_rel(rel: &str) -> Option<String> {
    rel.rsplit_once('/')
        .map(|(parent, _)| parent.to_owned())
        .filter(|parent| !parent.is_empty())
}

fn path_is_blocked(path: &str, blockers: &[String]) -> bool {
    blockers
        .iter()
        .any(|blocker| is_equal_or_descendant(path, blocker))
}

fn is_equal_or_descendant(path: &str, ancestor: &str) -> bool {
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
