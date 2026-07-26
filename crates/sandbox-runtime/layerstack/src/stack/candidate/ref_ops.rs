use std::fmt;

use sandbox_runtime_layerstack_core::{
    ActorId, AttributionRootId, BranchId, Digest32, PinId, PublicationId, RawDigest, TypedDigest,
};

use super::operation::{
    OpenDisposition, OperationError, OperationJournal, OperationKind, OperationPhase,
    OperationRequest, TerminalOutcome,
};
use super::refs::{CommitLock, GcBarrier, Head, Pin, RefError, RefStore, RefTarget};
use super::tree::{AttributionQuery, PersistentPages, TreeError};

const MAX_REVERT_PROOFS: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ForkMode {
    Writable { branch: BranchId },
    Retained { pin: PinId, reason_class: u8 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ForkOutcome {
    Writable(Head),
    Retained(Pin),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckoutSource {
    Head(BranchId),
    Checkpoint(BranchId),
    Pin(PinId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionSelection {
    pub(crate) source: CheckoutSource,
    pub(crate) target: RefTarget,
}

#[derive(Clone, Debug)]
pub(crate) struct DirtyCheckpointRequest<'a> {
    pub(crate) operation: &'a OperationRequest,
    pub(crate) target: RefTarget,
    pub(crate) changed_path_digest: Option<Digest32>,
    pub(crate) checkpoint: &'a BranchId,
    pub(crate) now_unix_seconds: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RevertRequest<'a> {
    pub(crate) operation: &'a OperationRequest,
    pub(crate) target: RefTarget,
    pub(crate) actor: ActorId,
    pub(crate) reverted: &'a [AttributionQuery],
    pub(crate) now_unix_seconds: u64,
}

#[derive(Debug)]
pub(crate) enum RefOperationError {
    Operation(OperationError),
    Ref(RefError),
    Tree(TreeError),
    Missing(&'static str),
    Invalid(&'static str),
}

impl fmt::Display for RefOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Operation(error) => write!(formatter, "candidate ref operation failed: {error}"),
            Self::Ref(error) => write!(formatter, "candidate ref operation failed: {error}"),
            Self::Tree(error) => write!(formatter, "candidate ref operation failed: {error}"),
            Self::Missing(message) => write!(formatter, "candidate ref is missing: {message}"),
            Self::Invalid(message) => {
                write!(formatter, "invalid candidate ref operation: {message}")
            }
        }
    }
}

impl std::error::Error for RefOperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Operation(error) => Some(error),
            Self::Ref(error) => Some(error),
            Self::Tree(error) => Some(error),
            Self::Missing(_) | Self::Invalid(_) => None,
        }
    }
}

impl From<OperationError> for RefOperationError {
    fn from(error: OperationError) -> Self {
        Self::Operation(error)
    }
}

impl From<RefError> for RefOperationError {
    fn from(error: RefError) -> Self {
        Self::Ref(error)
    }
}

impl From<TreeError> for RefOperationError {
    fn from(error: TreeError) -> Self {
        Self::Tree(error)
    }
}

pub(crate) fn clean_checkpoint<L, B, D>(
    refs: &mut RefStore<'_, L, B>,
    branch: &BranchId,
    checkpoint: &BranchId,
    digest: &mut D,
) -> Result<RefTarget, RefOperationError>
where
    L: CommitLock,
    B: GcBarrier,
    D: RawDigest + TypedDigest,
{
    let head = refs
        .read_head(branch, digest)?
        .ok_or(RefOperationError::Missing("checkpoint source head"))?;
    refs.create_checkpoint(checkpoint, head.target, digest)?;
    Ok(head.target)
}

pub(crate) fn fork_or_pin<L, B, D>(
    refs: &mut RefStore<'_, L, B>,
    source: &BranchId,
    mode: ForkMode,
    digest: &mut D,
) -> Result<ForkOutcome, RefOperationError>
where
    L: CommitLock,
    B: GcBarrier,
    D: RawDigest + TypedDigest,
{
    let source = refs
        .read_head(source, digest)?
        .ok_or(RefOperationError::Missing("fork source head"))?;
    match mode {
        ForkMode::Writable { branch } => {
            let fork = Head {
                target: source.target,
                generation: 0,
                publication_id: source.publication_id,
            };
            refs.commit_head(&branch, None, fork, digest)?;
            Ok(ForkOutcome::Writable(fork))
        }
        ForkMode::Retained { pin, reason_class } => {
            let retained = Pin {
                target: source.target,
                reason_class,
            };
            refs.create_pin(&pin, retained, digest)?;
            Ok(ForkOutcome::Retained(retained))
        }
    }
}

pub(crate) fn checkout<L, B, D>(
    refs: &RefStore<'_, L, B>,
    source: CheckoutSource,
    digest: &mut D,
) -> Result<SessionSelection, RefOperationError>
where
    L: CommitLock,
    B: GcBarrier,
    D: RawDigest,
{
    let target = match &source {
        CheckoutSource::Head(branch) => refs
            .read_head(branch, digest)?
            .map(|head| head.target)
            .ok_or(RefOperationError::Missing("checkout head"))?,
        CheckoutSource::Checkpoint(checkpoint) => refs
            .read_checkpoint(checkpoint, digest)?
            .ok_or(RefOperationError::Missing("checkout checkpoint"))?,
        CheckoutSource::Pin(pin) => refs
            .read_pin(pin, digest)?
            .map(|pin| pin.target)
            .ok_or(RefOperationError::Missing("checkout pin"))?,
    };
    Ok(SessionSelection { source, target })
}

pub(crate) fn dirty_checkpoint<L, B, D>(
    journal: &OperationJournal<'_, L>,
    refs: &mut RefStore<'_, L, B>,
    request: DirtyCheckpointRequest<'_>,
    digest: &mut D,
) -> Result<TerminalOutcome, RefOperationError>
where
    L: CommitLock,
    B: GcBarrier,
    D: RawDigest + TypedDigest,
{
    require_kind(request.operation, OperationKind::DirtyCheckpoint)?;
    let outcome = complete_prepared(
        journal,
        refs,
        request.operation,
        request.target,
        request.changed_path_digest,
        request.now_unix_seconds,
        digest,
    )?;
    let committed = outcome_head(&outcome)?;
    refs.create_checkpoint(request.checkpoint, committed.target, digest)?;
    Ok(outcome)
}

pub(crate) fn revert<L, B, D>(
    journal: &OperationJournal<'_, L>,
    refs: &mut RefStore<'_, L, B>,
    request: RevertRequest<'_>,
    pages: &mut PersistentPages<'_>,
    digest: &mut D,
) -> Result<TerminalOutcome, RefOperationError>
where
    L: CommitLock,
    B: GcBarrier,
    D: RawDigest + TypedDigest,
{
    require_kind(request.operation, OperationKind::Revert)?;
    verify_revert_attribution(
        request.target,
        request.actor,
        request.operation.publication_id,
        request.reverted,
        pages,
    )?;
    complete_prepared(
        journal,
        refs,
        request.operation,
        request.target,
        None,
        request.now_unix_seconds,
        digest,
    )
}

pub(crate) fn reset<L, B, D>(
    journal: &OperationJournal<'_, L>,
    refs: &mut RefStore<'_, L, B>,
    request: &OperationRequest,
    historical: RefTarget,
    now_unix_seconds: u64,
    digest: &mut D,
) -> Result<TerminalOutcome, RefOperationError>
where
    L: CommitLock,
    B: GcBarrier,
    D: RawDigest + TypedDigest,
{
    require_kind(request, OperationKind::Reset)?;
    let outcome = complete_prepared(
        journal,
        refs,
        request,
        historical,
        None,
        now_unix_seconds,
        digest,
    )?;
    if !matches!(outcome, TerminalOutcome::Reset(_)) {
        return Err(RefOperationError::Invalid(
            "reset did not return an explicit reset outcome",
        ));
    }
    Ok(outcome)
}

fn complete_prepared<L, B, D>(
    journal: &OperationJournal<'_, L>,
    refs: &mut RefStore<'_, L, B>,
    request: &OperationRequest,
    target: RefTarget,
    changed_path_digest: Option<Digest32>,
    now_unix_seconds: u64,
    digest: &mut D,
) -> Result<TerminalOutcome, RefOperationError>
where
    L: CommitLock,
    B: GcBarrier,
    D: RawDigest + TypedDigest,
{
    let opened = journal.open_prepared(
        request,
        target,
        changed_path_digest,
        now_unix_seconds,
        digest,
    )?;
    if let OpenDisposition::Terminal(outcome) = opened.disposition {
        return Ok(outcome);
    }
    if opened.state.base != request.base.map(|head| head.target)
        || opened.state.base_generation != request.base.map_or(0, |head| head.generation)
    {
        return Err(RefOperationError::Invalid(
            "operation base changed on retry",
        ));
    }
    if opened.state.phase != OperationPhase::Prepared
        || opened.state.prepared != Some(target)
        || opened.state.changed_path_digest != changed_path_digest
    {
        return Err(RefOperationError::Invalid(
            "ref operation has an invalid prepared state",
        ));
    }
    Ok(journal.commit_success(opened.id, refs, now_unix_seconds, digest, |_| Ok(()))?)
}

fn require_kind(
    request: &OperationRequest,
    expected: OperationKind,
) -> Result<(), RefOperationError> {
    if request.kind != expected {
        return Err(RefOperationError::Invalid("wrong operation kind"));
    }
    Ok(())
}

fn outcome_head(outcome: &TerminalOutcome) -> Result<Head, RefOperationError> {
    match outcome {
        TerminalOutcome::Success(head) | TerminalOutcome::Reset(head) => Ok(*head),
        _ => Err(RefOperationError::Invalid(
            "ref operation has no committed head",
        )),
    }
}

fn verify_revert_attribution(
    target: RefTarget,
    actor: ActorId,
    publication: PublicationId,
    reverted: &[AttributionQuery],
    pages: &mut PersistentPages<'_>,
) -> Result<(), RefOperationError> {
    if reverted.is_empty() || reverted.len() > MAX_REVERT_PROOFS {
        return Err(RefOperationError::Invalid("revert attribution proof count"));
    }
    let (content, attribution) =
        pages.load_attribution_root(AttributionRootId::new(target.attribution_root))?;
    if content.digest() != target.root {
        return Err(RefOperationError::Invalid(
            "revert attribution root names another content root",
        ));
    }
    for query in reverted {
        let facts = pages.query_attribution(attribution, query)?;
        if !facts
            .iter()
            .any(|fact| fact.actor == actor && fact.publication == *publication.as_bytes())
        {
            return Err(RefOperationError::Invalid(
                "reverted path lacks reverting actor attribution",
            ));
        }
    }
    Ok(())
}
