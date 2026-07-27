use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{fsync_dir, read_json, write_immutable_json, FileLock};
use crate::locator::LocatorStore;
use crate::owner::current_owner;
use crate::ref_store::{PairedRefStore, RefCommitOutcome, RefCommitReceipt};
use crate::{
    AllocationId, CanonicalDurabilityReceipt, CanonicalRootPair, LocatorRefCandidate,
    NamedFaultInjector, OperationId, OwnerSubject, PairedRefValue, PocError, PocResult,
    PublicationId, RefSequence, SCHEMA_VERSION,
};

const OCC_FORMAT: &str = "mpla-poc-occ-v1";
const MAX_REBASE_ATTEMPTS: u32 = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChangedPathSet {
    paths: Vec<String>,
}

impl ChangedPathSet {
    pub fn new(paths: impl IntoIterator<Item = String>) -> PocResult<Self> {
        let mut normalized = BTreeSet::new();
        for path in paths {
            validate_changed_path(&path)?;
            normalized.insert(path);
        }
        if normalized.is_empty() {
            return Err(PocError::Integrity(
                "OCC changed path set must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            paths: normalized.into_iter().collect(),
        })
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    pub fn overlaps(&self, other: &Self) -> Vec<PathOverlap> {
        let mut overlaps = Vec::new();
        for incoming in &self.paths {
            for committed in &other.paths {
                if paths_overlap(incoming, committed) {
                    overlaps.push(PathOverlap {
                        incoming: incoming.clone(),
                        committed: committed.clone(),
                    });
                }
            }
        }
        overlaps.sort_by(|left, right| {
            (&left.incoming, &left.committed).cmp(&(&right.incoming, &right.committed))
        });
        overlaps.dedup();
        overlaps
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PathOverlap {
    pub incoming: String,
    pub committed: String,
}

#[derive(Clone, Debug)]
pub struct ConflictAllocation {
    pub allocation_root: PathBuf,
    pub allocation_id: AllocationId,
    pub owner_epoch: u64,
    pub accounted_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetainedOverlapConflict {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub expected_sequence: RefSequence,
    pub observed_sequence: RefSequence,
    pub roots: CanonicalRootPair,
    pub locator_generation: crate::LocatorGeneration,
    pub allocation_id: AllocationId,
    pub owner_epoch: u64,
    pub accounted_bytes: u64,
    pub overlaps: Vec<PathOverlap>,
    pub conflict_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OccPublishOutcome {
    Committed {
        receipt: RefCommitReceipt,
        rebase_count: u32,
    },
    Conflict(RetainedOverlapConflict),
}

#[derive(Clone, Debug)]
pub struct OccPublication {
    pub candidate: LocatorRefCandidate,
    pub canonical: CanonicalDurabilityReceipt,
    pub changed_paths: ChangedPathSet,
    pub conflict_allocation: ConflictAllocation,
}

#[derive(Clone, Debug)]
pub struct RebasedCanonical {
    pub roots: CanonicalRootPair,
    pub durability: CanonicalDurabilityReceipt,
}

#[derive(Clone, Debug)]
pub struct BranchOcc {
    root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ChangeRecord {
    schema_version: u32,
    format: String,
    operation_id: OperationId,
    publication_id: PublicationId,
    parent_sequence: RefSequence,
    sequence: RefSequence,
    roots: CanonicalRootPair,
    locator_generation: crate::LocatorGeneration,
    changed_paths: ChangedPathSet,
    checksum_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ChangeSelection {
    schema_version: u32,
    format: String,
    operation_id: OperationId,
    publication_id: PublicationId,
    sequence: RefSequence,
    change_sha256: String,
    checksum_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ConflictRecord {
    schema_version: u32,
    format: String,
    request_sha256: String,
    conflict: RetainedOverlapConflict,
}

impl BranchOcc {
    pub fn open(root: impl Into<PathBuf>) -> PocResult<Self> {
        let occ = Self { root: root.into() };
        std::fs::create_dir_all(occ.root.join("branches"))
            .map_err(|source| PocError::io("create OCC root", occ.root.join("branches"), source))?;
        fsync_dir(&occ.root)?;
        Ok(occ)
    }

    pub fn publish<F>(
        &self,
        branch: &str,
        publication: &OccPublication,
        locator_store: &LocatorStore,
        ref_store: &PairedRefStore,
        faults: &mut NamedFaultInjector,
        mut rebase: F,
    ) -> PocResult<OccPublishOutcome>
    where
        F: FnMut(
            &LocatorRefCandidate,
            &PairedRefValue,
            &ChangedPathSet,
        ) -> PocResult<RebasedCanonical>,
    {
        validate_path_component(branch, "branch")?;
        validate_publication(publication)?;
        let branch_dir = self.prepare_branch(branch)?;
        let _lock = FileLock::exclusive(&branch_dir.join("LOCK"))?;
        let request_sha256 = publication_request_sha256(publication)?;
        if let Some(conflict) = read_conflict(
            &branch_dir,
            publication.candidate.operation_id.as_str(),
            &request_sha256,
        )? {
            validate_retained_owner(&conflict, &publication.conflict_allocation)?;
            return Ok(OccPublishOutcome::Conflict(conflict));
        }

        let selected_locator = locator_store.selected()?.ok_or_else(|| {
            PocError::RecoveryRequired("OCC publication has no selected locator".to_owned())
        })?;
        let mut locator_receipt = selected_locator.receipt;
        let mut candidate = publication.candidate.clone();
        candidate.locator_generation = locator_receipt.generation;
        let mut canonical = publication.canonical.clone();
        let mut rebase_count = 0;

        for _ in 0..MAX_REBASE_ATTEMPTS {
            let current = ref_store.read(branch)?;
            if let Some(head) = current.as_ref() {
                ensure_change_selection(&branch_dir, head)?;
                if head.operation_id == candidate.operation_id {
                    let receipt = ref_store
                        .recover_committed(branch, candidate.operation_id.as_str(), locator_store)?
                        .ok_or_else(|| {
                            PocError::RecoveryRequired(
                                "matching OCC operation lost its paired ref".to_owned(),
                            )
                        })?;
                    return Ok(OccPublishOutcome::Committed {
                        receipt,
                        rebase_count,
                    });
                }
            }
            let observed_sequence = current
                .as_ref()
                .map_or(RefSequence::ZERO, |head| head.sequence);
            if observed_sequence.get() < candidate.expected_sequence.get() {
                return Err(PocError::Integrity(format!(
                    "OCC expected parent {} is newer than observed head {}",
                    candidate.expected_sequence, observed_sequence
                )));
            }
            if observed_sequence != candidate.expected_sequence {
                let overlaps = self.overlaps_since(
                    &branch_dir,
                    candidate.expected_sequence,
                    observed_sequence,
                    &publication.changed_paths,
                )?;
                if !overlaps.is_empty() {
                    let conflict = retain_conflict(
                        &branch_dir,
                        publication,
                        &candidate,
                        observed_sequence,
                        overlaps,
                        &request_sha256,
                    )?;
                    validate_retained_owner(&conflict, &publication.conflict_allocation)?;
                    return Ok(OccPublishOutcome::Conflict(conflict));
                }
                let head = current.as_ref().ok_or_else(|| {
                    PocError::RecoveryRequired(
                        "OCC observed a nonzero sequence without a paired ref".to_owned(),
                    )
                })?;
                let rebased = rebase(&candidate, head, &publication.changed_paths)?;
                candidate.roots = rebased.roots;
                candidate.expected_sequence = head.sequence;
                canonical = rebased.durability;
                let selected_locator = locator_store.selected()?.ok_or_else(|| {
                    PocError::RecoveryRequired(
                        "selected locator disappeared during OCC rebase".to_owned(),
                    )
                })?;
                locator_receipt = selected_locator.receipt;
                candidate.locator_generation = locator_receipt.generation;
                rebase_count = rebase_count
                    .checked_add(1)
                    .ok_or_else(|| PocError::Integrity("OCC rebase count overflow".to_owned()))?;
                continue;
            }

            let change = change_record(&candidate, &publication.changed_paths)?;
            write_immutable_json(
                &change_candidate_path(&branch_dir, change.sequence, change.operation_id.as_str()),
                &change,
            )?;
            match ref_store.commit(
                branch,
                &candidate,
                &canonical,
                &locator_receipt,
                locator_store,
                faults,
            )? {
                RefCommitOutcome::Committed(receipt) => {
                    persist_change_selection(&branch_dir, &change, &receipt.value)?;
                    return Ok(OccPublishOutcome::Committed {
                        receipt,
                        rebase_count,
                    });
                }
                RefCommitOutcome::ExpectedParent { .. } => continue,
            }
        }
        Err(PocError::RecoveryRequired(format!(
            "OCC exceeded {MAX_REBASE_ATTEMPTS} durable rebase attempts"
        )))
    }

    fn overlaps_since(
        &self,
        branch_dir: &Path,
        expected: RefSequence,
        observed: RefSequence,
        incoming: &ChangedPathSet,
    ) -> PocResult<Vec<PathOverlap>> {
        let mut overlaps = Vec::new();
        for sequence in expected.get().saturating_add(1)..=observed.get() {
            let selection = read_change_selection(branch_dir, sequence)?;
            let record = read_selected_change(branch_dir, &selection)?;
            if record.sequence.get() != sequence {
                return Err(PocError::RecoveryRequired(format!(
                    "OCC change history sequence mismatch at {sequence}"
                )));
            }
            overlaps.extend(incoming.overlaps(&record.changed_paths));
        }
        overlaps.sort();
        overlaps.dedup();
        Ok(overlaps)
    }

    fn prepare_branch(&self, branch: &str) -> PocResult<PathBuf> {
        let branch_dir = self.root.join("branches").join(branch);
        std::fs::create_dir_all(branch_dir.join("changes")).map_err(|source| {
            PocError::io("create OCC changes", branch_dir.join("changes"), source)
        })?;
        std::fs::create_dir_all(branch_dir.join("conflicts")).map_err(|source| {
            PocError::io("create OCC conflicts", branch_dir.join("conflicts"), source)
        })?;
        create_lock_file(&branch_dir.join("LOCK"))?;
        fsync_dir(&branch_dir)?;
        fsync_dir(&self.root.join("branches"))?;
        Ok(branch_dir)
    }
}

fn change_record(
    candidate: &LocatorRefCandidate,
    changed_paths: &ChangedPathSet,
) -> PocResult<ChangeRecord> {
    let mut record = ChangeRecord {
        schema_version: SCHEMA_VERSION,
        format: OCC_FORMAT.to_owned(),
        operation_id: candidate.operation_id.clone(),
        publication_id: candidate.publication_id.clone(),
        parent_sequence: candidate.expected_sequence,
        sequence: candidate.expected_sequence.checked_next()?,
        roots: candidate.roots.clone(),
        locator_generation: candidate.locator_generation,
        changed_paths: changed_paths.clone(),
        checksum_sha256: String::new(),
    };
    record.checksum_sha256 = digest_json(&record)?;
    Ok(record)
}

fn validate_change_record(record: &ChangeRecord) -> PocResult<()> {
    if record.schema_version != SCHEMA_VERSION || record.format != OCC_FORMAT {
        return Err(PocError::Integrity(
            "unsupported OCC change record".to_owned(),
        ));
    }
    if record.parent_sequence.checked_next()? != record.sequence {
        return Err(PocError::RecoveryRequired(
            "OCC change record has a noncontiguous parent".to_owned(),
        ));
    }
    let mut expected = record.clone();
    let observed = expected.checksum_sha256.clone();
    expected.checksum_sha256.clear();
    if digest_json(&expected)? != observed {
        return Err(PocError::RecoveryRequired(
            "OCC change record checksum mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_change_selection(branch_dir: &Path, head: &PairedRefValue) -> PocResult<()> {
    let path = change_selection_path(branch_dir, head.sequence);
    if path.exists() {
        let selection: ChangeSelection = read_json(&path)?;
        validate_change_selection(&selection)?;
        let record = read_selected_change(branch_dir, &selection)?;
        return validate_change_matches_head(&record, head);
    }
    let record: ChangeRecord = read_json(&change_candidate_path(
        branch_dir,
        head.sequence,
        head.operation_id.as_str(),
    ))?;
    validate_change_record(&record)?;
    validate_change_matches_head(&record, head)?;
    persist_change_selection(branch_dir, &record, head)
}

fn persist_change_selection(
    branch_dir: &Path,
    record: &ChangeRecord,
    head: &PairedRefValue,
) -> PocResult<()> {
    validate_change_matches_head(record, head)?;
    let mut selection = ChangeSelection {
        schema_version: SCHEMA_VERSION,
        format: OCC_FORMAT.to_owned(),
        operation_id: record.operation_id.clone(),
        publication_id: record.publication_id.clone(),
        sequence: record.sequence,
        change_sha256: record.checksum_sha256.clone(),
        checksum_sha256: String::new(),
    };
    selection.checksum_sha256 = digest_json(&selection)?;
    write_immutable_json(
        &change_selection_path(branch_dir, record.sequence),
        &selection,
    )
}

fn read_change_selection(branch_dir: &Path, sequence: u64) -> PocResult<ChangeSelection> {
    let path = branch_dir
        .join("changes")
        .join("selected")
        .join(format!("{sequence:020}.json"));
    if !path.exists() {
        return Err(PocError::RecoveryRequired(format!(
            "OCC selected change history is absent for sequence {sequence}"
        )));
    }
    let selection: ChangeSelection = read_json(&path)?;
    validate_change_selection(&selection)?;
    if selection.sequence.get() != sequence {
        return Err(PocError::RecoveryRequired(format!(
            "OCC selected change history sequence mismatch at {sequence}"
        )));
    }
    Ok(selection)
}

fn read_selected_change(branch_dir: &Path, selection: &ChangeSelection) -> PocResult<ChangeRecord> {
    let record: ChangeRecord = read_json(&change_candidate_path(
        branch_dir,
        selection.sequence,
        selection.operation_id.as_str(),
    ))?;
    validate_change_record(&record)?;
    if record.operation_id != selection.operation_id
        || record.publication_id != selection.publication_id
        || record.sequence != selection.sequence
        || record.checksum_sha256 != selection.change_sha256
    {
        return Err(PocError::RecoveryRequired(
            "OCC selected change disagrees with its immutable candidate".to_owned(),
        ));
    }
    Ok(record)
}

fn validate_change_selection(selection: &ChangeSelection) -> PocResult<()> {
    if selection.schema_version != SCHEMA_VERSION || selection.format != OCC_FORMAT {
        return Err(PocError::Integrity(
            "unsupported OCC change selection".to_owned(),
        ));
    }
    let mut expected = selection.clone();
    let observed = expected.checksum_sha256.clone();
    expected.checksum_sha256.clear();
    if digest_json(&expected)? != observed {
        return Err(PocError::RecoveryRequired(
            "OCC change selection checksum mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn validate_change_matches_head(record: &ChangeRecord, head: &PairedRefValue) -> PocResult<()> {
    if record.operation_id != head.operation_id
        || record.publication_id != head.publication_id
        || record.sequence != head.sequence
        || record.roots != head.roots
        || record.locator_generation != head.locator_generation
    {
        return Err(PocError::RecoveryRequired(
            "OCC change record disagrees with its paired ref".to_owned(),
        ));
    }
    Ok(())
}

fn retain_conflict(
    branch_dir: &Path,
    publication: &OccPublication,
    candidate: &LocatorRefCandidate,
    observed_sequence: RefSequence,
    overlaps: Vec<PathOverlap>,
    request_sha256: &str,
) -> PocResult<RetainedOverlapConflict> {
    let owner = current_owner(&publication.conflict_allocation.allocation_root)?;
    if owner.allocation_id != publication.conflict_allocation.allocation_id
        || owner.owner_epoch != publication.conflict_allocation.owner_epoch
        || owner.operation_id != publication.candidate.operation_id
        || owner.subject
            != (OwnerSubject::PayloadOwned {
                publication_id: publication.candidate.publication_id.clone(),
            })
    {
        return Err(PocError::RecoveryRequired(
            "overlap allocation is not exactly payload-owned by the losing operation".to_owned(),
        ));
    }
    let mut conflict = RetainedOverlapConflict {
        schema_version: SCHEMA_VERSION,
        operation_id: candidate.operation_id.clone(),
        publication_id: candidate.publication_id.clone(),
        expected_sequence: candidate.expected_sequence,
        observed_sequence,
        roots: candidate.roots.clone(),
        locator_generation: candidate.locator_generation,
        allocation_id: publication.conflict_allocation.allocation_id.clone(),
        owner_epoch: publication.conflict_allocation.owner_epoch,
        accounted_bytes: publication.conflict_allocation.accounted_bytes,
        overlaps,
        conflict_sha256: String::new(),
    };
    conflict.conflict_sha256 = digest_json(&conflict)?;
    let record = ConflictRecord {
        schema_version: SCHEMA_VERSION,
        format: OCC_FORMAT.to_owned(),
        request_sha256: request_sha256.to_owned(),
        conflict: conflict.clone(),
    };
    write_immutable_json(
        &conflict_path(branch_dir, publication.candidate.operation_id.as_str()),
        &record,
    )?;
    Ok(conflict)
}

fn read_conflict(
    branch_dir: &Path,
    operation_id: &str,
    request_sha256: &str,
) -> PocResult<Option<RetainedOverlapConflict>> {
    let path = conflict_path(branch_dir, operation_id);
    if !path.exists() {
        return Ok(None);
    }
    let record: ConflictRecord = read_json(&path)?;
    if record.schema_version != SCHEMA_VERSION
        || record.format != OCC_FORMAT
        || record.request_sha256 != request_sha256
    {
        return Err(PocError::Integrity(
            "stable operation ID was reused for another OCC request".to_owned(),
        ));
    }
    let mut expected = record.conflict.clone();
    let observed = expected.conflict_sha256.clone();
    expected.conflict_sha256.clear();
    if digest_json(&expected)? != observed {
        return Err(PocError::RecoveryRequired(
            "retained OCC conflict checksum mismatch".to_owned(),
        ));
    }
    Ok(Some(record.conflict))
}

fn validate_retained_owner(
    conflict: &RetainedOverlapConflict,
    allocation: &ConflictAllocation,
) -> PocResult<()> {
    if conflict.accounted_bytes == 0
        || conflict.allocation_id != allocation.allocation_id
        || conflict.owner_epoch != allocation.owner_epoch
        || conflict.accounted_bytes != allocation.accounted_bytes
    {
        return Err(PocError::RecoveryRequired(
            "retained OCC conflict allocation is not fully accounted".to_owned(),
        ));
    }
    let owner = current_owner(&allocation.allocation_root)?;
    if owner.allocation_id != conflict.allocation_id
        || owner.owner_epoch != conflict.owner_epoch
        || owner.operation_id != conflict.operation_id
        || owner.subject
            != (OwnerSubject::PayloadOwned {
                publication_id: conflict.publication_id.clone(),
            })
    {
        return Err(PocError::RecoveryRequired(
            "retained OCC conflict no longer has exactly one valid owner".to_owned(),
        ));
    }
    Ok(())
}

fn validate_publication(publication: &OccPublication) -> PocResult<()> {
    validate_path_component(publication.candidate.operation_id.as_str(), "operation ID")?;
    if publication.candidate.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(
            "unsupported OCC publication candidate".to_owned(),
        ));
    }
    if publication.conflict_allocation.accounted_bytes == 0
        || publication.conflict_allocation.owner_epoch == 0
    {
        return Err(PocError::Integrity(
            "OCC conflict allocation must be owned and accounted".to_owned(),
        ));
    }
    Ok(())
}

fn publication_request_sha256(publication: &OccPublication) -> PocResult<String> {
    #[derive(Serialize)]
    struct RequestDigest<'a> {
        candidate: &'a LocatorRefCandidate,
        canonical: &'a CanonicalDurabilityReceipt,
        changed_paths: &'a ChangedPathSet,
        allocation_id: &'a AllocationId,
        owner_epoch: u64,
        accounted_bytes: u64,
    }
    digest_json(&RequestDigest {
        candidate: &publication.candidate,
        canonical: &publication.canonical,
        changed_paths: &publication.changed_paths,
        allocation_id: &publication.conflict_allocation.allocation_id,
        owner_epoch: publication.conflict_allocation.owner_epoch,
        accounted_bytes: publication.conflict_allocation.accounted_bytes,
    })
}

fn change_candidate_path(branch_dir: &Path, sequence: RefSequence, operation_id: &str) -> PathBuf {
    branch_dir
        .join("changes")
        .join("candidates")
        .join(format!("{:020}", sequence.get()))
        .join(format!("{operation_id}.json"))
}

fn change_selection_path(branch_dir: &Path, sequence: RefSequence) -> PathBuf {
    branch_dir
        .join("changes")
        .join("selected")
        .join(format!("{:020}.json", sequence.get()))
}

fn conflict_path(branch_dir: &Path, operation_id: &str) -> PathBuf {
    branch_dir
        .join("conflicts")
        .join(format!("{operation_id}.json"))
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn validate_changed_path(path: &str) -> PocResult<()> {
    let candidate = Path::new(path);
    if path.is_empty()
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(PocError::Integrity(format!(
            "invalid OCC changed path: {path}"
        )));
    }
    Ok(())
}

fn validate_path_component(value: &str, label: &str) -> PocResult<()> {
    let valid = !value.is_empty()
        && value.len() <= 255
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        return Err(PocError::Integrity(format!(
            "{label} is not a safe path component"
        )));
    }
    Ok(())
}

fn create_lock_file(path: &Path) -> PocResult<()> {
    File::options()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| PocError::io("create OCC lock", path, source))
}

fn digest_json<T: Serialize>(value: &T) -> PocResult<String> {
    let bytes = serde_json::to_vec(value)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
}
