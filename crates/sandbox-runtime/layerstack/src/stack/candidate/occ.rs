use std::fmt;
use std::time::Instant;

use sandbox_runtime_layerstack_core::{Digest32, Error, ErrorKind, RawDigest, RootId, TypedDigest};

use super::operation::{
    validate_rebase_budget, OperationError, OperationId, OperationJournal, TerminalOutcome,
};
use super::refs::{CommitLock, GcBarrier, Head, RefError, RefStore, RefTarget};
use super::spool::{MutationAction, SortedSpool, SpoolError};
use super::tree::{PersistentPages, TreeError};

const CONFLICT_DIGEST_DOMAIN: &[u8] = b"EOS-LS3-CONFLICT-KEYS\0";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct OccCounters {
    pub(crate) head_advances: u64,
    pub(crate) rebase_attempts: u64,
    pub(crate) mutation_records: u64,
    pub(crate) semantic_keys_compared: u64,
    pub(crate) tree_lookups: u64,
    pub(crate) changed_keys: u64,
    pub(crate) grouped_records: u64,
    pub(crate) opaque_records: u64,
    pub(crate) maximum_keys_buffered: u64,
}

impl OccCounters {
    fn add_scan(&mut self, scan: Self) {
        self.mutation_records = self.mutation_records.saturating_add(scan.mutation_records);
        self.semantic_keys_compared = self
            .semantic_keys_compared
            .saturating_add(scan.semantic_keys_compared);
        self.tree_lookups = self.tree_lookups.saturating_add(scan.tree_lookups);
        self.changed_keys = self.changed_keys.saturating_add(scan.changed_keys);
        self.grouped_records = self.grouped_records.saturating_add(scan.grouped_records);
        self.opaque_records = self.opaque_records.saturating_add(scan.opaque_records);
        self.maximum_keys_buffered = self.maximum_keys_buffered.max(scan.maximum_keys_buffered);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConflictScan {
    pub(crate) conflict: bool,
    pub(crate) conflict_key_digest: Digest32,
    pub(crate) counters: OccCounters,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommitReport {
    pub(crate) outcome: TerminalOutcome,
    pub(crate) counters: OccCounters,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommitRequest {
    pub(crate) id: OperationId,
    pub(crate) now_unix_seconds: u64,
}

#[derive(Debug)]
pub(crate) enum OccError {
    Digest(Error),
    Operation(OperationError),
    Ref(RefError),
    Spool(SpoolError),
    Tree(TreeError),
    Invalid(&'static str),
}

impl OccError {
    pub(crate) const fn kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Digest(error) => Some(error.kind()),
            Self::Operation(error) => error.kind(),
            Self::Ref(error) => error.kind(),
            Self::Spool(_) | Self::Tree(_) | Self::Invalid(_) => None,
        }
    }
}

impl fmt::Display for OccError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Digest(error) => write!(formatter, "candidate OCC digest failed: {error}"),
            Self::Operation(error) => write!(formatter, "candidate OCC operation failed: {error}"),
            Self::Ref(error) => write!(formatter, "candidate OCC ref failed: {error}"),
            Self::Spool(error) => write!(formatter, "candidate OCC spool failed: {error}"),
            Self::Tree(error) => write!(formatter, "candidate OCC tree lookup failed: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid candidate OCC state: {message}"),
        }
    }
}

impl std::error::Error for OccError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Digest(error) => Some(error),
            Self::Operation(error) => Some(error),
            Self::Ref(error) => Some(error),
            Self::Spool(error) => Some(error),
            Self::Tree(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

impl From<Error> for OccError {
    fn from(error: Error) -> Self {
        Self::Digest(error)
    }
}

impl From<OperationError> for OccError {
    fn from(error: OperationError) -> Self {
        Self::Operation(error)
    }
}

impl From<RefError> for OccError {
    fn from(error: RefError) -> Self {
        Self::Ref(error)
    }
}

impl From<SpoolError> for OccError {
    fn from(error: SpoolError) -> Self {
        Self::Spool(error)
    }
}

impl From<TreeError> for OccError {
    fn from(error: TreeError) -> Self {
        Self::Tree(error)
    }
}

pub(crate) fn compare_semantic_keys<D>(
    pages: &mut PersistentPages<'_>,
    base: Digest32,
    current: Digest32,
    changed_paths: &SortedSpool,
    digest: &mut D,
) -> Result<ConflictScan, OccError>
where
    D: RawDigest,
{
    let mut conflict_key_digest = digest.digest_bytes(CONFLICT_DIGEST_DOMAIN)?;
    let mut counters = OccCounters::default();
    let mut scan_error = None;
    changed_paths.for_each(|record| {
        if scan_error.is_some() {
            return Ok(());
        }
        counters.mutation_records = counters.mutation_records.saturating_add(1);
        counters.grouped_records = counters
            .grouped_records
            .saturating_add(u64::from(record.conflict_group.is_some()));
        counters.opaque_records = counters.opaque_records.saturating_add(u64::from(matches!(
            record.action,
            MutationAction::OpaqueDirectory
        )));

        for end in record
            .path
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'/').then_some(index))
            .chain(std::iter::once(record.path.len()))
        {
            let key = &record.path[..end];
            counters.semantic_keys_compared = counters.semantic_keys_compared.saturating_add(1);
            counters.tree_lookups = counters.tree_lookups.saturating_add(2);
            counters.maximum_keys_buffered = 1;
            let compared = pages
                .lookup_path(RootId::new(base), key)
                .and_then(|base_value| {
                    pages
                        .lookup_path(RootId::new(current), key)
                        .map(|current_value| (base_value, current_value))
                });
            match compared {
                Ok((base_value, current_value)) if base_value == current_value => {}
                Ok(_) => {
                    counters.changed_keys = counters.changed_keys.saturating_add(1);
                    let mut preimage =
                        Vec::with_capacity(CONFLICT_DIGEST_DOMAIN.len() + 32 + 2 + key.len());
                    preimage.extend_from_slice(CONFLICT_DIGEST_DOMAIN);
                    preimage.extend_from_slice(conflict_key_digest.as_bytes());
                    let length = match u16::try_from(key.len()) {
                        Ok(length) => length,
                        Err(_) => {
                            scan_error = Some(OccError::Invalid("semantic key length"));
                            return Ok(());
                        }
                    };
                    preimage.extend_from_slice(&length.to_be_bytes());
                    preimage.extend_from_slice(key);
                    match digest.digest_bytes(&preimage) {
                        Ok(next) => conflict_key_digest = next,
                        Err(error) => scan_error = Some(OccError::Digest(error)),
                    }
                }
                Err(error) => scan_error = Some(OccError::Tree(error)),
            }
            if scan_error.is_some() {
                break;
            }
        }
        Ok(())
    })?;
    if let Some(error) = scan_error {
        return Err(error);
    }
    Ok(ConflictScan {
        conflict: counters.changed_keys != 0,
        conflict_key_digest,
        counters,
    })
}

pub(crate) fn commit_with_rebase<L, B, D, F>(
    journal: &OperationJournal<'_, L>,
    refs: &mut RefStore<'_, L, B>,
    request: CommitRequest,
    changed_paths: &SortedSpool,
    pages: &mut PersistentPages<'_>,
    digest: &mut D,
    mut rebuild: F,
) -> Result<CommitReport, OccError>
where
    L: CommitLock,
    B: GcBarrier,
    D: RawDigest + TypedDigest,
    F: FnMut(Head, u8) -> Result<RefTarget, OccError>,
{
    let started = Instant::now();
    let mut counters = OccCounters::default();
    loop {
        match journal.commit_success(request.id, refs, request.now_unix_seconds, digest, |_| {
            Ok(())
        }) {
            Ok(outcome) => {
                return Ok(CommitReport { outcome, counters });
            }
            Err(OperationError::Ref(RefError::HeadMismatch)) => {
                counters.head_advances = counters.head_advances.saturating_add(1);
            }
            Err(error) => return Err(error.into()),
        }

        let state = journal.read(request.id, digest)?;
        let current = refs
            .read_head(&state.branch, digest)?
            .ok_or(OccError::Invalid("advanced head is missing"))?;
        let attempt = state
            .rebase_attempts
            .checked_add(1)
            .ok_or(OccError::Operation(OperationError::ContentionLimit))?;
        let elapsed_seconds = started.elapsed().as_secs();
        if let Err(error) = validate_rebase_budget(attempt, elapsed_seconds) {
            let kind = error
                .kind()
                .ok_or(OccError::Invalid("bounded retry error kind missing"))?;
            journal.finish_failure(request.id, kind, request.now_unix_seconds, digest)?;
            return Err(error.into());
        }
        let base = state
            .base
            .ok_or(OccError::Invalid("rebase base is missing"))?;
        let scan =
            compare_semantic_keys(pages, base.root, current.target.root, changed_paths, digest)?;
        counters.add_scan(scan.counters);
        if scan.conflict {
            let outcome = journal.finish_conflict(
                request.id,
                scan.conflict_key_digest,
                request.now_unix_seconds,
                digest,
            )?;
            return Ok(CommitReport { outcome, counters });
        }

        let target = rebuild(current, attempt)?;
        journal.rebase_prepared(
            request.id,
            current,
            target,
            attempt,
            elapsed_seconds,
            digest,
        )?;
        counters.rebase_attempts = counters.rebase_attempts.saturating_add(1);
    }
}
