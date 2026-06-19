use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use crate::model::LayerChange;

use super::error::CommitError;
use super::model::{ChangesetResult, CommitOptions, OccTraceEvent};
use super::route::{PublishDecision, Route};
use super::worker::{CommitQueue, CommitTransaction, PreparedChangeset};

pub(crate) struct CommitWriter {
    commit_queue: CommitQueue,
}

impl CommitWriter {
    pub(crate) fn with_options(root: PathBuf, options: CommitOptions) -> Result<Self, CommitError> {
        let options = CommitOptions::new(options.auto_squash_max_depth);
        let transaction = CommitTransaction {
            root: root.clone(),
            options,
        };
        let mut commit_queue = CommitQueue::new(transaction);
        commit_queue.start()?;
        Ok(Self { commit_queue })
    }

    pub(crate) fn apply_changeset_with_decisions(
        &self,
        changes: &[LayerChange],
        snapshot_version: Option<u64>,
        atomic: bool,
        decisions: Vec<PublishDecision>,
    ) -> Result<ChangesetResult, CommitError> {
        let prepared = PreparedChangeset::try_new(changes, decisions, atomic)?;
        let handoff_event =
            worker_handoff_event(&prepared.decisions, prepared.changes.len(), prepared.atomic);
        let receiver = self.commit_queue.submit(prepared)?;
        let mut result = receiver
            .recv()
            .map_err(|_| CommitError::ReplyDisconnected)??;
        result.events.insert(0, handoff_event);
        if let (Some(published), Some(snapshot)) =
            (result.published_manifest_version, snapshot_version)
        {
            result.timings.insert(
                "occ.apply.manifest_lag".to_owned(),
                published.saturating_sub(snapshot + 1) as f64,
            );
        }
        Ok(result)
    }

    pub(crate) fn apply_layerstack_changeset(
        &self,
        changes: &[LayerChange],
        snapshot_version: Option<u64>,
        decisions: Vec<PublishDecision>,
    ) -> Result<ChangesetResult, CommitError> {
        self.apply_changeset_with_decisions(changes, snapshot_version, true, decisions)
    }
}

impl Drop for CommitWriter {
    fn drop(&mut self) {
        let _ = self.commit_queue.close();
    }
}

fn worker_handoff_event(
    decisions: &[PublishDecision],
    publishable_change_count: usize,
    atomic: bool,
) -> OccTraceEvent {
    let mut gated_path_count = 0;
    let mut direct_path_count = 0;
    let mut drop_path_count = 0;
    let mut drop_reason_counts: BTreeMap<String, usize> = BTreeMap::new();
    for decision in decisions {
        match decision.route() {
            Route::Gated => gated_path_count += 1,
            Route::Direct => direct_path_count += 1,
            Route::Drop => {
                drop_path_count += 1;
                if let Some(reason) = decision.drop_reason() {
                    *drop_reason_counts
                        .entry(reason.as_str().to_owned())
                        .or_default() += 1;
                }
            }
        }
    }
    OccTraceEvent::new(
        "occ",
        "worker_handoff",
        json!({
            "path_count": decisions.len(),
            "publishable_change_count": publishable_change_count,
            "atomic": atomic,
            "gated_path_count": gated_path_count,
            "direct_path_count": direct_path_count,
            "drop_path_count": drop_path_count,
            "drop_reason_counts": drop_reason_counts,
        }),
    )
}
