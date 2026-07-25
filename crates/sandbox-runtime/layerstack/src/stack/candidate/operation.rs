use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_runtime_layerstack_core::{
    decode_v3_record_as, encode_v3_record, BranchId, CanonicalRecordV3, CanonicalSink,
    CanonicalSource, Digest32, Error, ErrorKind, FieldClass, PublicationId, RawDigest,
    RecordKindV3, TlvV3, TypedDigest, ROOT_FORMAT_V3,
};

use super::materialization_operation::recognizes_materialization_state;
use super::refs::{CommitLock, GcBarrier, Head, RefError, RefStage, RefStore, RefTarget};

const OPERATION_ID_DOMAIN: &[u8] = b"EOS-LS3-OPERATION-ID\0";
const STATE_MAX_BYTES: u64 = 4096;
const WORK_MAX_BYTES: usize = 4 * 1024 * 1024;
const RETENTION_SECONDS: u64 = 86_400;
const RECOVERY_BATCH_LIMIT: usize = 1024;
const MAX_REBASE_ATTEMPTS: u8 = 8;
const REQUEST_DEADLINE_SECONDS: u64 = 60;
static NEXT_STATE_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum OperationKind {
    Publish = 1,
    Revert = 2,
    Reset = 3,
    DirtyCheckpoint = 4,
    HiddenValidation = 5,
}

impl OperationKind {
    fn from_byte(value: u8) -> Result<Self, OperationError> {
        match value {
            1 => Ok(Self::Publish),
            2 => Ok(Self::Revert),
            3 => Ok(Self::Reset),
            4 => Ok(Self::DirtyCheckpoint),
            5 => Ok(Self::HiddenValidation),
            _ => Err(OperationError::Invalid("operation kind")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum OperationPhase {
    Preparing = 1,
    Prepared = 2,
    Committed = 3,
    Conflicted = 4,
    Failed = 5,
    Acknowledged = 6,
    Expired = 7,
}

impl OperationPhase {
    fn from_byte(value: u8) -> Result<Self, OperationError> {
        match value {
            1 => Ok(Self::Preparing),
            2 => Ok(Self::Prepared),
            3 => Ok(Self::Committed),
            4 => Ok(Self::Conflicted),
            5 => Ok(Self::Failed),
            6 => Ok(Self::Acknowledged),
            7 => Ok(Self::Expired),
            _ => Err(OperationError::Invalid("operation phase")),
        }
    }

    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Committed | Self::Conflicted | Self::Failed | Self::Acknowledged | Self::Expired
        )
    }

    const fn is_tombstone(self) -> bool {
        matches!(self, Self::Acknowledged | Self::Expired)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TerminalOutcome {
    None,
    Success(Head),
    Conflict {
        error_code: u16,
        conflict_keys: Digest32,
    },
    Failure {
        error_code: u16,
    },
    Reset(Head),
    Tombstone {
        outcome_digest: Digest32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationState {
    pub(crate) kind: OperationKind,
    pub(crate) branch: BranchId,
    pub(crate) publication_id: PublicationId,
    pub(crate) request_digest: Digest32,
    pub(crate) base: Option<RefTarget>,
    pub(crate) base_generation: u64,
    pub(crate) phase: OperationPhase,
    pub(crate) prepared: Option<RefTarget>,
    pub(crate) changed_path_digest: Option<Digest32>,
    pub(crate) rebase_attempts: u8,
    pub(crate) outcome: TerminalOutcome,
    pub(crate) terminal_expiry_unix_seconds: u64,
    pub(crate) acknowledged: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OperationRequest {
    pub(crate) kind: OperationKind,
    pub(crate) branch: BranchId,
    pub(crate) publication_id: PublicationId,
    pub(crate) request_digest: Digest32,
    pub(crate) base: Option<Head>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct OperationId(Digest32);

impl OperationId {
    pub(crate) const fn digest(self) -> Digest32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OperationStage {
    BeforeState,
    DuringSpill,
    ObjectInstall,
    ObjectsDurable,
    AfterPrepared,
    GcBarrier,
    BeforeHead,
    HeadBeforeTerminal,
    TerminalBeforeResponse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OpenDisposition {
    Created,
    Resumed,
    Terminal(TerminalOutcome),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OpenOperation {
    pub(crate) id: OperationId,
    pub(crate) state: OperationState,
    pub(crate) disposition: OpenDisposition,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RecoveryReport {
    pub(crate) inspected: u64,
    pub(crate) repaired_terminals: u64,
    pub(crate) reaped_work_directories: u64,
    pub(crate) deferred: bool,
}

#[derive(Debug)]
pub(crate) enum OperationError {
    Io(std::io::Error),
    Core(Error),
    Ref(RefError),
    Invalid(&'static str),
    IdempotencyMismatch,
    OutcomeExpired,
    ContentionLimit,
    RequestDeadline,
    Injected(OperationStage),
}

impl OperationError {
    pub(crate) const fn kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Core(error) => Some(error.kind()),
            Self::Ref(error) => error.kind(),
            Self::IdempotencyMismatch => Some(ErrorKind::IdempotencyMismatch),
            Self::OutcomeExpired => Some(ErrorKind::OutcomeExpired),
            Self::ContentionLimit => Some(ErrorKind::ContentionLimit),
            Self::RequestDeadline => Some(ErrorKind::RequestDeadline),
            Self::Io(_) | Self::Invalid(_) | Self::Injected(_) => None,
        }
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "candidate operation I/O failed: {error}"),
            Self::Core(error) => write!(formatter, "candidate operation codec failed: {error}"),
            Self::Ref(error) => write!(formatter, "candidate operation ref failed: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid candidate operation: {message}"),
            Self::IdempotencyMismatch => write!(formatter, "candidate request digest changed"),
            Self::OutcomeExpired => write!(formatter, "candidate outcome expired"),
            Self::ContentionLimit => write!(formatter, "candidate rebase attempt limit reached"),
            Self::RequestDeadline => write!(formatter, "candidate operation deadline reached"),
            Self::Injected(stage) => write!(formatter, "injected operation stop at {stage:?}"),
        }
    }
}

impl std::error::Error for OperationError {}

impl From<std::io::Error> for OperationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<Error> for OperationError {
    fn from(error: Error) -> Self {
        Self::Core(error)
    }
}

impl From<RefError> for OperationError {
    fn from(error: RefError) -> Self {
        Self::Ref(error)
    }
}

pub(crate) struct OperationJournal<'a, L>
where
    L: CommitLock,
{
    storage_root: PathBuf,
    lock: &'a L,
}

impl<'a, L> OperationJournal<'a, L>
where
    L: CommitLock,
{
    pub(crate) const fn new(storage_root: PathBuf, lock: &'a L) -> Self {
        Self { storage_root, lock }
    }

    pub(crate) fn operation_id<D>(
        request: &OperationRequest,
        digest: &mut D,
    ) -> Result<OperationId, OperationError>
    where
        D: RawDigest,
    {
        let branch_length = u16::try_from(request.branch.as_bytes().len())
            .map_err(|_| OperationError::Invalid("branch length"))?;
        let mut preimage = Vec::with_capacity(
            OPERATION_ID_DOMAIN.len() + 1 + 2 + request.branch.as_bytes().len() + 16,
        );
        preimage.extend_from_slice(OPERATION_ID_DOMAIN);
        preimage.push(request.kind as u8);
        preimage.extend_from_slice(&branch_length.to_be_bytes());
        preimage.extend_from_slice(request.branch.as_bytes());
        preimage.extend_from_slice(request.publication_id.as_bytes());
        Ok(OperationId(digest.digest_bytes(&preimage)?))
    }

    pub(crate) fn state_path(&self, id: OperationId) -> PathBuf {
        self.operation_path(id).join("STATE")
    }

    pub(crate) fn work_path(&self, id: OperationId) -> PathBuf {
        self.operation_path(id).join("work")
    }

    pub(crate) fn open<D>(
        &self,
        request: &OperationRequest,
        now_unix_seconds: u64,
        digest: &mut D,
    ) -> Result<OpenOperation, OperationError>
    where
        D: RawDigest,
    {
        self.open_with_hook(request, now_unix_seconds, digest, |_| Ok(()))
    }

    pub(crate) fn open_with_hook<D, F>(
        &self,
        request: &OperationRequest,
        now_unix_seconds: u64,
        digest: &mut D,
        mut hook: F,
    ) -> Result<OpenOperation, OperationError>
    where
        D: RawDigest,
        F: FnMut(OperationStage) -> Result<(), OperationError>,
    {
        let id = Self::operation_id(request, digest)?;
        self.with_lock(|| {
            let path = self.state_path(id);
            if let Some(bytes) = read_bounded_optional(&path, STATE_MAX_BYTES)? {
                let mut state = decode_state(&bytes, digest)?;
                validate_request_match(&state, request)?;
                if state.phase.is_tombstone() {
                    return Err(OperationError::OutcomeExpired);
                }
                if state.phase.is_terminal() {
                    if now_unix_seconds >= state.terminal_expiry_unix_seconds {
                        state = tombstone_state(state, OperationPhase::Expired, digest)?;
                        replace_state(&path, &state, digest)?;
                        reap_work(&self.work_path(id))?;
                        return Err(OperationError::OutcomeExpired);
                    }
                    return Ok(OpenOperation {
                        id,
                        disposition: OpenDisposition::Terminal(state.outcome.clone()),
                        state,
                    });
                }
                return Ok(OpenOperation {
                    id,
                    disposition: OpenDisposition::Resumed,
                    state,
                });
            }

            hook(OperationStage::BeforeState)?;
            let state = state_from_request(request);
            replace_state(&path, &state, digest)?;
            Ok(OpenOperation {
                id,
                state,
                disposition: OpenDisposition::Created,
            })
        })
    }

    pub(crate) fn read<D>(
        &self,
        id: OperationId,
        digest: &mut D,
    ) -> Result<OperationState, OperationError>
    where
        D: RawDigest,
    {
        let bytes = read_bounded_optional(&self.state_path(id), STATE_MAX_BYTES)?
            .ok_or(OperationError::Invalid("operation state missing"))?;
        decode_state(&bytes, digest)
    }

    pub(crate) fn stage_changed_path_run<D, F>(
        &self,
        id: OperationId,
        bytes: &[u8],
        digest: &mut D,
        mut hook: F,
    ) -> Result<Digest32, OperationError>
    where
        D: RawDigest,
        F: FnMut(OperationStage) -> Result<(), OperationError>,
    {
        if bytes.len() > WORK_MAX_BYTES {
            return Err(OperationError::Invalid("operation work exceeds 4 MiB"));
        }
        let work = self.work_path(id);
        std::fs::create_dir_all(&work)?;
        fsync_parent(&work)?;
        let temporary = work.join("changed-paths.run.tmp");
        let final_path = work.join("changed-paths.run");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let midpoint = bytes.len() / 2;
        file.write_all(&bytes[..midpoint])?;
        file.sync_all()?;
        hook(OperationStage::DuringSpill)?;
        file.write_all(&bytes[midpoint..])?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &final_path)?;
        fsync_dir(&work)?;
        Ok(digest.digest_bytes(bytes)?)
    }

    pub(crate) fn prepare<D>(
        &self,
        id: OperationId,
        target: RefTarget,
        changed_path_digest: Option<Digest32>,
        rebase_attempts: u8,
        digest: &mut D,
    ) -> Result<OperationState, OperationError>
    where
        D: RawDigest,
    {
        self.prepare_with_hook(
            id,
            target,
            changed_path_digest,
            rebase_attempts,
            digest,
            |_| Ok(()),
        )
    }

    pub(crate) fn prepare_with_hook<D, F>(
        &self,
        id: OperationId,
        target: RefTarget,
        changed_path_digest: Option<Digest32>,
        rebase_attempts: u8,
        digest: &mut D,
        mut hook: F,
    ) -> Result<OperationState, OperationError>
    where
        D: RawDigest,
        F: FnMut(OperationStage) -> Result<(), OperationError>,
    {
        validate_rebase_budget(rebase_attempts, 0)?;
        hook(OperationStage::ObjectInstall)?;
        hook(OperationStage::ObjectsDurable)?;
        let state = self.with_lock(|| {
            let mut state = self.read(id, digest)?;
            if state.phase != OperationPhase::Preparing {
                if state.phase == OperationPhase::Prepared
                    && state.prepared == Some(target)
                    && state.changed_path_digest == changed_path_digest
                    && state.rebase_attempts == rebase_attempts
                {
                    return Ok(state);
                }
                return Err(OperationError::Invalid("operation is not preparing"));
            }
            state.phase = OperationPhase::Prepared;
            state.prepared = Some(target);
            state.changed_path_digest = changed_path_digest;
            state.rebase_attempts = rebase_attempts;
            replace_state(&self.state_path(id), &state, digest)?;
            Ok(state)
        })?;
        hook(OperationStage::AfterPrepared)?;
        Ok(state)
    }

    pub(crate) fn rebase_prepared<D>(
        &self,
        id: OperationId,
        base: Head,
        target: RefTarget,
        rebase_attempts: u8,
        elapsed_seconds: u64,
        digest: &mut D,
    ) -> Result<OperationState, OperationError>
    where
        D: RawDigest,
    {
        validate_rebase_budget(rebase_attempts, elapsed_seconds)?;
        self.with_lock(|| {
            let mut state = self.read(id, digest)?;
            if state.phase != OperationPhase::Prepared {
                return Err(OperationError::Invalid("operation is not prepared"));
            }
            if rebase_attempts <= state.rebase_attempts {
                return Err(OperationError::Invalid(
                    "rebase attempt did not advance monotonically",
                ));
            }
            state.base = Some(base.target);
            state.base_generation = base.generation;
            state.prepared = Some(target);
            state.rebase_attempts = rebase_attempts;
            replace_state(&self.state_path(id), &state, digest)?;
            Ok(state)
        })
    }

    pub(crate) fn finish_conflict<D>(
        &self,
        id: OperationId,
        conflict_keys: Digest32,
        now_unix_seconds: u64,
        digest: &mut D,
    ) -> Result<TerminalOutcome, OperationError>
    where
        D: RawDigest,
    {
        let error_code = ErrorKind::Conflict
            .stage03_code()
            .ok_or(OperationError::Invalid("conflict error code missing"))?;
        self.finish_terminal_error(
            id,
            OperationPhase::Conflicted,
            TerminalOutcome::Conflict {
                error_code,
                conflict_keys,
            },
            now_unix_seconds,
            digest,
        )
    }

    pub(crate) fn finish_failure<D>(
        &self,
        id: OperationId,
        kind: ErrorKind,
        now_unix_seconds: u64,
        digest: &mut D,
    ) -> Result<TerminalOutcome, OperationError>
    where
        D: RawDigest,
    {
        let error_code = kind
            .stage03_code()
            .ok_or(OperationError::Invalid("failure error code missing"))?;
        self.finish_terminal_error(
            id,
            OperationPhase::Failed,
            TerminalOutcome::Failure { error_code },
            now_unix_seconds,
            digest,
        )
    }

    pub(crate) fn commit_success<D, B, F>(
        &self,
        id: OperationId,
        refs: &mut RefStore<'_, L, B>,
        now_unix_seconds: u64,
        digest: &mut D,
        mut hook: F,
    ) -> Result<TerminalOutcome, OperationError>
    where
        D: RawDigest + TypedDigest,
        B: GcBarrier,
        F: FnMut(OperationStage) -> Result<(), OperationError>,
    {
        self.recover_batch(refs, now_unix_seconds, digest)?;
        let state = self.read(id, digest)?;
        if state.phase.is_tombstone() {
            return Err(OperationError::OutcomeExpired);
        }
        if state.phase.is_terminal() {
            return Ok(state.outcome);
        }
        if state.phase != OperationPhase::Prepared {
            return Err(OperationError::Invalid("operation is not prepared"));
        }
        let target = state
            .prepared
            .ok_or(OperationError::Invalid("prepared target missing"))?;
        let generation = match state.base {
            Some(_) => state
                .base_generation
                .checked_add(1)
                .ok_or(OperationError::Core(Error::new(
                    ErrorKind::GenerationOverflow,
                    ROOT_FORMAT_V3,
                    FieldClass::Publication,
                    3,
                )))?,
            None => 0,
        };
        let head = Head {
            target,
            generation,
            publication_id: *state.publication_id.as_bytes(),
        };
        let outcome = if state.kind == OperationKind::Reset {
            TerminalOutcome::Reset(head)
        } else {
            TerminalOutcome::Success(head)
        };
        let expiry = now_unix_seconds
            .checked_add(RETENTION_SECONDS)
            .ok_or(OperationError::Invalid("terminal expiry overflow"))?;
        let mut terminal = state.clone();
        terminal.phase = OperationPhase::Committed;
        terminal.outcome = outcome.clone();
        terminal.terminal_expiry_unix_seconds = expiry;
        terminal.acknowledged = false;
        validate_state(&terminal)?;
        let terminal_bytes = encode_state(&terminal, digest)?;
        let mut prepared_terminal = PreparedStateFile::new(&self.state_path(id), &terminal_bytes)?;

        let expected = state.base.map(|base| Head {
            target: base,
            generation: state.base_generation,
            publication_id: [0; 16],
        });
        let expected = expected.map(|mut head| {
            if let Ok(Some(current)) = refs.read_head(&state.branch, digest) {
                if current.target == head.target && current.generation == head.generation {
                    head.publication_id = current.publication_id;
                }
            }
            head
        });

        let mut hook_failure = None;
        let mut terminal_failure = None;
        let commit_result =
            refs.commit_head_with_hook(&state.branch, expected, head, digest, |stage| {
                let operation_stage = match stage {
                    RefStage::BarrierRegistered => Some(OperationStage::GcBarrier),
                    RefStage::BeforeVisibility => Some(OperationStage::BeforeHead),
                    RefStage::ParentFsynced => Some(OperationStage::HeadBeforeTerminal),
                    RefStage::TempCreated
                    | RefStage::BytesWritten
                    | RefStage::FileFsynced
                    | RefStage::LockAcquired
                    | RefStage::AfterVisibility => None,
                };
                if let Some(stage) = operation_stage {
                    if let Err(error) = hook(stage) {
                        hook_failure = Some(error);
                        return Err(RefError::Invalid("operation failpoint"));
                    }
                }
                if stage == RefStage::ParentFsynced {
                    if let Err(error) = prepared_terminal.replace_and_sync(&self.state_path(id)) {
                        terminal_failure = Some(error);
                        return Err(RefError::Invalid("terminal state installation failed"));
                    }
                }
                Ok(())
            });
        if let Some(error) = hook_failure {
            return Err(error);
        }
        if let Some(error) = terminal_failure {
            return Err(error);
        }
        commit_result?;
        reap_work(&self.work_path(id))?;
        hook(OperationStage::TerminalBeforeResponse)?;
        Ok(outcome)
    }

    pub(crate) fn acknowledge<D>(
        &self,
        id: OperationId,
        digest: &mut D,
    ) -> Result<(), OperationError>
    where
        D: RawDigest,
    {
        self.with_lock(|| {
            let state = self.read(id, digest)?;
            if state.phase.is_tombstone() {
                return Ok(());
            }
            if !state.phase.is_terminal() {
                return Err(OperationError::Invalid(
                    "cannot acknowledge nonterminal state",
                ));
            }
            let state = tombstone_state(state, OperationPhase::Acknowledged, digest)?;
            replace_state(&self.state_path(id), &state, digest)?;
            reap_work(&self.work_path(id)).map(|_| ())
        })
    }

    pub(crate) fn recover_batch<D, B>(
        &self,
        refs: &mut RefStore<'_, L, B>,
        now_unix_seconds: u64,
        digest: &mut D,
    ) -> Result<RecoveryReport, OperationError>
    where
        D: RawDigest,
        B: GcBarrier,
    {
        let operations = self.storage_root.join("operations");
        let mut paths = read_operation_paths(&operations)?;
        let deferred = paths.len() > RECOVERY_BATCH_LIMIT;
        paths.truncate(RECOVERY_BATCH_LIMIT);
        let mut report = RecoveryReport {
            deferred,
            ..RecoveryReport::default()
        };
        for path in paths {
            report.inspected = report.inspected.saturating_add(1);
            let state_path = path.join("STATE");
            let Some(bytes) = read_bounded_optional(&state_path, STATE_MAX_BYTES)? else {
                continue;
            };
            if recognizes_materialization_state(&path, &bytes) {
                continue;
            }
            let state = decode_state(&bytes, digest)?;
            if state.phase == OperationPhase::Preparing {
                if reap_work(&path.join("work"))? {
                    report.reaped_work_directories =
                        report.reaped_work_directories.saturating_add(1);
                }
                continue;
            }
            if state.phase != OperationPhase::Prepared {
                continue;
            }
            let Some(head) = refs.read_head(&state.branch, digest)? else {
                continue;
            };
            if head.publication_id != *state.publication_id.as_bytes() {
                continue;
            }
            let prepared = state
                .prepared
                .ok_or(OperationError::Invalid("prepared target missing"))?;
            let expected_generation = match state.base {
                Some(_) => state
                    .base_generation
                    .checked_add(1)
                    .ok_or(OperationError::Invalid("generation overflow in recovery"))?,
                None => 0,
            };
            if head.target != prepared || head.generation != expected_generation {
                return Err(OperationError::Invalid("head/prepared recovery mismatch"));
            }
            let outcome = if state.kind == OperationKind::Reset {
                TerminalOutcome::Reset(head)
            } else {
                TerminalOutcome::Success(head)
            };
            let mut terminal = state;
            terminal.phase = OperationPhase::Committed;
            terminal.outcome = outcome;
            terminal.terminal_expiry_unix_seconds = now_unix_seconds
                .checked_add(RETENTION_SECONDS)
                .ok_or(OperationError::Invalid("terminal expiry overflow"))?;
            let terminal_bytes = encode_state(&terminal, digest)?;
            let mut prepared_terminal = PreparedStateFile::new(&state_path, &terminal_bytes)?;
            self.with_lock(|| prepared_terminal.replace_and_sync(&state_path))?;
            if reap_work(&path.join("work"))? {
                report.reaped_work_directories = report.reaped_work_directories.saturating_add(1);
            }
            report.repaired_terminals = report.repaired_terminals.saturating_add(1);
        }
        Ok(report)
    }

    fn operation_path(&self, id: OperationId) -> PathBuf {
        self.storage_root
            .join("operations")
            .join(hex_component(id.digest().as_bytes()))
    }

    fn finish_terminal_error<D>(
        &self,
        id: OperationId,
        phase: OperationPhase,
        outcome: TerminalOutcome,
        now_unix_seconds: u64,
        digest: &mut D,
    ) -> Result<TerminalOutcome, OperationError>
    where
        D: RawDigest,
    {
        self.with_lock(|| {
            let mut state = self.read(id, digest)?;
            if state.phase.is_tombstone() {
                return Err(OperationError::OutcomeExpired);
            }
            if state.phase.is_terminal() {
                return Ok(state.outcome);
            }
            if state.phase != OperationPhase::Prepared {
                return Err(OperationError::Invalid("operation is not prepared"));
            }
            state.phase = phase;
            state.outcome = outcome.clone();
            state.terminal_expiry_unix_seconds = now_unix_seconds
                .checked_add(RETENTION_SECONDS)
                .ok_or(OperationError::Invalid("terminal expiry overflow"))?;
            state.acknowledged = false;
            validate_state(&state)?;
            replace_state(&self.state_path(id), &state, digest)?;
            reap_work(&self.work_path(id))?;
            Ok(outcome)
        })
    }

    fn with_lock<T, F>(&self, operation: F) -> Result<T, OperationError>
    where
        F: FnOnce() -> Result<T, OperationError>,
    {
        let mut result = None;
        self.lock.with_exclusive(|| {
            result = Some(operation());
            Ok(())
        })?;
        result.ok_or(OperationError::Invalid("commit lock did not execute"))?
    }
}

pub(crate) fn validate_rebase_budget(
    attempts: u8,
    elapsed_seconds: u64,
) -> Result<(), OperationError> {
    if attempts > MAX_REBASE_ATTEMPTS {
        return Err(OperationError::ContentionLimit);
    }
    if elapsed_seconds > REQUEST_DEADLINE_SECONDS {
        return Err(OperationError::RequestDeadline);
    }
    Ok(())
}

fn state_from_request(request: &OperationRequest) -> OperationState {
    OperationState {
        kind: request.kind,
        branch: request.branch.clone(),
        publication_id: request.publication_id,
        request_digest: request.request_digest,
        base: request.base.map(|head| head.target),
        base_generation: request.base.map_or(0, |head| head.generation),
        phase: OperationPhase::Preparing,
        prepared: None,
        changed_path_digest: None,
        rebase_attempts: 0,
        outcome: TerminalOutcome::None,
        terminal_expiry_unix_seconds: 0,
        acknowledged: false,
    }
}

fn validate_request_match(
    state: &OperationState,
    request: &OperationRequest,
) -> Result<(), OperationError> {
    if state.kind != request.kind
        || state.branch != request.branch
        || state.publication_id != request.publication_id
        || state.request_digest != request.request_digest
    {
        return Err(OperationError::IdempotencyMismatch);
    }
    Ok(())
}

fn tombstone_state<D>(
    mut state: OperationState,
    phase: OperationPhase,
    digest: &mut D,
) -> Result<OperationState, OperationError>
where
    D: RawDigest,
{
    if !matches!(
        phase,
        OperationPhase::Acknowledged | OperationPhase::Expired
    ) {
        return Err(OperationError::Invalid("invalid tombstone phase"));
    }
    let outcome_bytes = encode_outcome(&state.outcome)?;
    state.outcome = TerminalOutcome::Tombstone {
        outcome_digest: digest.digest_bytes(&outcome_bytes)?,
    };
    state.phase = phase;
    state.acknowledged = phase == OperationPhase::Acknowledged;
    validate_state(&state)?;
    Ok(state)
}

pub(crate) fn encode_state<D>(
    state: &OperationState,
    digest: &mut D,
) -> Result<Vec<u8>, OperationError>
where
    D: RawDigest,
{
    validate_state(state)?;
    let (base_root, base_attribution) = encode_optional_pair(state.base);
    let (prepared_root, prepared_attribution) = encode_optional_pair(state.prepared);
    let record = CanonicalRecordV3::mutable(
        RecordKindV3::OperationState,
        vec![
            TlvV3::new(1, vec![state.kind as u8]),
            TlvV3::new(2, state.branch.as_bytes().to_vec()),
            TlvV3::new(3, state.publication_id.as_bytes().to_vec()),
            TlvV3::new(4, state.request_digest.into_bytes().to_vec()),
            TlvV3::new(5, base_root),
            TlvV3::new(6, base_attribution),
            TlvV3::new(7, state.base_generation.to_be_bytes().to_vec()),
            TlvV3::new(8, vec![state.phase as u8]),
            TlvV3::new(9, prepared_root),
            TlvV3::new(10, prepared_attribution),
            TlvV3::new(11, encode_optional_digest(state.changed_path_digest)),
            TlvV3::new(12, vec![state.rebase_attempts]),
            TlvV3::new(13, encode_outcome(&state.outcome)?),
            TlvV3::new(
                14,
                state.terminal_expiry_unix_seconds.to_be_bytes().to_vec(),
            ),
            TlvV3::new(15, vec![u8::from(state.acknowledged)]),
        ],
        digest,
    )?;
    let mut sink = VecSink::default();
    encode_v3_record(&record, &mut sink)?;
    Ok(sink.bytes)
}

fn decode_state<D>(bytes: &[u8], digest: &mut D) -> Result<OperationState, OperationError>
where
    D: RawDigest,
{
    let mut source = SliceSource::new(bytes);
    let record = decode_v3_record_as(&mut source, RecordKindV3::OperationState, digest)?;
    let fields = record
        .fields()
        .ok_or(OperationError::Invalid("operation fields missing"))?;
    let base = decode_optional_pair(&fields[4], &fields[5])?;
    let prepared = decode_optional_pair(&fields[8], &fields[9])?;
    let publication_id = PublicationId::new(fixed::<16>(fields[2].value())?)?;
    let state = OperationState {
        kind: OperationKind::from_byte(one(fields[0].value())?)?,
        branch: BranchId::new(fields[1].value().to_vec())?,
        publication_id,
        request_digest: Digest32::new(fixed::<32>(fields[3].value())?),
        base,
        base_generation: u64_value(fields[6].value())?,
        phase: OperationPhase::from_byte(one(fields[7].value())?)?,
        prepared,
        changed_path_digest: decode_optional_digest(fields[10].value())?,
        rebase_attempts: one(fields[11].value())?,
        outcome: decode_outcome(fields[12].value())?,
        terminal_expiry_unix_seconds: u64_value(fields[13].value())?,
        acknowledged: match one(fields[14].value())? {
            0 => false,
            1 => true,
            _ => return Err(OperationError::Invalid("acknowledged flag")),
        },
    };
    validate_state(&state)?;
    Ok(state)
}

fn validate_state(state: &OperationState) -> Result<(), OperationError> {
    if state.rebase_attempts > MAX_REBASE_ATTEMPTS {
        return Err(OperationError::ContentionLimit);
    }
    if state.base.is_none() && state.base_generation != 0 {
        return Err(OperationError::Invalid(
            "missing base with nonzero generation",
        ));
    }
    let valid = match (&state.phase, &state.outcome) {
        (OperationPhase::Preparing, TerminalOutcome::None) => {
            state.prepared.is_none()
                && state.changed_path_digest.is_none()
                && state.terminal_expiry_unix_seconds == 0
                && !state.acknowledged
        }
        (OperationPhase::Prepared, TerminalOutcome::None) => {
            state.prepared.is_some()
                && state.terminal_expiry_unix_seconds == 0
                && !state.acknowledged
        }
        (OperationPhase::Committed, TerminalOutcome::Success(head))
        | (OperationPhase::Committed, TerminalOutcome::Reset(head)) => {
            state.prepared == Some(head.target)
                && head.publication_id == *state.publication_id.as_bytes()
                && state.terminal_expiry_unix_seconds != 0
                && !state.acknowledged
        }
        (OperationPhase::Conflicted, TerminalOutcome::Conflict { error_code, .. }) => {
            valid_error_code(*error_code)
                && state.terminal_expiry_unix_seconds != 0
                && !state.acknowledged
        }
        (OperationPhase::Failed, TerminalOutcome::Failure { error_code }) => {
            valid_error_code(*error_code)
                && state.terminal_expiry_unix_seconds != 0
                && !state.acknowledged
        }
        (OperationPhase::Acknowledged, TerminalOutcome::Tombstone { .. }) => state.acknowledged,
        (OperationPhase::Expired, TerminalOutcome::Tombstone { .. }) => !state.acknowledged,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(OperationError::Invalid("operation phase/outcome mismatch"))
    }
}

fn valid_error_code(code: u16) -> bool {
    (1..=32).contains(&code)
}

fn encode_outcome(outcome: &TerminalOutcome) -> Result<Vec<u8>, OperationError> {
    let mut bytes = Vec::new();
    match outcome {
        TerminalOutcome::None => bytes.push(0),
        TerminalOutcome::Success(head) | TerminalOutcome::Reset(head) => {
            bytes.push(if matches!(outcome, TerminalOutcome::Success(_)) {
                1
            } else {
                4
            });
            bytes.extend_from_slice(head.target.root.as_bytes());
            bytes.extend_from_slice(head.target.attribution_root.as_bytes());
            bytes.extend_from_slice(&head.generation.to_be_bytes());
            bytes.extend_from_slice(&head.publication_id);
        }
        TerminalOutcome::Conflict {
            error_code,
            conflict_keys,
        } => {
            if !valid_error_code(*error_code) {
                return Err(OperationError::Invalid("terminal conflict error code"));
            }
            bytes.push(2);
            bytes.extend_from_slice(&error_code.to_be_bytes());
            bytes.extend_from_slice(conflict_keys.as_bytes());
        }
        TerminalOutcome::Failure { error_code } => {
            if !valid_error_code(*error_code) {
                return Err(OperationError::Invalid("terminal failure error code"));
            }
            bytes.push(3);
            bytes.extend_from_slice(&error_code.to_be_bytes());
        }
        TerminalOutcome::Tombstone { outcome_digest } => {
            bytes.push(0);
            bytes.extend_from_slice(outcome_digest.as_bytes());
        }
    }
    Ok(bytes)
}

fn decode_outcome(bytes: &[u8]) -> Result<TerminalOutcome, OperationError> {
    let Some((kind, body)) = bytes.split_first() else {
        return Err(OperationError::Invalid("empty terminal outcome"));
    };
    match (*kind, body.len()) {
        (0, 0) => Ok(TerminalOutcome::None),
        (0, 32) => Ok(TerminalOutcome::Tombstone {
            outcome_digest: Digest32::new(fixed::<32>(body)?),
        }),
        (1 | 4, 88) => {
            let head = Head {
                target: RefTarget {
                    root: Digest32::new(fixed::<32>(&body[..32])?),
                    attribution_root: Digest32::new(fixed::<32>(&body[32..64])?),
                },
                generation: u64_value(&body[64..72])?,
                publication_id: fixed::<16>(&body[72..88])?,
            };
            if *kind == 1 {
                Ok(TerminalOutcome::Success(head))
            } else {
                Ok(TerminalOutcome::Reset(head))
            }
        }
        (2, 34) => {
            let error_code = u16::from_be_bytes(fixed::<2>(&body[..2])?);
            if !valid_error_code(error_code) {
                return Err(OperationError::Invalid("terminal conflict error code"));
            }
            Ok(TerminalOutcome::Conflict {
                error_code,
                conflict_keys: Digest32::new(fixed::<32>(&body[2..])?),
            })
        }
        (3, 2) => {
            let error_code = u16::from_be_bytes(fixed::<2>(body)?);
            if !valid_error_code(error_code) {
                return Err(OperationError::Invalid("terminal failure error code"));
            }
            Ok(TerminalOutcome::Failure { error_code })
        }
        _ => Err(OperationError::Invalid("terminal outcome framing")),
    }
}

fn encode_optional_pair(target: Option<RefTarget>) -> (Vec<u8>, Vec<u8>) {
    match target {
        Some(target) => {
            let mut root = Vec::with_capacity(33);
            root.push(1);
            root.extend_from_slice(target.root.as_bytes());
            let mut attribution = Vec::with_capacity(33);
            attribution.push(1);
            attribution.extend_from_slice(target.attribution_root.as_bytes());
            (root, attribution)
        }
        None => (vec![0], vec![0]),
    }
}

fn decode_optional_pair(
    root: &TlvV3,
    attribution: &TlvV3,
) -> Result<Option<RefTarget>, OperationError> {
    match (
        decode_optional_digest(root.value())?,
        decode_optional_digest(attribution.value())?,
    ) {
        (None, None) => Ok(None),
        (Some(root), Some(attribution_root)) => Ok(Some(RefTarget {
            root,
            attribution_root,
        })),
        _ => Err(OperationError::Invalid("partial root/attribution pair")),
    }
}

fn encode_optional_digest(value: Option<Digest32>) -> Vec<u8> {
    match value {
        Some(value) => {
            let mut bytes = Vec::with_capacity(33);
            bytes.push(1);
            bytes.extend_from_slice(value.as_bytes());
            bytes
        }
        None => vec![0],
    }
}

fn decode_optional_digest(bytes: &[u8]) -> Result<Option<Digest32>, OperationError> {
    match bytes {
        [0] => Ok(None),
        [1, rest @ ..] if rest.len() == 32 => Ok(Some(Digest32::new(fixed::<32>(rest)?))),
        _ => Err(OperationError::Invalid("optional digest framing")),
    }
}

fn replace_state<D>(
    path: &Path,
    state: &OperationState,
    digest: &mut D,
) -> Result<(), OperationError>
where
    D: RawDigest,
{
    let bytes = encode_state(state, digest)?;
    let mut prepared = PreparedStateFile::new(path, &bytes)?;
    prepared.replace_and_sync(path)
}

pub(crate) fn replace_common_state(path: &Path, bytes: &[u8]) -> Result<(), OperationError> {
    let mut prepared = PreparedStateFile::new(path, bytes)?;
    prepared.replace_and_sync(path)
}

pub(crate) fn read_common_state(path: &Path) -> Result<Option<Vec<u8>>, OperationError> {
    read_bounded_optional(path, STATE_MAX_BYTES)
}

pub(crate) fn reap_common_work(path: &Path) -> Result<bool, OperationError> {
    reap_work(path)
}

pub(crate) fn sync_common_parent(path: &Path) -> Result<(), OperationError> {
    fsync_parent(path)
}

struct PreparedStateFile {
    path: Option<PathBuf>,
}

impl PreparedStateFile {
    fn new(final_path: &Path, bytes: &[u8]) -> Result<Self, OperationError> {
        if bytes.len() as u64 > STATE_MAX_BYTES {
            return Err(OperationError::Invalid(
                "operation state exceeds 4096 bytes",
            ));
        }
        let parent = final_path
            .parent()
            .ok_or(OperationError::Invalid("operation state has no parent"))?;
        std::fs::create_dir_all(parent)?;
        fsync_dir(parent)?;
        let temporary = parent.join(format!(
            ".STATE.{}.{}.tmp",
            std::process::id(),
            NEXT_STATE_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let prepared = Self {
            path: Some(temporary.clone()),
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if std::fs::read(&temporary)? != bytes {
            return Err(OperationError::Invalid("prepared state validation failed"));
        }
        Ok(prepared)
    }

    fn replace_and_sync(&mut self, final_path: &Path) -> Result<(), OperationError> {
        let temporary = self
            .path
            .take()
            .ok_or(OperationError::Invalid("prepared state already consumed"))?;
        std::fs::rename(temporary, final_path)?;
        fsync_parent(final_path)
    }
}

impl Drop for PreparedStateFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn read_bounded_optional(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, OperationError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(OperationError::Invalid(
            "operation state is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > maximum {
        return Err(OperationError::Invalid(
            "operation state changed or exceeded its bound",
        ));
    }
    Ok(Some(bytes))
}

fn read_operation_paths(root: &Path) -> Result<Vec<PathBuf>, OperationError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn reap_work(path: &Path) -> Result<bool, OperationError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            std::fs::remove_dir_all(path)?;
            fsync_parent(path)?;
            Ok(true)
        }
        Ok(_) => Err(OperationError::Invalid("operation work is not a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[derive(Default)]
struct VecSink {
    bytes: Vec<u8>,
}

impl CanonicalSink for VecSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

struct SliceSource<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SliceSource<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
}

impl CanonicalSource for SliceSource<'_> {
    fn read_exact(&mut self, output: &mut [u8]) -> Result<(), Error> {
        let end = self.position.checked_add(output.len()).ok_or_else(|| {
            Error::new(
                ErrorKind::ArithmeticOverflow,
                ROOT_FORMAT_V3,
                FieldClass::Length,
                0,
            )
        })?;
        let bytes = self.bytes.get(self.position..end).ok_or_else(|| {
            Error::new(
                ErrorKind::CorruptRecord,
                ROOT_FORMAT_V3,
                FieldClass::Record,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            )
        })?;
        output.copy_from_slice(bytes);
        self.position = end;
        Ok(())
    }

    fn ensure_exhausted(&mut self) -> Result<(), Error> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::TrailingBytes,
                ROOT_FORMAT_V3,
                FieldClass::Record,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ))
        }
    }
}

fn one(bytes: &[u8]) -> Result<u8, OperationError> {
    match bytes {
        [value] => Ok(*value),
        _ => Err(OperationError::Invalid("one-byte field length")),
    }
}

fn fixed<const N: usize>(bytes: &[u8]) -> Result<[u8; N], OperationError> {
    bytes
        .try_into()
        .map_err(|_| OperationError::Invalid("fixed field length"))
}

fn u64_value(bytes: &[u8]) -> Result<u64, OperationError> {
    Ok(u64::from_be_bytes(fixed::<8>(bytes)?))
}

fn hex_component(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(not(windows))]
fn fsync_dir(path: &Path) -> Result<(), OperationError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn fsync_dir(_path: &Path) -> Result<(), OperationError> {
    Ok(())
}

fn fsync_parent(path: &Path) -> Result<(), OperationError> {
    fsync_dir(
        path.parent()
            .ok_or(OperationError::Invalid("path has no parent"))?,
    )
}
