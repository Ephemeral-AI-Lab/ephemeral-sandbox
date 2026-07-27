use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{fsync_dir, read_json, replace_json, FileLock};
use crate::locator::{LocatorDelta, LocatorStore};
use crate::occ::{
    BranchOcc, ChangedPathSet, ConflictAllocation, OccPublication, OccPublishOutcome,
    RebasedCanonical, RetainedOverlapConflict,
};
use crate::owner::current_owner;
use crate::ref_store::{PairedRefStore, RefCommitReceipt};
use crate::{
    AllocationId, CanonicalDurabilityReceipt, LocatorRefCandidate, NamedFaultInjector, OperationId,
    OwnerSubject, PairedRefValue, PocError, PocResult, PublicationId, SCHEMA_VERSION,
};

const RECOVERY_FORMAT: &str = "mpla-poc-recovery-v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableRecoveryPhase {
    Sealing,
    PayloadOwned,
    CanonicalDurable,
    LocatorDurable,
    RefCommitted,
    PublicationCommitted,
    RetainedConflict,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecoveryRequest {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub branch: String,
    pub allocation_root: PathBuf,
    pub allocation_id: AllocationId,
    pub owner_epoch: u64,
    pub accounted_bytes: u64,
    pub locator_delta: LocatorDelta,
    pub candidate: LocatorRefCandidate,
    pub canonical: CanonicalDurabilityReceipt,
    pub changed_paths: ChangedPathSet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryOutcome {
    AwaitingOwnerTransition {
        phase: DurableRecoveryPhase,
        observed: OwnerSubject,
    },
    Committed(RefCommitReceipt),
    Conflict(RetainedOverlapConflict),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoverySnapshot {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub publication_id: PublicationId,
    pub phase: DurableRecoveryPhase,
    pub request_sha256: String,
    pub committed_ref: Option<PairedRefValue>,
    pub conflict: Option<RetainedOverlapConflict>,
}

#[derive(Clone, Debug)]
pub struct PublicationRecovery {
    root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DurableRecoveryRecord {
    schema_version: u32,
    format: String,
    request_sha256: String,
    request: RecoveryRequest,
    working_candidate: LocatorRefCandidate,
    phase: DurableRecoveryPhase,
    committed_ref: Option<PairedRefValue>,
    conflict: Option<RetainedOverlapConflict>,
    state_sha256: String,
}

impl PublicationRecovery {
    pub fn open(root: impl Into<PathBuf>) -> PocResult<Self> {
        let recovery = Self { root: root.into() };
        std::fs::create_dir_all(recovery.root.join("operations")).map_err(|source| {
            PocError::io(
                "create publication recovery root",
                recovery.root.join("operations"),
                source,
            )
        })?;
        fsync_dir(&recovery.root)?;
        Ok(recovery)
    }

    pub fn prepare(&self, request: &RecoveryRequest) -> PocResult<RecoverySnapshot> {
        validate_request(request)?;
        let operation_dir = self.prepare_operation(request.operation_id.as_str())?;
        let _lock = FileLock::exclusive(&operation_dir.join("LOCK"))?;
        let request_sha256 = digest_json(request)?;
        let state_path = operation_dir.join("STATE.json");
        if state_path.exists() {
            let record = read_record(&state_path)?;
            if record.request_sha256 != request_sha256 {
                return Err(PocError::Integrity(
                    "stable operation ID was reused for another recovery request".to_owned(),
                ));
            }
            return Ok(snapshot(&record));
        }
        let mut record = DurableRecoveryRecord {
            schema_version: SCHEMA_VERSION,
            format: RECOVERY_FORMAT.to_owned(),
            request_sha256,
            request: request.clone(),
            working_candidate: request.candidate.clone(),
            phase: DurableRecoveryPhase::Sealing,
            committed_ref: None,
            conflict: None,
            state_sha256: String::new(),
        };
        persist_record(&state_path, &mut record)?;
        Ok(snapshot(&record))
    }

    pub fn inspect(&self, operation_id: &OperationId) -> PocResult<RecoverySnapshot> {
        validate_path_component(operation_id.as_str(), "operation ID")?;
        let operation_dir = self.prepare_operation(operation_id.as_str())?;
        let _lock = FileLock::shared(&operation_dir.join("LOCK"))?;
        read_record(&operation_dir.join("STATE.json")).map(|record| snapshot(&record))
    }

    pub fn replay<F>(
        &self,
        operation_id: &OperationId,
        locator_store: &LocatorStore,
        ref_store: &PairedRefStore,
        occ: &BranchOcc,
        faults: &mut NamedFaultInjector,
        rebase: F,
    ) -> PocResult<RecoveryOutcome>
    where
        F: FnMut(
            &LocatorRefCandidate,
            &PairedRefValue,
            &ChangedPathSet,
        ) -> PocResult<RebasedCanonical>,
    {
        validate_path_component(operation_id.as_str(), "operation ID")?;
        let operation_dir = self.prepare_operation(operation_id.as_str())?;
        let _lock = FileLock::exclusive(&operation_dir.join("LOCK"))?;
        let state_path = operation_dir.join("STATE.json");
        let mut record = read_record(&state_path)?;
        if record.request.operation_id != *operation_id {
            return Err(PocError::Integrity(
                "recovery state operation ID mismatch".to_owned(),
            ));
        }

        let owner = current_owner(&record.request.allocation_root)?;
        match &owner.subject {
            OwnerSubject::PayloadOwned { publication_id }
                if owner.allocation_id == record.request.allocation_id
                    && owner.owner_epoch == record.request.owner_epoch
                    && owner.operation_id == record.request.operation_id
                    && *publication_id == record.request.publication_id => {}
            OwnerSubject::WorkspaceOwned { .. } | OwnerSubject::OwnerTransitionIntent { .. } => {
                return Ok(RecoveryOutcome::AwaitingOwnerTransition {
                    phase: record.phase,
                    observed: owner.subject,
                });
            }
            _ => {
                return Err(PocError::RecoveryRequired(
                    "recovery observed zero or multiple valid owners for the publication"
                        .to_owned(),
                ));
            }
        }

        if let Some(receipt) = ref_store.recover_committed(
            &record.request.branch,
            operation_id.as_str(),
            locator_store,
        )? {
            if receipt.value.publication_id != record.request.publication_id {
                return Err(PocError::RecoveryRequired(
                    "committed paired ref belongs to another publication".to_owned(),
                ));
            }
            record.phase = DurableRecoveryPhase::PublicationCommitted;
            record.committed_ref = Some(receipt.value.clone());
            record.conflict = None;
            persist_record(&state_path, &mut record)?;
            return Ok(RecoveryOutcome::Committed(receipt));
        }

        if let Some(conflict) = record.conflict.clone() {
            validate_conflict_owner(&record.request, &conflict)?;
            return Ok(RecoveryOutcome::Conflict(conflict));
        }
        if let Some(committed) = record.committed_ref.clone() {
            return Err(PocError::RecoveryRequired(format!(
                "recovery state claims paired ref {} but durable head is absent",
                committed.sequence
            )));
        }

        advance_phase(&mut record, DurableRecoveryPhase::PayloadOwned)?;
        persist_record(&state_path, &mut record)?;
        validate_canonical_durability(&record.request.canonical)?;
        advance_phase(&mut record, DurableRecoveryPhase::CanonicalDurable)?;
        persist_record(&state_path, &mut record)?;

        let locator = locator_store.install(&record.request.locator_delta, faults)?;
        record.working_candidate.locator_generation = locator.generation;
        advance_phase(&mut record, DurableRecoveryPhase::LocatorDurable)?;
        persist_record(&state_path, &mut record)?;

        let publication = OccPublication {
            candidate: record.working_candidate.clone(),
            canonical: record.request.canonical.clone(),
            changed_paths: record.request.changed_paths.clone(),
            conflict_allocation: ConflictAllocation {
                allocation_root: record.request.allocation_root.clone(),
                allocation_id: record.request.allocation_id.clone(),
                owner_epoch: record.request.owner_epoch,
                accounted_bytes: record.request.accounted_bytes,
            },
        };
        match occ.publish(
            &record.request.branch,
            &publication,
            locator_store,
            ref_store,
            faults,
            rebase,
        )? {
            OccPublishOutcome::Committed { receipt, .. } => {
                advance_phase(&mut record, DurableRecoveryPhase::RefCommitted)?;
                record.committed_ref = Some(receipt.value.clone());
                persist_record(&state_path, &mut record)?;
                advance_phase(&mut record, DurableRecoveryPhase::PublicationCommitted)?;
                persist_record(&state_path, &mut record)?;
                Ok(RecoveryOutcome::Committed(receipt))
            }
            OccPublishOutcome::Conflict(conflict) => {
                validate_conflict_owner(&record.request, &conflict)?;
                record.phase = DurableRecoveryPhase::RetainedConflict;
                record.conflict = Some(conflict.clone());
                persist_record(&state_path, &mut record)?;
                Ok(RecoveryOutcome::Conflict(conflict))
            }
        }
    }

    fn prepare_operation(&self, operation_id: &str) -> PocResult<PathBuf> {
        validate_path_component(operation_id, "operation ID")?;
        let operation_dir = self.root.join("operations").join(operation_id);
        std::fs::create_dir_all(&operation_dir).map_err(|source| {
            PocError::io(
                "create publication recovery operation",
                &operation_dir,
                source,
            )
        })?;
        create_lock_file(&operation_dir.join("LOCK"))?;
        fsync_dir(&operation_dir)?;
        Ok(operation_dir)
    }
}

fn validate_request(request: &RecoveryRequest) -> PocResult<()> {
    if request.schema_version != SCHEMA_VERSION
        || request.candidate.schema_version != SCHEMA_VERSION
        || request.locator_delta.schema_version != SCHEMA_VERSION
    {
        return Err(PocError::Integrity(
            "unsupported publication recovery request".to_owned(),
        ));
    }
    validate_path_component(request.operation_id.as_str(), "operation ID")?;
    validate_path_component(&request.branch, "branch")?;
    if request.operation_id != request.candidate.operation_id
        || request.operation_id != request.locator_delta.operation_id
        || request.publication_id != request.candidate.publication_id
        || request.publication_id != request.locator_delta.publication_id
        || request.owner_epoch == 0
        || request.accounted_bytes == 0
    {
        return Err(PocError::Integrity(
            "publication recovery identities or ownership accounting disagree".to_owned(),
        ));
    }
    let mut matching_reverse = request
        .locator_delta
        .reverse
        .iter()
        .filter(|entry| entry.allocation_id == request.allocation_id);
    let reverse = matching_reverse.next().ok_or_else(|| {
        PocError::Integrity(
            "publication recovery has no reverse locator for its adopted allocation".to_owned(),
        )
    })?;
    if matching_reverse.next().is_some()
        || reverse.owner_epoch != request.owner_epoch
        || reverse.operation_id != request.operation_id
        || reverse.publication_id != request.publication_id
        || reverse.accounted_bytes != request.accounted_bytes
    {
        return Err(PocError::Integrity(
            "publication recovery reverse locator ownership/accounting disagrees".to_owned(),
        ));
    }
    Ok(())
}

fn validate_canonical_durability(receipt: &CanonicalDurabilityReceipt) -> PocResult<()> {
    if !receipt.files_fsynced
        || !receipt.object_directory_fsynced
        || !receipt.manifest_fsynced
        || !receipt.manifest_directory_fsynced
    {
        return Err(PocError::RecoveryRequired(
            "canonical objects are not completely durable".to_owned(),
        ));
    }
    if receipt.object_set_sha256.len() != 64
        || !receipt
            .object_set_sha256
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(PocError::Integrity(
            "canonical object set digest is invalid".to_owned(),
        ));
    }
    File::open(&receipt.root_manifest)
        .and_then(|file| file.sync_all())
        .map_err(|source| {
            PocError::io(
                "verify durable canonical root manifest",
                &receipt.root_manifest,
                source,
            )
        })
}

fn validate_conflict_owner(
    request: &RecoveryRequest,
    conflict: &RetainedOverlapConflict,
) -> PocResult<()> {
    if conflict.operation_id != request.operation_id
        || conflict.publication_id != request.publication_id
        || conflict.allocation_id != request.allocation_id
        || conflict.owner_epoch != request.owner_epoch
        || conflict.accounted_bytes != request.accounted_bytes
    {
        return Err(PocError::RecoveryRequired(
            "recovered conflict does not retain the exact publication allocation".to_owned(),
        ));
    }
    let owner = current_owner(&request.allocation_root)?;
    if owner.allocation_id != request.allocation_id
        || owner.owner_epoch != request.owner_epoch
        || owner.operation_id != request.operation_id
        || owner.subject
            != (OwnerSubject::PayloadOwned {
                publication_id: request.publication_id.clone(),
            })
    {
        return Err(PocError::RecoveryRequired(
            "recovered conflict allocation does not have exactly one owner".to_owned(),
        ));
    }
    Ok(())
}

fn advance_phase(record: &mut DurableRecoveryRecord, next: DurableRecoveryPhase) -> PocResult<()> {
    if record.phase == DurableRecoveryPhase::RetainedConflict
        || record.phase == DurableRecoveryPhase::PublicationCommitted
    {
        if record.phase == next {
            return Ok(());
        }
        return Err(PocError::Integrity(
            "terminal recovery state cannot advance".to_owned(),
        ));
    }
    if next < record.phase {
        return Ok(());
    }
    record.phase = next;
    Ok(())
}

fn persist_record(path: &Path, record: &mut DurableRecoveryRecord) -> PocResult<()> {
    record.state_sha256.clear();
    record.state_sha256 = digest_json(record)?;
    replace_json(path, record)
}

fn read_record(path: &Path) -> PocResult<DurableRecoveryRecord> {
    let record: DurableRecoveryRecord = read_json(path)?;
    if record.schema_version != SCHEMA_VERSION || record.format != RECOVERY_FORMAT {
        return Err(PocError::Integrity(
            "unsupported publication recovery record".to_owned(),
        ));
    }
    let mut expected = record.clone();
    let observed = expected.state_sha256.clone();
    expected.state_sha256.clear();
    if digest_json(&expected)? != observed || digest_json(&record.request)? != record.request_sha256
    {
        return Err(PocError::RecoveryRequired(
            "publication recovery record checksum mismatch".to_owned(),
        ));
    }
    validate_request(&record.request)?;
    Ok(record)
}

fn snapshot(record: &DurableRecoveryRecord) -> RecoverySnapshot {
    RecoverySnapshot {
        schema_version: record.schema_version,
        operation_id: record.request.operation_id.clone(),
        publication_id: record.request.publication_id.clone(),
        phase: record.phase,
        request_sha256: record.request_sha256.clone(),
        committed_ref: record.committed_ref.clone(),
        conflict: record.conflict.clone(),
    }
}

fn create_lock_file(path: &Path) -> PocResult<()> {
    File::options()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| PocError::io("create publication recovery lock", path, source))
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

fn digest_json<T: Serialize>(value: &T) -> PocResult<String> {
    let bytes = serde_json::to_vec(value)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
}
