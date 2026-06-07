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
//! The Rust uses `threading.Thread` + `queue.Queue` + `concurrent.futures`,
//! NOT asyncio, for the queue itself (eos-occ has no tokio dep). The Rust port
//! is an `mpsc` work queue with one dedicated consumer thread named
//! `occ-commit-queue`; each work item carries a `std::sync::mpsc` reply sender
//! (the std analogue of a `oneshot`) so the async daemon can await the result
//! without the OCC crate ever holding a lock across `.await`.

use std::collections::HashSet;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use eos_protocol::LayerChange;

use crate::error::OccError;
use crate::route::{ChangesetResult, FileResult, OccStatus, PublishDecision, Route};

/// Dedicated single-writer thread name (reproduce exactly).
pub(crate) const COMMIT_QUEUE_THREAD_NAME: &str = "occ-commit-queue";

/// Default upper bound on changesets coalesced into one CAS attempt.
pub(crate) const MAX_BATCH_SIZE: usize = 64;

/// Default batch-coalescing window in seconds (2 ms).
///
/// Only paid when a non-blocking drain emptied the queue AND batch headroom
/// remains; otherwise it is dead wall-clock on the single-commit hot path.
pub(crate) const BATCH_WINDOW_S: f64 = 0.002;

/// Bounded CAS-mismatch retry budget before `AbortedVersion`.
pub(crate) const MAX_OCC_CAS_RETRIES: u32 = 3;

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
    /// All-or-nothing publish (Rust default `True`).
    pub atomic: bool,
}

/// The publish-transaction half of the layer-stack port the queue drives.
///
/// Defined here as the queue's narrow inverted interface. The daemon injects
/// the layer-stack-backed implementation that revalidates the CAS base and
/// publishes a new manifest version, returning [`PublishConflict`] on a stale
/// base.
pub trait CommitTransactionPort: Send {
    /// Revalidate the base hash and atomically publish, or signal a CAS
    /// conflict so the queue can retry.
    ///
    /// # Errors
    ///
    /// Returns [`PublishConflict`] when the manifest CAS base is stale.
    fn revalidate_and_publish(
        &self,
        combined: &PreparedChangeset,
    ) -> Result<ChangesetResult, PublishConflict>;
}

/// Signals a manifest CAS mismatch (`ManifestConflictError`) so the writer
/// retries against the fresh base.
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
    enqueued_at: Instant,
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

struct CommitWorker<T: CommitTransactionPort + 'static> {
    receiver: mpsc::Receiver<QueueItem>,
    transaction: T,
    max_batch_size: usize,
    batch_window_s: f64,
    max_cas_retries: u32,
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
    /// Clamps to match Rust: `max_batch_size >= 1`, `batch_window_s >= 0.0`,
    /// `max_cas_retries >= 1`.
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
    ///
    /// # Errors
    ///
    /// Returns [`OccError`] when the queue is closed, has already consumed its
    /// startup state, or the worker thread cannot be spawned.
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
            .map_err(|_| OccError::QueueStatePoisoned("receiver slot"))?
            .take()
            .ok_or(OccError::QueueNotStarted)?;
        let transaction = self
            .transaction
            .lock()
            .map_err(|_| OccError::QueueStatePoisoned("transaction slot"))?
            .take()
            .ok_or(OccError::QueueNotStarted)?;
        let worker = CommitWorker {
            receiver,
            transaction,
            max_batch_size: self.max_batch_size,
            batch_window_s: self.batch_window_s,
            max_cas_retries: self.max_cas_retries,
        };
        let handle = std::thread::Builder::new()
            .name(COMMIT_QUEUE_THREAD_NAME.to_owned())
            .spawn(move || {
                worker.run();
            })
            .map_err(|err| OccError::WorkerStart(err.to_string()))?;
        self.handle = Some(handle);
        Ok(())
    }

    /// Stop the worker after pending queued work drains.
    ///
    /// # Errors
    ///
    /// Returns [`OccError::WorkerPanicked`] when the worker thread panicked.
    pub fn close(&mut self) -> Result<(), OccError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        if self.handle.is_none() {
            return Ok(());
        }
        let _ = self.sender.send(QueueItem::Stop);
        self.handle.take().map_or(Ok(()), |handle| {
            handle.join().map_err(|_| OccError::WorkerPanicked)
        })
    }

    /// Enqueue a prepared changeset and return a reply receiver to await on.
    ///
    /// The reply channel is the std analogue of a tokio `oneshot`; the async
    /// daemon awaits it off-thread without the queue holding any lock across
    /// `.await` (RUST-GUIDANCE §5).
    ///
    /// # Errors
    ///
    /// Returns [`OccError::QueueClosed`] if the queue is closed/disconnected and
    /// [`OccError::QueueNotStarted`] if no live worker is available.
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
            .is_none_or(std::thread::JoinHandle::is_finished)
        {
            return Err(OccError::QueueNotStarted);
        }
        let (reply, receiver) = mpsc::channel();
        self.sender
            .send(QueueItem::Work(WorkItem {
                prepared,
                reply,
                enqueued_at: Instant::now(),
            }))
            .map_err(|_| OccError::QueueClosed)?;
        Ok(receiver)
    }

    /// Commit one disjoint batch with the bounded CAS-retry loop, fanning each
    /// path's [`FileResult`](crate::FileResult) back to its submitter.
    fn commit_batch(transaction: &T, batch: Vec<WorkItem>, max_cas_retries: u32) {
        let commit_start = Instant::now();
        let Some(combined) = combine_prepared(batch.iter().map(|item| &item.prepared)) else {
            return;
        };
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
        let commit_elapsed_s = commit_start.elapsed().as_secs_f64();
        let batch_size = usize_to_f64_saturating(batch.len());
        for item in batch {
            let files = result_files_for_item(&result, &item.prepared);
            let mut timings = result.timings.clone();
            timings.insert(
                "occ.serial.queue_wait_s".to_owned(),
                commit_start.duration_since(item.enqueued_at).as_secs_f64(),
            );
            timings.insert("occ.serial.batch_size".to_owned(), batch_size);
            timings.insert("occ.serial.commit_s".to_owned(), commit_elapsed_s);
            timings.insert(
                "occ.serial.cas_attempts".to_owned(),
                f64::from(attempts + 1),
            );
            let _ = item.reply.send(Ok(ChangesetResult {
                files,
                published_manifest_version: result.published_manifest_version,
                timings,
            }));
        }
    }
}

impl<T: CommitTransactionPort + 'static> CommitWorker<T> {
    /// Consumer loop: block for the first item, non-blocking-drain the rest,
    /// pay the batch window only with headroom, then commit disjoint batches.
    fn run(self) {
        while let Ok(first) = self.receiver.recv() {
            let QueueItem::Work(first) = first else {
                return;
            };
            let mut items = vec![first];
            let mut stop_seen = drain_ready(&self.receiver, &mut items, self.max_batch_size);
            if !stop_seen && self.batch_window_s > 0.0 && items.len() < self.max_batch_size {
                std::thread::sleep(Duration::from_secs_f64(self.batch_window_s));
                stop_seen = drain_ready(&self.receiver, &mut items, self.max_batch_size);
            }
            for batch in disjoint_batches(items) {
                CommitQueue::<T>::commit_batch(&self.transaction, batch, self.max_cas_retries);
            }
            if stop_seen {
                return;
            }
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
            Ok(QueueItem::Stop) | Err(mpsc::TryRecvError::Disconnected) => return true,
            Err(mpsc::TryRecvError::Empty) => return false,
        }
    }
    false
}

fn usize_to_f64_saturating(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::from(u32::MAX), f64::from)
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

fn combine_prepared<'a>(
    items: impl Iterator<Item = &'a PreparedChangeset>,
) -> Option<PreparedChangeset> {
    let items: Vec<&PreparedChangeset> = items.collect();
    let first = items.first()?;
    debug_assert!(items.len() == 1 || !items.iter().any(|prepared| prepared.atomic));
    Some(PreparedChangeset {
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
    })
}

fn result_files_for_item(
    result: &ChangesetResult,
    prepared: &PreparedChangeset,
) -> Vec<FileResult> {
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
        .map(|group| {
            let (status, message) = match group.route {
                Route::Drop => (
                    OccStatus::Dropped,
                    group.message.clone().unwrap_or_default(),
                ),
                Route::Reject => (
                    OccStatus::Rejected,
                    group.message.clone().unwrap_or_default(),
                ),
                Route::Direct | Route::Gated => (OccStatus::AbortedVersion, message.clone()),
            };
            FileResult {
                path: group.path.clone(),
                status,
                message,
            }
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

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

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
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(combined.clone());
            let should_conflict = {
                let mut remaining = self
                    .conflicts_before_success
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *remaining > 0 {
                    *remaining -= 1;
                    true
                } else {
                    false
                }
            };
            if should_conflict {
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

    fn prepared(path: &str, atomic: bool) -> TestResult<PreparedChangeset> {
        let path = LayerPath::parse(path)?;
        Ok(PreparedChangeset {
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
        })
    }

    fn recv_ok(
        receiver: &mpsc::Receiver<Result<ChangesetResult, OccError>>,
    ) -> TestResult<ChangesetResult> {
        match receiver.recv()? {
            Ok(result) => Ok(result),
            Err(error) => Err(Box::new(error)),
        }
    }

    #[test]
    fn batches_disjoint_non_atomic_changesets() -> TestResult {
        let transaction = RecordingTransaction::new(0);
        let calls = transaction.calls.clone();
        let mut queue = CommitQueue::with_config(transaction, 64, 0.02, 3);
        queue.start()?;
        let first = queue.submit(prepared("a.txt", false)?)?;
        let second = queue.submit(prepared("b.txt", false)?)?;

        assert!(recv_ok(&first)?.success());
        assert!(recv_ok(&second)?.success());
        queue.close()?;

        {
            let calls = calls
                .lock()
                .map_err(|_| std::io::Error::other("calls lock poisoned"))?;
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].path_groups.len(), 2);
            drop(calls);
        }
        Ok(())
    }

    #[test]
    fn atomic_changesets_are_not_batched() -> TestResult {
        let transaction = RecordingTransaction::new(0);
        let calls = transaction.calls.clone();
        let mut queue = CommitQueue::with_config(transaction, 64, 0.02, 3);
        queue.start()?;
        let first = queue.submit(prepared("a.txt", true)?)?;
        let second = queue.submit(prepared("b.txt", true)?)?;

        assert!(recv_ok(&first)?.success());
        assert!(recv_ok(&second)?.success());
        queue.close()?;

        {
            let calls = calls
                .lock()
                .map_err(|_| std::io::Error::other("calls lock poisoned"))?;
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].path_groups.len(), 1);
            assert_eq!(calls[1].path_groups.len(), 1);
            drop(calls);
        }
        Ok(())
    }

    #[test]
    fn retries_cas_conflict_then_succeeds() -> TestResult {
        let transaction = RecordingTransaction::new(1);
        let calls = transaction.calls.clone();
        let mut queue = CommitQueue::with_config(transaction, 64, 0.0, 3);
        queue.start()?;
        let result = queue.submit(prepared("a.txt", true)?)?;

        assert!(recv_ok(&result)?.success());
        queue.close()?;

        assert_eq!(
            calls
                .lock()
                .map_err(|_| std::io::Error::other("calls lock poisoned"))?
                .len(),
            2
        );
        Ok(())
    }

    #[test]
    fn cas_retry_exhaustion_surfaces_aborted_version() -> TestResult {
        let transaction = RecordingTransaction::new(3);
        let mut queue = CommitQueue::with_config(transaction, 64, 0.0, 3);
        queue.start()?;
        let result = queue.submit(prepared("a.txt", true)?)?;

        let result = recv_ok(&result)?;
        queue.close()?;

        assert!(!result.success());
        assert_eq!(result.files[0].status, OccStatus::AbortedVersion);
        Ok(())
    }
}
