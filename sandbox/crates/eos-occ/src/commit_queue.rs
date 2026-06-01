//! The single-writer publish queue — the MF-1 invariant in code.
//!
//! Exactly one `occ-commit-queue` writer serializes every publish for a given
//! `layer_stack_root`. N disjoint file-API writes are batched into ONE manifest
//! CAS attempt; on a stale base the publisher returns a conflict and the writer
//! retries up to [`MAX_OCC_CAS_RETRIES`] times before surfacing
//! [`OccStatus::AbortedVersion`](crate::OccStatus::AbortedVersion) on every
//! path.
//!
//! ## MF-1: a SINGLE writer, no second instance
//! Any second OCC entry point (notably the PPC self-managed plugin callback)
//! MUST route through THIS same writer + the storage lease keyed by
//! `layer_stack_root`. A second [`CommitQueue`] for the same root would race the
//! manifest CAS and break linearizability — the per-root services singleton
//! (eos-daemon) is what guarantees one queue per root.
//!
//! ## Threading model (per RUST-GUIDANCE §5)
//! The Python uses `threading.Thread` + `queue.Queue` + `concurrent.futures`,
//! NOT asyncio, for the queue itself (eos-occ has no tokio dep). The Rust port
//! is an `mpsc` work queue with one dedicated consumer thread named
//! `occ-commit-queue`; each work item carries a `std::sync::mpsc` reply sender
//! (the std analogue of a `oneshot`) so the async daemon can await the result
//! without the OCC crate ever holding a lock across `.await`.

use std::collections::HashSet;
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use eos_protocol::LayerChange;

use crate::error::OccError;
use crate::route::{ChangesetResult, PublishDecision};

/// Dedicated single-writer thread name (reproduce exactly).
// PORT backend/src/sandbox/occ/commit_queue.py:90 — Thread(name="occ-commit-queue")
pub const COMMIT_QUEUE_THREAD_NAME: &str = "occ-commit-queue";

/// Default upper bound on changesets coalesced into one CAS attempt.
// PORT backend/src/sandbox/occ/commit_queue.py:66 — max_batch_size: int = 64
pub const MAX_BATCH_SIZE: usize = 64;

/// Default batch-coalescing window in seconds (2 ms).
///
/// Only paid when a non-blocking drain emptied the queue AND batch headroom
/// remains; otherwise it is dead wall-clock on the single-commit hot path.
// PORT backend/src/sandbox/occ/commit_queue.py:67 — batch_window_s: float = 0.002
pub const BATCH_WINDOW_S: f64 = 0.002;

/// Bounded CAS-mismatch retry budget before `AbortedVersion`.
// PORT backend/src/sandbox/occ/commit_queue.py:27 — MAX_OCC_CAS_RETRIES: int = 3
pub const MAX_OCC_CAS_RETRIES: u32 = 3;

/// A routed changeset ready for the publish transaction.
///
/// One [`PublishDecision`] per disjoint normalized path plus the typed changes
/// to apply; `atomic` requires every path to validate before any path lands.
/// The `snapshot_version` pins the base the CAS check revalidates against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedChangeset {
    /// Base manifest version this changeset was prepared against (`None` =
    /// empty root).
    pub snapshot_version: Option<u64>,
    /// Disjoint per-path route decisions.
    pub path_groups: Vec<PublishDecision>,
    /// Typed mutations (CAS-hashed by the layer-stack publisher).
    pub changes: Vec<LayerChange>,
    /// All-or-nothing publish (Python default `True`).
    pub atomic: bool,
}

/// The publish-transaction half of the layer-stack port the queue drives.
///
/// Defined here as a local placeholder (the real port + adapter live in
/// `eos-layerstack`; do NOT import sibling items still being written). The
/// daemon injects an implementation that revalidates the CAS base and publishes
/// a new manifest version, returning [`PublishConflict`] on a stale base.
// PORT backend/src/sandbox/occ/commit_transaction.py — CommitTransaction.revalidate_and_publish
pub trait CommitTransactionPort: Send {
    /// Revalidate the base hash and atomically publish, or signal a CAS
    /// conflict so the queue can retry.
    fn revalidate_and_publish(
        &self,
        combined: &PreparedChangeset,
    ) -> Result<ChangesetResult, PublishConflict>;
}

/// Signals a manifest CAS mismatch (`ManifestConflictError`) so the writer
/// retries against the fresh base.
// PORT backend/src/sandbox/layer_stack/manifest.py — ManifestConflictError
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishConflict {
    /// The base version the publisher actually observed.
    pub observed_version: Option<u64>,
}

/// One unit of work on the single-writer queue: a prepared changeset plus the
/// reply channel the submitter awaits.
struct WorkItem {
    prepared: PreparedChangeset,
    reply: mpsc::Sender<Result<ChangesetResult, OccError>>,
}

/// Either real work or the stop sentinel that drains and exits the worker.
enum QueueItem {
    Work(WorkItem),
    Stop,
}

/// Serializes OCC publishes while batching disjoint prepared changesets.
///
/// Owns the `mpsc` producer half; the consumer half is moved into the spawned
/// `occ-commit-queue` thread on [`CommitQueue::start`].
pub struct CommitQueue<T: CommitTransactionPort + 'static> {
    sender: mpsc::Sender<QueueItem>,
    receiver: Mutex<Option<mpsc::Receiver<QueueItem>>>,
    transaction: Mutex<Option<T>>,
    handle: Option<std::thread::JoinHandle<()>>,
    max_batch_size: usize,
    batch_window_s: f64,
    max_cas_retries: u32,
    closed: bool,
}

impl<T: CommitTransactionPort + 'static> CommitQueue<T> {
    /// Build a queue with default batching/retry tuning.
    pub fn new(transaction: T) -> Self {
        Self::with_config(
            transaction,
            MAX_BATCH_SIZE,
            BATCH_WINDOW_S,
            MAX_OCC_CAS_RETRIES,
        )
    }

    /// Build a queue with explicit batching/retry tuning.
    ///
    /// Clamps to match Python: `max_batch_size >= 1`, `batch_window_s >= 0.0`,
    /// `max_cas_retries >= 1`.
    // PORT backend/src/sandbox/occ/commit_queue.py:66-74 — __init__ clamps
    pub fn with_config(
        transaction: T,
        max_batch_size: usize,
        batch_window_s: f64,
        max_cas_retries: u32,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            transaction: Mutex::new(Some(transaction)),
            handle: None,
            max_batch_size: max_batch_size.max(1),
            batch_window_s: batch_window_s.max(0.0),
            max_cas_retries: max_cas_retries.max(1),
            closed: false,
        }
    }

    /// Spawn the single `occ-commit-queue` consumer thread.
    // PORT backend/src/sandbox/occ/commit_queue.py:90 — Thread(target=_run, name="occ-commit-queue", daemon=True)
    pub fn start(&mut self) -> Result<(), OccError> {
        if self.closed {
            return Err(OccError::QueueClosed);
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
            .expect("commit queue receiver slot poisoned")
            .take()
            .ok_or(OccError::QueueNotStarted)?;
        let transaction = self
            .transaction
            .lock()
            .expect("commit queue transaction slot poisoned")
            .take()
            .ok_or(OccError::QueueNotStarted)?;
        let max_batch_size = self.max_batch_size;
        let batch_window_s = self.batch_window_s;
        let max_cas_retries = self.max_cas_retries;
        let handle = std::thread::Builder::new()
            .name(COMMIT_QUEUE_THREAD_NAME.to_owned())
            .spawn(move || {
                Self::run(
                    receiver,
                    transaction,
                    max_batch_size,
                    batch_window_s,
                    max_cas_retries,
                );
            })
            .map_err(|err| OccError::WorkerStart(err.to_string()))?;
        self.handle = Some(handle);
        Ok(())
    }

    /// Stop the worker after pending queued work drains.
    // PORT backend/src/sandbox/occ/commit_queue.py — close(): put _STOP then join
    pub fn close(&mut self) -> Result<(), OccError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if self.handle.is_none() {
            return Ok(());
        }
        let _ = self.sender.send(QueueItem::Stop);
        let handle = self.handle.take().expect("checked above");
        handle.join().map_err(|_| OccError::WorkerPanicked)
    }

    /// Enqueue a prepared changeset and return a reply receiver to await on.
    ///
    /// The reply channel is the std analogue of a tokio `oneshot`; the async
    /// daemon awaits it off-thread without the queue holding any lock across
    /// `.await` (RUST-GUIDANCE §5).
    // PORT backend/src/sandbox/occ/commit_queue.py:108-124 — submit(): future + enqueue
    pub fn submit(
        &self,
        prepared: PreparedChangeset,
    ) -> Result<mpsc::Receiver<Result<ChangesetResult, OccError>>, OccError> {
        if self.closed {
            return Err(OccError::QueueClosed);
        }
        if self
            .handle
            .as_ref()
            .is_none_or(|handle| handle.is_finished())
        {
            return Err(OccError::QueueNotStarted);
        }
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(QueueItem::Work(WorkItem { prepared, reply }))
            .map_err(|_| OccError::QueueClosed)?;
        Ok(receiver)
    }

    /// Consumer loop: block for the first item, non-blocking-drain the rest,
    /// pay the batch window only with headroom, then commit disjoint batches.
    // PORT backend/src/sandbox/occ/commit_queue.py:131 — _run() consumer loop
    fn run(
        receiver: mpsc::Receiver<QueueItem>,
        transaction: T,
        max_batch_size: usize,
        batch_window_s: f64,
        max_cas_retries: u32,
    ) {
        while let Ok(first) = receiver.recv() {
            let QueueItem::Work(first) = first else {
                return;
            };
            let mut items = vec![first];
            let mut stop_seen = drain_ready(&receiver, &mut items, max_batch_size);
            if !stop_seen && batch_window_s > 0.0 && items.len() < max_batch_size {
                std::thread::sleep(Duration::from_secs_f64(batch_window_s));
                stop_seen = drain_ready(&receiver, &mut items, max_batch_size);
            }
            for batch in disjoint_batches(items) {
                Self::commit_batch(&transaction, batch, max_cas_retries);
            }
            if stop_seen {
                return;
            }
        }
    }

    /// Commit one disjoint batch with the bounded CAS-retry loop, fanning each
    /// path's [`FileResult`](crate::FileResult) back to its submitter.
    // PORT backend/src/sandbox/occ/commit_queue.py:168 — _commit_batch(): retry + fan-out
    fn commit_batch(transaction: &T, batch: Vec<WorkItem>, max_cas_retries: u32) {
        let combined = combine_prepared(batch.iter().map(|item| &item.prepared));
        let mut attempts = 0;
        let result = loop {
            match transaction.revalidate_and_publish(&combined) {
                Ok(result) => break result,
                Err(conflict) => {
                    attempts += 1;
                    if attempts >= max_cas_retries {
                        break cas_exhaustion_result(&combined, &conflict, max_cas_retries);
                    }
                }
            }
        };
        for item in batch {
            let files = result_files_for_item(&result, &item.prepared);
            let _ = item.reply.send(Ok(ChangesetResult {
                files,
                published_manifest_version: result.published_manifest_version,
                timings: result.timings.clone(),
            }));
        }
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
            Ok(QueueItem::Stop) => return true,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => return true,
        }
    }
    false
}

fn disjoint_batches(items: Vec<WorkItem>) -> Vec<Vec<WorkItem>> {
    let mut pending: Vec<(WorkItem, HashSet<String>)> = items
        .into_iter()
        .map(|item| {
            let paths = item
                .prepared
                .path_groups
                .iter()
                .map(|group| group.path.as_str().to_owned())
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

fn combine_prepared<'a>(items: impl Iterator<Item = &'a PreparedChangeset>) -> PreparedChangeset {
    let items: Vec<&PreparedChangeset> = items.collect();
    let first = items
        .first()
        .expect("commit queue only combines non-empty batches");
    debug_assert!(items.len() == 1 || !items.iter().any(|prepared| prepared.atomic));
    PreparedChangeset {
        snapshot_version: first.snapshot_version,
        path_groups: items
            .iter()
            .flat_map(|prepared| prepared.path_groups.iter().cloned())
            .collect(),
        changes: items
            .iter()
            .flat_map(|prepared| prepared.changes.iter().cloned())
            .collect(),
        atomic: first.atomic,
    }
}

fn result_files_for_item(
    result: &ChangesetResult,
    prepared: &PreparedChangeset,
) -> Vec<crate::FileResult> {
    prepared
        .path_groups
        .iter()
        .filter_map(|group| {
            result
                .files
                .iter()
                .find(|file| file.path == group.path)
                .cloned()
        })
        .collect()
}

fn cas_exhaustion_result(
    prepared: &PreparedChangeset,
    conflict: &PublishConflict,
    max_cas_retries: u32,
) -> ChangesetResult {
    let message = format!(
        "CAS mismatch retry budget exhausted after {max_cas_retries} attempts: observed version {:?}",
        conflict.observed_version
    );
    let files = prepared
        .path_groups
        .iter()
        .map(|group| match group.route {
            crate::Route::Drop => crate::FileResult {
                path: group.path.clone(),
                status: crate::OccStatus::Dropped,
                message: group.message.clone().unwrap_or_default(),
            },
            crate::Route::Reject => crate::FileResult {
                path: group.path.clone(),
                status: crate::OccStatus::Rejected,
                message: group.message.clone().unwrap_or_default(),
            },
            crate::Route::Direct | crate::Route::Gated => crate::FileResult {
                path: group.path.clone(),
                status: crate::OccStatus::AbortedVersion,
                message: message.clone(),
            },
        })
        .collect();
    ChangesetResult {
        files,
        published_manifest_version: None,
        timings: std::collections::BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use eos_protocol::LayerPath;

    use super::*;
    use crate::{FileResult, OccStatus, Route};

    #[derive(Clone)]
    struct RecordingTransaction {
        calls: Arc<Mutex<Vec<PreparedChangeset>>>,
        conflicts_before_success: Arc<Mutex<u32>>,
    }

    impl RecordingTransaction {
        fn new(conflicts_before_success: u32) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                conflicts_before_success: Arc::new(Mutex::new(conflicts_before_success)),
            }
        }
    }

    impl CommitTransactionPort for RecordingTransaction {
        fn revalidate_and_publish(
            &self,
            combined: &PreparedChangeset,
        ) -> Result<ChangesetResult, PublishConflict> {
            self.calls
                .lock()
                .expect("calls poisoned")
                .push(combined.clone());
            let mut remaining = self
                .conflicts_before_success
                .lock()
                .expect("counter poisoned");
            if *remaining > 0 {
                *remaining -= 1;
                return Err(PublishConflict {
                    observed_version: Some(42),
                });
            }
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
                published_manifest_version: Some(2),
                timings: std::collections::BTreeMap::new(),
            })
        }
    }

    fn prepared(path: &str, atomic: bool) -> PreparedChangeset {
        let path = LayerPath::parse(path).expect("valid test path");
        PreparedChangeset {
            snapshot_version: Some(1),
            path_groups: vec![crate::PublishDecision {
                path: path.clone(),
                route: Route::Gated,
                base_hash: None,
                message: None,
            }],
            changes: vec![eos_protocol::LayerChange::Write {
                path,
                content: b"x".to_vec(),
            }],
            atomic,
        }
    }

    fn recv_ok(receiver: mpsc::Receiver<Result<ChangesetResult, OccError>>) -> ChangesetResult {
        receiver
            .recv()
            .expect("commit queue reply channel stayed open")
            .expect("commit queue returned a changeset result")
    }

    #[test]
    fn batches_disjoint_non_atomic_changesets() {
        let transaction = RecordingTransaction::new(0);
        let calls = transaction.calls.clone();
        let mut queue = CommitQueue::with_config(transaction, 64, 0.02, 3);
        queue.start().expect("queue starts");
        let first = queue
            .submit(prepared("a.txt", false))
            .expect("first submit succeeds");
        let second = queue
            .submit(prepared("b.txt", false))
            .expect("second submit succeeds");

        assert!(recv_ok(first).success());
        assert!(recv_ok(second).success());
        queue.close().expect("queue closes");

        let calls = calls.lock().expect("calls lock not poisoned");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].path_groups.len(), 2);
    }

    #[test]
    fn atomic_changesets_are_not_batched() {
        let transaction = RecordingTransaction::new(0);
        let calls = transaction.calls.clone();
        let mut queue = CommitQueue::with_config(transaction, 64, 0.02, 3);
        queue.start().expect("queue starts");
        let first = queue
            .submit(prepared("a.txt", true))
            .expect("first submit succeeds");
        let second = queue
            .submit(prepared("b.txt", true))
            .expect("second submit succeeds");

        assert!(recv_ok(first).success());
        assert!(recv_ok(second).success());
        queue.close().expect("queue closes");

        let calls = calls.lock().expect("calls lock not poisoned");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].path_groups.len(), 1);
        assert_eq!(calls[1].path_groups.len(), 1);
    }

    #[test]
    fn retries_cas_conflict_then_succeeds() {
        let transaction = RecordingTransaction::new(1);
        let calls = transaction.calls.clone();
        let mut queue = CommitQueue::with_config(transaction, 64, 0.0, 3);
        queue.start().expect("queue starts");
        let result = queue
            .submit(prepared("a.txt", true))
            .expect("submit succeeds");

        assert!(recv_ok(result).success());
        queue.close().expect("queue closes");

        assert_eq!(calls.lock().expect("calls lock not poisoned").len(), 2);
    }

    #[test]
    fn cas_retry_exhaustion_surfaces_aborted_version() {
        let transaction = RecordingTransaction::new(3);
        let mut queue = CommitQueue::with_config(transaction, 64, 0.0, 3);
        queue.start().expect("queue starts");
        let result = queue
            .submit(prepared("a.txt", true))
            .expect("submit succeeds");

        let result = recv_ok(result);
        queue.close().expect("queue closes");

        assert!(!result.success());
        assert_eq!(result.files[0].status, OccStatus::AbortedVersion);
    }
}
