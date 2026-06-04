//! OCC service: prepare typed changesets and commit through the single writer.
//!
//! The service routes changes into [`PublishDecision`]s, submits the prepared
//! changeset to the per-root [`CommitQueue`].

use std::sync::Arc;
use std::time::Instant;

use eos_protocol::{LayerChange, LayerPath};

use crate::commit_queue::{CommitQueue, CommitTransactionPort, PreparedChangeset};
use crate::error::OccError;
use crate::route::{ChangesetResult, PublishDecision, Route};

/// Route/base-hash provider used while preparing OCC changesets.
///
/// The daemon owns the concrete layer-stack/gitignore implementation because
/// this crate must not know daemon workspace bindings. The default provider
/// routes every non-`.git` path as gated with an unknown base hash, giving unit
/// tests and custom queues a conservative default.
pub trait OccRouteProvider: Send + Sync {
    /// Is this normalized path gitignored in the operation snapshot?
    ///
    /// # Errors
    ///
    /// Returns [`OccError`] when ignore-state lookup fails.
    fn is_ignored(&self, path: &LayerPath) -> Result<bool, OccError>;

    /// Content hash of `path` in the operation snapshot, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns [`OccError`] when snapshot content lookup fails.
    fn base_hash(&self, path: &LayerPath) -> Result<Option<String>, OccError>;
}

#[derive(Debug)]
struct AllGatedRouteProvider;

impl OccRouteProvider for AllGatedRouteProvider {
    fn is_ignored(&self, _path: &LayerPath) -> Result<bool, OccError> {
        Ok(false)
    }

    fn base_hash(&self, _path: &LayerPath) -> Result<Option<String>, OccError> {
        Ok(None)
    }
}

/// Prepare typed OCC changesets and commit them through the single writer.
///
/// Holds the per-root [`CommitQueue`]. There is exactly one `OccService` per
/// `layer_stack_root` (the MF-1 owner).
pub struct OccService<T: CommitTransactionPort + 'static> {
    commit_queue: CommitQueue<T>,
    route_provider: Arc<dyn OccRouteProvider>,
}

impl<T: CommitTransactionPort + 'static> OccService<T> {
    /// Build a service and start its owned commit queue.
    ///
    /// # Errors
    ///
    /// Returns [`OccError`] when the owned commit queue cannot be started.
    pub fn new(commit_queue: CommitQueue<T>) -> Result<Self, OccError> {
        Self::with_route_provider(commit_queue, Arc::new(AllGatedRouteProvider))
    }

    /// Build a service with a daemon-provided route/base-hash provider.
    ///
    /// # Errors
    ///
    /// Returns [`OccError`] when the owned commit queue cannot be started.
    pub fn with_route_provider(
        mut commit_queue: CommitQueue<T>,
        route_provider: Arc<dyn OccRouteProvider>,
    ) -> Result<Self, OccError> {
        commit_queue.start()?;
        Ok(Self {
            commit_queue,
            route_provider,
        })
    }

    /// Prepare and commit a changeset through the layer stack.
    ///
    /// # Errors
    ///
    /// Returns [`OccError`] when preparation, queue submission, or the commit
    /// worker reply fails.
    pub fn apply_changeset(
        &self,
        changes: &[LayerChange],
        snapshot_version: Option<u64>,
        atomic: bool,
    ) -> Result<ChangesetResult, OccError> {
        let prepared = self.prepare_changeset(changes, snapshot_version, atomic)?;
        self.apply_prepared_changeset(prepared)
    }

    /// Prepare and commit with caller-supplied base hashes.
    ///
    /// Direct file APIs use this to pin the hash observed before applying edit
    /// anchors. Overlay callers can pass hashes from their leased snapshot once
    /// the shell/search pipeline is wired.
    ///
    /// # Errors
    ///
    /// Returns [`OccError`] when preparation, queue submission, or the commit
    /// worker reply fails.
    pub fn apply_changeset_with_base_hashes(
        &self,
        changes: &[LayerChange],
        snapshot_version: Option<u64>,
        atomic: bool,
        base_hashes: &[(LayerPath, Option<String>)],
    ) -> Result<ChangesetResult, OccError> {
        let prepared = self.prepare_changeset_with_base_hashes(
            changes,
            snapshot_version,
            atomic,
            base_hashes,
        )?;
        self.apply_prepared_changeset(prepared)
    }

    /// Route raw changes into a [`PreparedChangeset`] (Drop/Direct/Gated/Reject).
    ///
    /// # Errors
    ///
    /// Returns [`OccError`] when route or base-hash lookup fails.
    pub fn prepare_changeset(
        &self,
        changes: &[LayerChange],
        snapshot_version: Option<u64>,
        atomic: bool,
    ) -> Result<PreparedChangeset, OccError> {
        self.prepare_changeset_with_base_hashes(changes, snapshot_version, atomic, &[])
    }

    /// Route raw changes into a [`PreparedChangeset`] with optional base-hash
    /// overrides supplied by the caller.
    ///
    /// # Errors
    ///
    /// Returns [`OccError`] when route or base-hash lookup fails.
    pub fn prepare_changeset_with_base_hashes(
        &self,
        changes: &[LayerChange],
        snapshot_version: Option<u64>,
        atomic: bool,
        base_hashes: &[(LayerPath, Option<String>)],
    ) -> Result<PreparedChangeset, OccError> {
        let mut path_groups = Vec::with_capacity(changes.len());
        let mut publishable = Vec::with_capacity(changes.len());
        for change in changes {
            let path = change.path().clone();
            if path.as_str() == ".git" || path.as_str().starts_with(".git/") {
                path_groups.push(PublishDecision {
                    path,
                    route: Route::Drop,
                    base_hash: None,
                    message: Some(".git paths are not mutable through OCC".to_owned()),
                });
                continue;
            }
            let route = if self.route_provider.is_ignored(&path)? {
                Route::Direct
            } else {
                Route::Gated
            };
            let base_hash = if route == Route::Gated {
                match base_hashes.iter().find(|(candidate, _)| candidate == &path) {
                    Some((_, hash)) => hash.clone(),
                    None => self.route_provider.base_hash(&path)?,
                }
            } else {
                None
            };
            path_groups.push(PublishDecision {
                path,
                route,
                base_hash,
                message: None,
            });
            publishable.push(change.clone());
        }
        Ok(PreparedChangeset {
            snapshot_version,
            path_groups,
            changes: publishable,
            atomic,
        })
    }

    fn apply_prepared_changeset(
        &self,
        prepared: PreparedChangeset,
    ) -> Result<ChangesetResult, OccError> {
        let total_start = Instant::now();
        let snapshot_version = prepared.snapshot_version;
        let receiver = self.commit_queue.submit(prepared)?;
        let commit_start = Instant::now();
        let result = receiver.recv().map_err(|_| OccError::ReplyDisconnected)??;
        Ok(finalize_apply_result(
            result,
            snapshot_version,
            commit_start.elapsed().as_secs_f64(),
            total_start.elapsed().as_secs_f64(),
        ))
    }
}

fn finalize_apply_result(
    mut result: ChangesetResult,
    snapshot_version: Option<u64>,
    commit_elapsed_s: f64,
    total_s: f64,
) -> ChangesetResult {
    let commit_queue_wait_s = timing_or_default(&result.timings, "occ.serial.queue_wait_s");
    let commit_worker_s = timing_or_default(&result.timings, "occ.commit.total_s")
        .max(timing_or_default(&result.timings, "occ.serial.commit_s"));
    result.timings.insert(
        "occ.apply.commit_queue_wait_s".to_owned(),
        commit_queue_wait_s,
    );
    result
        .timings
        .insert("occ.apply.commit_resume_wait_s".to_owned(), 0.0);
    result
        .timings
        .insert("occ.apply.commit_worker_s".to_owned(), commit_worker_s);
    result
        .timings
        .insert("occ.apply.commit_s".to_owned(), commit_elapsed_s);
    result
        .timings
        .insert("occ.apply.total_s".to_owned(), total_s);
    if let (Some(published), Some(snapshot)) = (result.published_manifest_version, snapshot_version)
    {
        result.timings.insert(
            "occ.apply.manifest_lag".to_owned(),
            published.saturating_sub(snapshot + 1) as f64,
        );
    }
    result
}

fn timing_or_default(timings: &std::collections::BTreeMap<String, f64>, key: &str) -> f64 {
    timings.get(key).copied().unwrap_or(0.0)
}

impl<T: CommitTransactionPort + 'static> Drop for OccService<T> {
    fn drop(&mut self) {
        let _ = self.commit_queue.close();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use eos_protocol::{LayerChange, LayerPath};

    use super::*;
    use crate::{CommitQueue, FileResult, OccStatus};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    struct RecordingTransaction;

    impl CommitTransactionPort for RecordingTransaction {
        fn revalidate_and_publish(
            &self,
            combined: &PreparedChangeset,
        ) -> Result<ChangesetResult, crate::PublishConflict> {
            let mut timings = BTreeMap::new();
            timings.insert("occ.commit.total_s".to_owned(), 0.123);
            Ok(ChangesetResult {
                files: combined
                    .path_groups
                    .iter()
                    .map(|group| FileResult {
                        path: group.path.clone(),
                        status: OccStatus::Committed,
                        message: String::new(),
                    })
                    .collect(),
                published_manifest_version: Some(3),
                timings,
            })
        }
    }

    #[test]
    fn apply_changeset_adds_public_apply_timing_envelope() -> TestResult {
        let queue = CommitQueue::with_config(RecordingTransaction, 64, 0.0, 3);
        let service = OccService::new(queue)?;
        let path = LayerPath::parse("timed.txt")?;
        let result = service.apply_changeset(
            &[LayerChange::Write {
                path,
                content: b"x".to_vec(),
            }],
            Some(1),
            true,
        )?;

        assert!(result.success());
        assert!(result.timings.contains_key("occ.apply.commit_queue_wait_s"));
        assert_eq!(
            result
                .timings
                .get("occ.apply.commit_resume_wait_s")
                .copied(),
            Some(0.0)
        );
        assert!(
            result
                .timings
                .get("occ.apply.commit_worker_s")
                .copied()
                .unwrap_or_default()
                >= 0.123
        );
        assert!(result.timings.contains_key("occ.apply.commit_s"));
        assert!(result.timings.contains_key("occ.apply.total_s"));
        assert_eq!(
            result.timings.get("occ.apply.manifest_lag").copied(),
            Some(1.0)
        );
        Ok(())
    }
}
