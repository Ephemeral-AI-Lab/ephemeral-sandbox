use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::fs::resolve_layer_path;
use crate::model::{LayerChange, LayerPath, Manifest};
use crate::{LayerStack, MergedView};

use super::super::model::{ChangesetResult, CommitStatus, FileResult};
use super::super::route::{hash_current, PublishDecision, Route};
use super::queue::{PreparedChangeset, PublishConflict};

#[derive(Clone)]
pub(crate) struct CommitTransaction {
    pub(crate) root: PathBuf,
}

impl CommitTransaction {
    pub(crate) fn revalidate_and_publish(
        &self,
        combined: &PreparedChangeset,
    ) -> std::result::Result<ChangesetResult, PublishConflict> {
        let total_start = Instant::now();
        let mut stack = match LayerStack::open(self.root.clone()) {
            Ok(stack) => stack,
            Err(err) => return Ok(failed_revalidate_result(combined, &err, total_start)),
        };
        let validate_start = Instant::now();
        let validations = match stack.with_active_manifest(|active| {
            let view = MergedView::new(self.root.clone());
            Ok(validate_prepared(&self.root, &view, active, combined))
        }) {
            Ok(validations) => validations,
            Err(err) => return Ok(failed_revalidate_result(combined, &err, total_start)),
        };
        let validate_s = validate_start.elapsed().as_secs_f64();
        if combined.atomic && validations.iter().any(|f| !f.status.is_non_conflicting()) {
            return Ok(atomic_validation_drop_result(
                combined,
                validations,
                validate_s,
                total_start,
            ));
        }
        let publishable_changes = publishable_changes(combined, &validations);
        if publishable_changes.is_empty() {
            return Ok(no_publish_result(
                combined,
                validations,
                validate_s,
                total_start,
            ));
        }
        let publish_start = Instant::now();
        match stack.publish_layer(&publishable_changes) {
            Ok(manifest) => {
                let publish_s = publish_start.elapsed().as_secs_f64();
                Ok(committed_changeset_result(
                    combined,
                    validations,
                    manifest_version_u64_optional(manifest.version),
                    validate_s,
                    publish_s,
                    total_start,
                ))
            }
            Err(crate::LayerStackError::ManifestConflict { found, .. }) => Err(PublishConflict {
                observed_version: manifest_version_u64_optional(found),
            }),
            Err(err) => {
                let publish_s = publish_start.elapsed().as_secs_f64();
                let timings = commit_timings(
                    combined,
                    validate_s,
                    publish_s,
                    total_start.elapsed().as_secs_f64(),
                );
                Ok(failed_changeset_with_timings(
                    combined,
                    &err.to_string(),
                    timings,
                ))
            }
        }
    }
}

fn failed_revalidate_result(
    combined: &PreparedChangeset,
    err: &crate::LayerStackError,
    total_start: Instant,
) -> ChangesetResult {
    let timings = commit_timings(combined, 0.0, 0.0, total_start.elapsed().as_secs_f64());
    failed_changeset_with_timings(combined, &err.to_string(), timings)
}

fn atomic_validation_drop_result(
    combined: &PreparedChangeset,
    validations: Vec<FileResult>,
    validate_s: f64,
    total_start: Instant,
) -> ChangesetResult {
    ChangesetResult {
        files: validations
            .into_iter()
            .map(|file| {
                if file.status.is_published() {
                    FileResult {
                        status: CommitStatus::Dropped,
                        message: "not published because atomic changeset validation failed"
                            .to_owned(),
                        ..file
                    }
                } else {
                    file
                }
            })
            .collect(),
        published_manifest_version: None,
        timings: commit_timings(
            combined,
            validate_s,
            0.0,
            total_start.elapsed().as_secs_f64(),
        ),
    }
}

fn publishable_changes(
    combined: &PreparedChangeset,
    validations: &[FileResult],
) -> Vec<LayerChange> {
    let publishable_paths = validations
        .iter()
        .filter(|file| file.status.is_published())
        .map(|file| file.path.as_str())
        .collect::<HashSet<_>>();
    combined
        .changes
        .iter()
        .filter(|change| publishable_paths.contains(change.path().as_str()))
        .cloned()
        .collect()
}

fn no_publish_result(
    combined: &PreparedChangeset,
    validations: Vec<FileResult>,
    validate_s: f64,
    total_start: Instant,
) -> ChangesetResult {
    ChangesetResult {
        files: validations,
        published_manifest_version: None,
        timings: commit_timings(
            combined,
            validate_s,
            0.0,
            total_start.elapsed().as_secs_f64(),
        ),
    }
}

fn committed_changeset_result(
    combined: &PreparedChangeset,
    validations: Vec<FileResult>,
    published_manifest_version: Option<u64>,
    validate_s: f64,
    publish_s: f64,
    total_start: Instant,
) -> ChangesetResult {
    let timings = commit_timings(
        combined,
        validate_s,
        publish_s,
        total_start.elapsed().as_secs_f64(),
    );
    ChangesetResult {
        files: validations
            .into_iter()
            .map(|file| {
                if file.status.is_published() {
                    FileResult {
                        status: CommitStatus::Committed,
                        ..file
                    }
                } else {
                    file
                }
            })
            .collect(),
        published_manifest_version,
        timings,
    }
}

fn validate_prepared(
    root: &Path,
    view: &MergedView,
    manifest: &Manifest,
    prepared: &PreparedChangeset,
) -> Vec<FileResult> {
    let mut parent_absent_cache = HashMap::new();
    prepared
        .decisions
        .iter()
        .map(|decision| match decision.route() {
            Route::Drop => decision
                .drop_file_result_with_default("change dropped")
                .expect("drop route has drop file result"),
            Route::Direct => accepted_file(decision.path()),
            Route::Gated => {
                validate_gated_group(root, view, manifest, decision, &mut parent_absent_cache)
            }
        })
        .collect()
}

fn accepted_file(path: &LayerPath) -> FileResult {
    FileResult {
        path: path.clone(),
        status: CommitStatus::Accepted,
        message: String::new(),
        observed_version: None,
        observed_state: None,
    }
}

fn validate_gated_group(
    root: &Path,
    view: &MergedView,
    manifest: &Manifest,
    group: &PublishDecision,
    parent_absent_cache: &mut HashMap<String, bool>,
) -> FileResult {
    if let Some(validation_base_hashes) = group.validation_base_hashes() {
        for (path, base_hash) in validation_base_hashes {
            let result = validate_gated_path(
                root,
                view,
                manifest,
                path,
                base_hash.as_deref(),
                parent_absent_cache,
            );
            if !result.status.is_non_conflicting() {
                return FileResult {
                    path: group.path().clone(),
                    status: result.status,
                    message: format!(
                        "opaque directory descendant {}: {}",
                        path.as_str(),
                        result.conflict_message(result.status.status_str())
                    ),
                    observed_version: result.observed_version,
                    observed_state: result.observed_state,
                };
            }
        }
        return accepted_file(group.path());
    }

    validate_gated_path(
        root,
        view,
        manifest,
        group.path(),
        group.base_hash(),
        parent_absent_cache,
    )
}

fn validate_gated_path(
    root: &Path,
    view: &MergedView,
    manifest: &Manifest,
    path: &LayerPath,
    base_hash: Option<&str>,
    parent_absent_cache: &mut HashMap<String, bool>,
) -> FileResult {
    let path_str = path.as_str();
    if base_hash.is_none() {
        if let Some(parent) = parent_dir(path_str) {
            let parent_absent = *parent_absent_cache
                .entry(parent.to_owned())
                .or_insert_with(|| parent_absent_from_manifest(root, manifest, parent));
            if parent_absent {
                return accepted_file(path);
            }
        }
    }
    match view.read_bytes(path_str, manifest) {
        Ok((bytes, exists)) if hash_current(bytes.as_deref(), exists).as_deref() == base_hash => {
            accepted_file(path)
        }
        Ok(_) => FileResult {
            path: path.clone(),
            status: CommitStatus::AbortedVersion,
            message: "content changed".to_owned(),
            observed_version: None,
            observed_state: Some("content_changed".to_owned()),
        },
        Err(err) => FileResult {
            path: path.clone(),
            status: CommitStatus::Failed,
            message: err.to_string(),
            observed_version: None,
            observed_state: Some("read_failed".to_owned()),
        },
    }
}

fn parent_dir(path: &str) -> Option<&str> {
    path.rsplit_once('/')
        .map(|(parent, _)| parent)
        .filter(|parent| !parent.is_empty())
}

fn parent_absent_from_manifest(root: &Path, manifest: &Manifest, parent: &str) -> bool {
    manifest.layers.iter().all(|layer| {
        let layer_dir = resolve_layer_path(root, &layer.path);
        matches!(
            std::fs::symlink_metadata(layer_dir.join(parent)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound
        )
    })
}

fn failed_changeset_with_timings(
    prepared: &PreparedChangeset,
    message: &str,
    timings: BTreeMap<String, f64>,
) -> ChangesetResult {
    ChangesetResult {
        files: prepared
            .decisions
            .iter()
            .map(|group| FileResult {
                path: group.path().clone(),
                status: CommitStatus::Failed,
                message: message.to_owned(),
                observed_version: None,
                observed_state: Some("storage_error".to_owned()),
            })
            .collect(),
        published_manifest_version: None,
        timings,
    }
}

pub(crate) fn commit_timings(
    prepared: &PreparedChangeset,
    validate_s: f64,
    publish_s: f64,
    total_s: f64,
) -> BTreeMap<String, f64> {
    let mut timings = BTreeMap::new();
    timings.insert("occ.commit.total_s".to_owned(), total_s);
    timings.insert("occ.commit.validate_groups_s".to_owned(), validate_s);
    timings.insert("occ.commit.publish_layer_s".to_owned(), publish_s);
    timings.insert(
        "occ.commit.gated_path_count".to_owned(),
        usize_to_f64_saturating(
            prepared
                .decisions
                .iter()
                .filter(|group| group.route() == Route::Gated)
                .count(),
        ),
    );
    timings.insert(
        "occ.commit.direct_path_count".to_owned(),
        usize_to_f64_saturating(
            prepared
                .decisions
                .iter()
                .filter(|group| group.route() == Route::Direct)
                .count(),
        ),
    );
    timings
}

fn manifest_version_u64_optional(version: i64) -> Option<u64> {
    u64::try_from(version).ok()
}

fn usize_to_f64_saturating(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::from(u32::MAX), f64::from)
}
