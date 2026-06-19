use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use serde_json::json;

use super::super::model::{ChangesetResult, CommitStatus, FileResult, OccTraceEvent};
use super::super::route::{PublishDecision, Route};
use super::super::CommitError;
use super::transaction::{commit_timings, CommitTransaction};
use crate::model::LayerChange;

pub(crate) const COMMIT_QUEUE_THREAD_NAME: &str = "occ-commit-queue";

pub(crate) const MAX_BATCH_SIZE: usize = 64;

pub(crate) const BATCH_WINDOW_S: f64 = 0.002;

pub(crate) const MAX_OCC_CAS_RETRIES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedChangeset {
    pub(crate) decisions: Vec<PublishDecision>,
    pub(crate) changes: Vec<LayerChange>,
    pub(crate) atomic: bool,
}

impl PreparedChangeset {
    pub(crate) fn try_new(
        changes: &[LayerChange],
        decisions: Vec<PublishDecision>,
        atomic: bool,
    ) -> Result<Self, CommitError> {
        if changes.len() > decisions.len() {
            return Err(CommitError::RoutePreparation(format!(
                "changeset has more payload changes than route decisions: {} changes, {} decisions",
                changes.len(),
                decisions.len()
            )));
        }
        for (change, decision) in changes.iter().zip(decisions.iter()) {
            if change.path() != decision.path() {
                return Err(CommitError::RoutePreparation(format!(
                    "changeset decision path mismatch: change {}, decision {}",
                    change.path().as_str(),
                    decision.path().as_str()
                )));
            }
        }
        if let Some(decision) = decisions
            .iter()
            .skip(changes.len())
            .find(|decision| decision.is_publishable())
        {
            return Err(CommitError::RoutePreparation(format!(
                "payload-less route decision must be dropped: {}",
                decision.path().as_str()
            )));
        }
        let changes = changes
            .iter()
            .zip(decisions.iter())
            .filter(|(_, decision)| decision.is_publishable())
            .map(|(change, _)| change.clone())
            .collect();
        Ok(Self {
            decisions,
            changes,
            atomic,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishConflict {
    pub(crate) observed_version: Option<u64>,
}

pub(crate) struct WorkItem {
    pub(crate) prepared: PreparedChangeset,
    pub(crate) reply: mpsc::Sender<Result<ChangesetResult, CommitError>>,
}

enum QueueItem {
    Work(WorkItem),
    Stop,
}

pub(crate) struct CommitQueue {
    sender: mpsc::Sender<QueueItem>,
    receiver: Mutex<Option<mpsc::Receiver<QueueItem>>>,
    transaction: Mutex<Option<CommitTransaction>>,
    handle: Option<std::thread::JoinHandle<()>>,
    closed: bool,
}

struct CommitWorker {
    receiver: mpsc::Receiver<QueueItem>,
    transaction: CommitTransaction,
}

impl CommitQueue {
    pub(crate) fn new(transaction: CommitTransaction) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            transaction: Mutex::new(Some(transaction)),
            handle: None,
            closed: false,
        }
    }

    pub(crate) fn start(&mut self) -> Result<(), CommitError> {
        if self.closed {
            return Err(CommitError::QueueClosed);
        }
        if self
            .handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Ok(());
        }
        let receiver = self
            .receiver
            .lock()
            .map_err(|_| CommitError::QueueStatePoisoned("receiver slot"))?
            .take()
            .ok_or(CommitError::QueueNotStarted)?;
        let transaction = self
            .transaction
            .lock()
            .map_err(|_| CommitError::QueueStatePoisoned("transaction slot"))?
            .take()
            .ok_or(CommitError::QueueNotStarted)?;
        let worker = CommitWorker {
            receiver,
            transaction,
        };
        let handle = std::thread::Builder::new()
            .name(COMMIT_QUEUE_THREAD_NAME.to_owned())
            .spawn(move || {
                worker.run();
            })
            .map_err(|err| CommitError::WorkerStart(err.to_string()))?;
        self.handle = Some(handle);
        Ok(())
    }

    pub(crate) fn close(&mut self) -> Result<(), CommitError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if self.handle.is_none() {
            return Ok(());
        }
        let _ = self.sender.send(QueueItem::Stop);
        self.handle.take().map_or(Ok(()), |handle| {
            handle.join().map_err(|_| CommitError::WorkerPanicked)
        })
    }

    pub(crate) fn submit(
        &self,
        prepared: PreparedChangeset,
    ) -> Result<mpsc::Receiver<Result<ChangesetResult, CommitError>>, CommitError> {
        if self.closed {
            return Err(CommitError::QueueClosed);
        }
        if self
            .handle
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
        {
            return Err(CommitError::QueueNotStarted);
        }
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(QueueItem::Work(WorkItem { prepared, reply }))
            .map_err(|_| CommitError::QueueClosed)?;
        Ok(receiver)
    }
}

impl CommitWorker {
    fn run(self) {
        while let Ok(first) = self.receiver.recv() {
            let QueueItem::Work(first) = first else {
                return;
            };
            let mut items = vec![first];
            let mut stop_seen = drain_ready(&self.receiver, &mut items, MAX_BATCH_SIZE);
            if !stop_seen && items.len() < MAX_BATCH_SIZE {
                std::thread::sleep(Duration::from_secs_f64(BATCH_WINDOW_S));
                stop_seen = drain_ready(&self.receiver, &mut items, MAX_BATCH_SIZE);
            }
            for batch in disjoint_batches(items) {
                commit_batch(&self.transaction, batch);
            }
            if stop_seen {
                return;
            }
        }
    }
}

fn commit_batch(transaction: &CommitTransaction, batch: Vec<WorkItem>) {
    let Some(combined) = combine_prepared(batch.iter().map(|item| &item.prepared)) else {
        return;
    };
    let mut attempts = 0;
    let mut result = loop {
        match transaction.revalidate_and_publish(&combined) {
            Ok(result) => break result,
            Err(conflict) => {
                attempts += 1;
                if attempts >= MAX_OCC_CAS_RETRIES {
                    break cas_exhaustion_result(&combined, &conflict, MAX_OCC_CAS_RETRIES);
                }
            }
        }
    };
    result.events.insert(
        0,
        OccTraceEvent::new(
            "occ",
            "worker_batch_finished",
            json!({
                "batch_item_count": batch.len(),
                "combined_path_count": combined.decisions.len(),
                "combined_change_count": combined.changes.len(),
                "atomic": combined.atomic,
                "cas_retry_count": attempts,
            }),
        ),
    );
    let files_by_path = result
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<HashMap<_, _>>();
    for item in batch {
        let files = result_files_for_item(&files_by_path, &item.prepared);
        let _ = item.reply.send(Ok(ChangesetResult {
            files,
            published_manifest_version: result.published_manifest_version,
            timings: result.timings.clone(),
            events: result.events.clone(),
        }));
    }
}

fn drain_ready(
    receiver: &mpsc::Receiver<QueueItem>,
    items: &mut Vec<WorkItem>,
    max_batch_size: usize,
) -> bool {
    while items.len() < max_batch_size {
        match receiver.try_recv() {
            Ok(QueueItem::Work(item)) => items.push(item),
            Ok(QueueItem::Stop) | Err(mpsc::TryRecvError::Disconnected) => return true,
            Err(mpsc::TryRecvError::Empty) => return false,
        }
    }
    false
}

pub(crate) fn disjoint_batches(items: Vec<WorkItem>) -> Vec<Vec<WorkItem>> {
    let mut pending: Vec<(WorkItem, HashSet<String>)> = items
        .into_iter()
        .map(|item| {
            let paths = item
                .prepared
                .decisions
                .iter()
                .map(|decision| decision.path().as_str().to_owned())
                .collect();
            (item, paths)
        })
        .collect();
    let mut batches = Vec::new();
    while !pending.is_empty() {
        let mut used = HashSet::new();
        let mut batch = Vec::new();
        let mut rest = Vec::new();
        for (item, paths) in pending {
            if item.prepared.atomic || !used.is_disjoint(&paths) {
                rest.push((item, paths));
            } else {
                used.extend(paths.iter().cloned());
                batch.push(item);
            }
        }
        if batch.is_empty() {
            let (item, _) = rest.remove(0);
            batch.push(item);
        }
        batches.push(batch);
        pending = rest;
    }
    batches
}

fn combine_prepared<'a>(
    items: impl Iterator<Item = &'a PreparedChangeset>,
) -> Option<PreparedChangeset> {
    let items: Vec<&PreparedChangeset> = items.collect();
    let first = items.first()?;
    debug_assert!(items.len() == 1 || !items.iter().any(|prepared| prepared.atomic));
    Some(PreparedChangeset {
        decisions: items
            .iter()
            .flat_map(|prepared| prepared.decisions.iter().cloned())
            .collect(),
        changes: items
            .iter()
            .flat_map(|prepared| prepared.changes.iter().cloned())
            .collect(),
        atomic: first.atomic,
    })
}

fn result_files_for_item<'a>(
    files_by_path: &HashMap<&'a str, &'a FileResult>,
    prepared: &PreparedChangeset,
) -> Vec<FileResult> {
    prepared
        .decisions
        .iter()
        .filter_map(|decision| {
            files_by_path
                .get(decision.path().as_str())
                .map(|file| (*file).clone())
        })
        .collect()
}

pub(crate) fn cas_exhaustion_result(
    prepared: &PreparedChangeset,
    conflict: &PublishConflict,
    max_cas_retries: u32,
) -> ChangesetResult {
    let message = format!(
        "CAS mismatch retry budget exhausted after {max_cas_retries} attempts: observed version {:?}",
        conflict.observed_version
    );
    let files = prepared
        .decisions
        .iter()
        .map(|decision| match decision.route() {
            Route::Drop => decision
                .drop_file_result_with_default("")
                .expect("drop route has drop file result"),
            Route::Direct | Route::Gated => FileResult {
                path: decision.path().clone(),
                status: CommitStatus::AbortedVersion,
                message: message.clone(),
                observed_version: conflict.observed_version,
                observed_state: Some("manifest_conflict".to_owned()),
            },
        })
        .collect();
    ChangesetResult {
        files,
        published_manifest_version: None,
        timings: commit_timings(prepared, 0.0, 0.0, 0.0),
        events: Vec::new(),
    }
}
