use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{read_json, replace_json, write_immutable_json, FileLock};
use crate::{
    AdoptionReceipt, AllocationDescriptor, AllocationId, OperationId, OwnerGeneration,
    OwnerSubject, OwnerTransitionRequest, PocError, PocResult, StableAllocationReceipt,
    SCHEMA_VERSION,
};

const JOURNAL_MAGIC: [u8; 4] = *b"MPLJ";
const JOURNAL_FRAME_VERSION: u32 = 1;
const JOURNAL_HEADER_BYTES: usize = 16;
const MAX_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_JOURNAL_RECORD_BYTES: usize = 1024 * 1024;
const BEFORE_SELECTOR_FAULT: &str = ".fault-before-owner-selector-replace";
const AFTER_SELECTOR_FAULT: &str = ".fault-after-owner-selector-replace";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct OwnerSelector {
    schema_version: u32,
    allocation_id: AllocationId,
    owner_epoch: u64,
    operation_id: OperationId,
    journal_sequence: u64,
    journal_record_hash: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JournalPhase {
    WorkspaceLeaseIssued,
    AdoptionIntent,
    OwnerCommitted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum JournalTerminalOutcome {
    Pending,
    WorkspaceOwned,
    PayloadOwned,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct OwnerJournalRecord {
    schema_version: u32,
    sequence: u64,
    allocation_id: AllocationId,
    operation_id: OperationId,
    prior_owner: Option<OwnerGeneration>,
    new_owner: OwnerGeneration,
    prior_owner_epoch: Option<u64>,
    new_owner_epoch: u64,
    phase: JournalPhase,
    terminal_outcome: JournalTerminalOutcome,
    previous_record_hash: Option<String>,
    record_hash: String,
    written_unix_ms: u64,
    checksum_crc32c: u32,
}

struct JournalRead {
    records: Vec<OwnerJournalRecord>,
    valid_bytes: u64,
    torn_tail: bool,
}

pub fn current_owner(allocation_root: &Path) -> PocResult<OwnerGeneration> {
    let _lock = FileLock::exclusive(&owner_lock_path(allocation_root))?;
    current_owner_locked(allocation_root)?.ok_or_else(|| {
        PocError::RecoveryRequired(format!(
            "allocation has no selected owner: {}",
            allocation_root.display()
        ))
    })
}

pub fn compare_and_adopt(
    allocation_root: &Path,
    stable: &StableAllocationReceipt,
    request: &OwnerTransitionRequest,
) -> PocResult<AdoptionReceipt> {
    validate_transition_inputs(allocation_root, stable, request)?;
    validate_path_component(request.operation_id.as_str(), "operation ID")?;
    let _lock = FileLock::exclusive(&owner_lock_path(allocation_root))?;

    if let Some(receipt) = read_receipt(allocation_root, &request.operation_id)? {
        validate_receipt(&receipt, request)?;
        let mut replay = receipt;
        replay.idempotent_replay = true;
        return Ok(replay);
    }

    let current = current_owner_locked(allocation_root)?
        .ok_or_else(|| PocError::OwnerConflict("allocation is not workspace-owned".to_owned()))?;
    match &current.subject {
        OwnerSubject::PayloadOwned { publication_id } => {
            if current.operation_id != request.operation_id
                || *publication_id != request.publication_id
            {
                return Err(PocError::OwnerConflict(format!(
                    "allocation {} is already payload-owned by another operation",
                    request.allocation_id
                )));
            }
            let journal = read_journal(&journal_path(allocation_root))?;
            let committed = find_adoption_commit(&journal.records, request)?;
            let receipt = receipt_from_commit(committed, request, true)?;
            persist_receipt(allocation_root, &receipt)?;
            Ok(receipt)
        }
        OwnerSubject::WorkspaceOwned {
            session_id,
            lease_epoch,
        } => {
            if *session_id != request.session_id
                || *lease_epoch != request.expected_lease_epoch
                || current.owner_epoch != request.expected_owner_epoch
            {
                return Err(PocError::OwnerConflict(format!(
                    "expected WorkspaceOwned({}, lease {}, owner {}), observed owner epoch {}",
                    request.session_id,
                    request.expected_lease_epoch,
                    request.expected_owner_epoch,
                    current.owner_epoch
                )));
            }
            adopt_workspace_owner(allocation_root, stable, request, current)
        }
        OwnerSubject::RecoveryRequired {
            operation_id,
            phase,
        } => Err(PocError::RecoveryRequired(format!(
            "owner recovery required for operation {operation_id} at {phase}"
        ))),
        OwnerSubject::TerminalError { operation_id, code } => Err(PocError::RecoveryRequired(
            format!("terminal owner error for operation {operation_id}: {code}"),
        )),
        OwnerSubject::OwnerTransitionIntent { operation_id, .. } => {
            Err(PocError::RecoveryRequired(format!(
                "selected owner transition intent for operation {operation_id}"
            )))
        }
    }
}

pub(crate) fn owner_lock_path(allocation_root: &Path) -> PathBuf {
    owner_dir(allocation_root).join("LOCK")
}

pub(crate) fn current_owner_locked(allocation_root: &Path) -> PocResult<Option<OwnerGeneration>> {
    let allocation_id = allocation_id_at_root(allocation_root)?;
    let journal_path = journal_path(allocation_root);
    let journal = read_journal(&journal_path)?;
    let mut selected = read_selector(allocation_root)?;

    if let Some(selector) = selected.as_ref() {
        validate_selector(&allocation_id, selector, &journal.records, allocation_root)?;
    }

    for committed in journal.records.iter().filter(|record| {
        matches!(
            record.terminal_outcome,
            JournalTerminalOutcome::WorkspaceOwned | JournalTerminalOutcome::PayloadOwned
        )
    }) {
        let should_advance = selected
            .as_ref()
            .is_none_or(|selector| committed.sequence > selector.journal_sequence);
        if !should_advance {
            continue;
        }
        if let Some(selector) = selected.as_ref() {
            if committed
                .prior_owner
                .as_ref()
                .map(|owner| owner.owner_epoch)
                != Some(selector.owner_epoch)
            {
                return Err(PocError::RecoveryRequired(format!(
                    "owner journal cannot advance selector for allocation {allocation_id}"
                )));
            }
        } else if committed.prior_owner.is_some() {
            return Err(PocError::RecoveryRequired(format!(
                "owner journal starts with a nonempty prior owner for allocation {allocation_id}"
            )));
        }
        install_generation(allocation_root, &committed.new_owner)?;
        let next = selector_for(committed);
        replace_json(&selector_path(allocation_root), &next)?;
        selected = Some(next);
    }

    if journal.torn_tail {
        let selector = selected.as_ref().ok_or_else(|| {
            PocError::RecoveryRequired(format!(
                "torn owner journal has no durable selector for allocation {allocation_id}"
            ))
        })?;
        validate_selector(&allocation_id, selector, &journal.records, allocation_root)?;
        truncate_journal(&journal_path, journal.valid_bytes)?;
    }

    selected
        .map(|selector| load_generation(allocation_root, selector.owner_epoch))
        .transpose()
}

pub(crate) fn selected_owner_locked(allocation_root: &Path) -> PocResult<Option<OwnerGeneration>> {
    let allocation_id = allocation_id_at_root(allocation_root)?;
    let journal = read_journal(&journal_path(allocation_root))?;
    let Some(selector) = read_selector(allocation_root)? else {
        return Ok(None);
    };
    validate_selector(&allocation_id, &selector, &journal.records, allocation_root)?;
    load_generation(allocation_root, selector.owner_epoch).map(Some)
}

pub(crate) fn initialize_workspace_owner_locked(
    allocation_root: &Path,
    owner: OwnerGeneration,
) -> PocResult<()> {
    if current_owner_locked(allocation_root)?.is_some() {
        return Err(PocError::OwnerConflict(format!(
            "allocation {} already has an owner",
            owner.allocation_id
        )));
    }
    install_generation(allocation_root, &owner)?;
    let committed = append_record(
        allocation_root,
        owner.operation_id.clone(),
        None,
        owner,
        JournalPhase::WorkspaceLeaseIssued,
        JournalTerminalOutcome::WorkspaceOwned,
    )?;
    replace_json(&selector_path(allocation_root), &selector_for(&committed))
}

fn adopt_workspace_owner(
    allocation_root: &Path,
    _stable: &StableAllocationReceipt,
    request: &OwnerTransitionRequest,
    prior_owner: OwnerGeneration,
) -> PocResult<AdoptionReceipt> {
    let journal = read_journal(&journal_path(allocation_root))?;
    match journal.records.iter().find(|record| {
        record.operation_id == request.operation_id && record.phase == JournalPhase::AdoptionIntent
    }) {
        Some(record) => validate_intent(record, request, &prior_owner)?,
        None => {
            let intent = OwnerGeneration {
                schema_version: SCHEMA_VERSION,
                allocation_id: request.allocation_id.clone(),
                owner_epoch: prior_owner.owner_epoch,
                previous_owner_epoch: Some(prior_owner.owner_epoch),
                subject: OwnerSubject::OwnerTransitionIntent {
                    operation_id: request.operation_id.clone(),
                    session_id: request.session_id.clone(),
                    expected_owner_epoch: request.expected_owner_epoch,
                    publication_id: request.publication_id.clone(),
                },
                operation_id: request.operation_id.clone(),
                written_unix_ms: crate::unix_time_ms()?,
            };
            append_record(
                allocation_root,
                request.operation_id.clone(),
                Some(prior_owner.clone()),
                intent,
                JournalPhase::AdoptionIntent,
                JournalTerminalOutcome::Pending,
            )?;
        }
    }

    crate::lease::fence_for_adoption_locked(allocation_root, request)?;
    let new_owner = OwnerGeneration {
        schema_version: SCHEMA_VERSION,
        allocation_id: request.allocation_id.clone(),
        owner_epoch: prior_owner.owner_epoch.checked_add(1).ok_or_else(|| {
            PocError::RecoveryRequired("owner epoch exhausted during adoption".to_owned())
        })?,
        previous_owner_epoch: Some(prior_owner.owner_epoch),
        subject: OwnerSubject::PayloadOwned {
            publication_id: request.publication_id.clone(),
        },
        operation_id: request.operation_id.clone(),
        written_unix_ms: crate::unix_time_ms()?,
    };
    let committed = append_record(
        allocation_root,
        request.operation_id.clone(),
        Some(prior_owner),
        new_owner,
        JournalPhase::OwnerCommitted,
        JournalTerminalOutcome::PayloadOwned,
    )?;
    install_generation(allocation_root, &committed.new_owner)?;

    if owner_dir(allocation_root)
        .join(BEFORE_SELECTOR_FAULT)
        .exists()
    {
        return Err(PocError::RecoveryRequired(
            "injected failure before owner selector replacement".to_owned(),
        ));
    }
    replace_json(&selector_path(allocation_root), &selector_for(&committed))?;
    if owner_dir(allocation_root)
        .join(AFTER_SELECTOR_FAULT)
        .exists()
    {
        return Err(PocError::RecoveryRequired(
            "injected failure after owner selector replacement".to_owned(),
        ));
    }

    let receipt = receipt_from_commit(&committed, request, false)?;
    persist_receipt(allocation_root, &receipt)?;
    Ok(receipt)
}

fn validate_transition_inputs(
    allocation_root: &Path,
    stable: &StableAllocationReceipt,
    request: &OwnerTransitionRequest,
) -> PocResult<()> {
    if stable.schema_version != SCHEMA_VERSION || request.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(
            "owner transition schema version mismatch".to_owned(),
        ));
    }
    if !stable.sync_completed || stable.before != stable.after {
        return Err(PocError::OwnerConflict(
            "allocation is not durably stable across both inventories".to_owned(),
        ));
    }
    if stable.operation_id != request.operation_id
        || stable.allocation.allocation_id != request.allocation_id
        || stable.before.allocation_id != request.allocation_id
        || stable.after.allocation_id != request.allocation_id
        || stable.expected_owner_epoch != request.expected_owner_epoch
    {
        return Err(PocError::OwnerConflict(
            "stable allocation receipt does not match owner transition request".to_owned(),
        ));
    }
    let descriptor: AllocationDescriptor = read_json(&allocation_root.join("ALLOCATION.json"))?;
    if descriptor != stable.allocation || descriptor.allocation_id != request.allocation_id {
        return Err(PocError::OwnerConflict(
            "allocation descriptor changed before adoption".to_owned(),
        ));
    }
    Ok(())
}

fn validate_intent(
    record: &OwnerJournalRecord,
    request: &OwnerTransitionRequest,
    prior_owner: &OwnerGeneration,
) -> PocResult<()> {
    let exact = record.prior_owner.as_ref() == Some(prior_owner)
        && record.new_owner.owner_epoch == prior_owner.owner_epoch
        && matches!(
            &record.new_owner.subject,
            OwnerSubject::OwnerTransitionIntent {
                operation_id,
                session_id,
                expected_owner_epoch,
                publication_id,
            } if operation_id == &request.operation_id
                && session_id == &request.session_id
                && *expected_owner_epoch == request.expected_owner_epoch
                && publication_id == &request.publication_id
        );
    if exact {
        Ok(())
    } else {
        Err(PocError::OwnerConflict(
            "durable adoption intent differs from retry".to_owned(),
        ))
    }
}

fn receipt_from_commit(
    committed: &OwnerJournalRecord,
    request: &OwnerTransitionRequest,
    idempotent_replay: bool,
) -> PocResult<AdoptionReceipt> {
    let prior_owner = committed.prior_owner.clone().ok_or_else(|| {
        PocError::RecoveryRequired("adoption commit omitted prior owner".to_owned())
    })?;
    if committed.operation_id != request.operation_id
        || committed.allocation_id != request.allocation_id
        || committed.new_owner.operation_id != request.operation_id
        || !matches!(
            &committed.new_owner.subject,
            OwnerSubject::PayloadOwned { publication_id }
                if publication_id == &request.publication_id
        )
    {
        return Err(PocError::OwnerConflict(
            "adoption commit does not match retry request".to_owned(),
        ));
    }
    Ok(AdoptionReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: request.operation_id.clone(),
        publication_id: request.publication_id.clone(),
        allocation_id: request.allocation_id.clone(),
        prior_owner,
        new_owner: committed.new_owner.clone(),
        idempotent_replay,
        committed_unix_ms: committed.written_unix_ms,
    })
}

fn validate_receipt(receipt: &AdoptionReceipt, request: &OwnerTransitionRequest) -> PocResult<()> {
    if receipt.schema_version != SCHEMA_VERSION
        || receipt.operation_id != request.operation_id
        || receipt.publication_id != request.publication_id
        || receipt.allocation_id != request.allocation_id
        || receipt.prior_owner.owner_epoch != request.expected_owner_epoch
        || !matches!(
            &receipt.prior_owner.subject,
            OwnerSubject::WorkspaceOwned {
                session_id,
                lease_epoch,
            } if session_id == &request.session_id
                && *lease_epoch == request.expected_lease_epoch
        )
    {
        return Err(PocError::OwnerConflict(
            "stored adoption receipt differs from retry".to_owned(),
        ));
    }
    Ok(())
}

fn read_receipt(
    allocation_root: &Path,
    operation_id: &OperationId,
) -> PocResult<Option<AdoptionReceipt>> {
    let path = receipt_path(allocation_root, operation_id);
    match read_json(&path) {
        Ok(receipt) => Ok(Some(receipt)),
        Err(PocError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn persist_receipt(allocation_root: &Path, receipt: &AdoptionReceipt) -> PocResult<()> {
    let mut stored = receipt.clone();
    stored.idempotent_replay = false;
    write_immutable_json(
        &receipt_path(allocation_root, &receipt.operation_id),
        &stored,
    )
}

fn receipt_path(allocation_root: &Path, operation_id: &OperationId) -> PathBuf {
    owner_dir(allocation_root)
        .join("receipts")
        .join(format!("{}.json", operation_id.as_str()))
}

fn find_adoption_commit<'a>(
    records: &'a [OwnerJournalRecord],
    request: &OwnerTransitionRequest,
) -> PocResult<&'a OwnerJournalRecord> {
    records
        .iter()
        .find(|record| {
            record.operation_id == request.operation_id
                && record.phase == JournalPhase::OwnerCommitted
                && record.terminal_outcome == JournalTerminalOutcome::PayloadOwned
        })
        .ok_or_else(|| {
            PocError::RecoveryRequired(format!(
                "payload owner has no adoption commit for operation {}",
                request.operation_id
            ))
        })
}

fn append_record(
    allocation_root: &Path,
    operation_id: OperationId,
    prior_owner: Option<OwnerGeneration>,
    new_owner: OwnerGeneration,
    phase: JournalPhase,
    terminal_outcome: JournalTerminalOutcome,
) -> PocResult<OwnerJournalRecord> {
    let path = journal_path(allocation_root);
    let journal = read_journal(&path)?;
    if journal.torn_tail {
        return Err(PocError::RecoveryRequired(format!(
            "owner journal must be recovered before append: {}",
            path.display()
        )));
    }
    let sequence = u64::try_from(journal.records.len())
        .map_err(|_| PocError::Integrity("owner journal sequence overflow".to_owned()))?
        .checked_add(1)
        .ok_or_else(|| PocError::Integrity("owner journal sequence overflow".to_owned()))?;
    let mut record = OwnerJournalRecord {
        schema_version: SCHEMA_VERSION,
        sequence,
        allocation_id: new_owner.allocation_id.clone(),
        operation_id,
        prior_owner_epoch: prior_owner.as_ref().map(|owner| owner.owner_epoch),
        new_owner_epoch: new_owner.owner_epoch,
        prior_owner,
        new_owner,
        phase,
        terminal_outcome,
        previous_record_hash: journal.records.last().map(|item| item.record_hash.clone()),
        record_hash: String::new(),
        written_unix_ms: crate::unix_time_ms()?,
        checksum_crc32c: 0,
    };
    seal_record(&mut record)?;
    let payload = serde_json::to_vec(&record)?;
    if payload.len() > MAX_JOURNAL_RECORD_BYTES {
        return Err(PocError::Integrity(
            "owner journal record exceeds framing limit".to_owned(),
        ));
    }
    let length = u64::try_from(payload.len())
        .map_err(|_| PocError::Integrity("owner journal record length overflow".to_owned()))?;
    let mut frame = Vec::with_capacity(JOURNAL_HEADER_BYTES + payload.len());
    frame.extend_from_slice(&JOURNAL_MAGIC);
    frame.extend_from_slice(&JOURNAL_FRAME_VERSION.to_le_bytes());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|source| PocError::io("open owner journal for append", &path, source))?;
    file.write_all(&frame)
        .map_err(|source| PocError::io("append owner journal record", &path, source))?;
    file.sync_data()
        .map_err(|source| PocError::io("fdatasync owner journal", &path, source))?;
    Ok(record)
}

fn read_journal(path: &Path) -> PocResult<JournalRead> {
    let mut file =
        File::open(path).map_err(|source| PocError::io("open owner journal", path, source))?;
    let length = file
        .metadata()
        .map_err(|source| PocError::io("stat owner journal", path, source))?
        .len();
    if length > MAX_JOURNAL_BYTES {
        return Err(PocError::Integrity(format!(
            "owner journal exceeds {MAX_JOURNAL_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(length)
            .map_err(|_| PocError::Integrity("owner journal length overflow".to_owned()))?,
    );
    file.read_to_end(&mut bytes)
        .map_err(|source| PocError::io("read owner journal", path, source))?;

    let mut records = Vec::new();
    let mut offset = 0_usize;
    let mut previous_hash = None;
    while offset < bytes.len() {
        if bytes.len() - offset < JOURNAL_HEADER_BYTES {
            break;
        }
        if bytes[offset..offset + 4] != JOURNAL_MAGIC {
            return Err(PocError::Integrity(format!(
                "owner journal frame magic mismatch at byte {offset}"
            )));
        }
        let version = u32::from_le_bytes(
            bytes[offset + 4..offset + 8]
                .try_into()
                .map_err(|_| PocError::Integrity("owner journal version frame".to_owned()))?,
        );
        if version != JOURNAL_FRAME_VERSION {
            return Err(PocError::Integrity(format!(
                "unsupported owner journal frame version {version}"
            )));
        }
        let payload_length = u64::from_le_bytes(
            bytes[offset + 8..offset + 16]
                .try_into()
                .map_err(|_| PocError::Integrity("owner journal length frame".to_owned()))?,
        );
        let payload_length = usize::try_from(payload_length)
            .map_err(|_| PocError::Integrity("owner journal frame length overflow".to_owned()))?;
        if payload_length > MAX_JOURNAL_RECORD_BYTES {
            return Err(PocError::Integrity(format!(
                "owner journal frame exceeds {MAX_JOURNAL_RECORD_BYTES} bytes"
            )));
        }
        let payload_start = offset + JOURNAL_HEADER_BYTES;
        let Some(payload_end) = payload_start.checked_add(payload_length) else {
            return Err(PocError::Integrity(
                "owner journal frame length overflow".to_owned(),
            ));
        };
        if payload_end > bytes.len() {
            break;
        }
        let record: OwnerJournalRecord =
            serde_json::from_slice(&bytes[payload_start..payload_end])?;
        verify_record(&record, records.len(), previous_hash.as_deref())?;
        previous_hash = Some(record.record_hash.clone());
        records.push(record);
        offset = payload_end;
    }
    Ok(JournalRead {
        records,
        valid_bytes: u64::try_from(offset)
            .map_err(|_| PocError::Integrity("owner journal offset overflow".to_owned()))?,
        torn_tail: offset != bytes.len(),
    })
}

fn seal_record(record: &mut OwnerJournalRecord) -> PocResult<()> {
    record.record_hash.clear();
    record.checksum_crc32c = 0;
    record.record_hash = sha256_hex(&serde_json::to_vec(record)?);
    record.checksum_crc32c = 0;
    record.checksum_crc32c = crc32c(&serde_json::to_vec(record)?);
    Ok(())
}

fn verify_record(
    record: &OwnerJournalRecord,
    prior_count: usize,
    previous_hash: Option<&str>,
) -> PocResult<()> {
    let expected_sequence = u64::try_from(prior_count)
        .map_err(|_| PocError::Integrity("owner journal sequence overflow".to_owned()))?
        .checked_add(1)
        .ok_or_else(|| PocError::Integrity("owner journal sequence overflow".to_owned()))?;
    if record.schema_version != SCHEMA_VERSION
        || record.sequence != expected_sequence
        || record.previous_record_hash.as_deref() != previous_hash
        || record.prior_owner_epoch != record.prior_owner.as_ref().map(|owner| owner.owner_epoch)
        || record.new_owner_epoch != record.new_owner.owner_epoch
        || record.allocation_id != record.new_owner.allocation_id
    {
        return Err(PocError::Integrity(format!(
            "owner journal record {} metadata mismatch",
            record.sequence
        )));
    }
    let stored_hash = record.record_hash.clone();
    let stored_crc = record.checksum_crc32c;
    let mut unhashed = record.clone();
    unhashed.record_hash.clear();
    unhashed.checksum_crc32c = 0;
    if sha256_hex(&serde_json::to_vec(&unhashed)?) != stored_hash {
        return Err(PocError::Integrity(format!(
            "owner journal record {} hash mismatch",
            record.sequence
        )));
    }
    unhashed.record_hash = stored_hash;
    if crc32c(&serde_json::to_vec(&unhashed)?) != stored_crc {
        return Err(PocError::Integrity(format!(
            "owner journal record {} checksum mismatch",
            record.sequence
        )));
    }
    Ok(())
}

fn selector_for(record: &OwnerJournalRecord) -> OwnerSelector {
    OwnerSelector {
        schema_version: SCHEMA_VERSION,
        allocation_id: record.allocation_id.clone(),
        owner_epoch: record.new_owner.owner_epoch,
        operation_id: record.operation_id.clone(),
        journal_sequence: record.sequence,
        journal_record_hash: record.record_hash.clone(),
    }
}

fn validate_selector(
    allocation_id: &AllocationId,
    selector: &OwnerSelector,
    records: &[OwnerJournalRecord],
    allocation_root: &Path,
) -> PocResult<()> {
    if selector.schema_version != SCHEMA_VERSION || selector.allocation_id != *allocation_id {
        return Err(PocError::Integrity(format!(
            "owner selector identity mismatch for allocation {allocation_id}"
        )));
    }
    let record = records
        .iter()
        .find(|record| record.sequence == selector.journal_sequence)
        .ok_or_else(|| {
            PocError::RecoveryRequired(format!(
                "owner selector points past durable journal for allocation {allocation_id}"
            ))
        })?;
    if record.record_hash != selector.journal_record_hash
        || record.operation_id != selector.operation_id
        || record.new_owner.owner_epoch != selector.owner_epoch
        || !matches!(
            record.terminal_outcome,
            JournalTerminalOutcome::WorkspaceOwned | JournalTerminalOutcome::PayloadOwned
        )
    {
        return Err(PocError::Integrity(format!(
            "owner selector does not match committed journal record for allocation {allocation_id}"
        )));
    }
    let generation = load_generation(allocation_root, selector.owner_epoch)?;
    if generation != record.new_owner {
        return Err(PocError::Integrity(format!(
            "owner generation does not match selector for allocation {allocation_id}"
        )));
    }
    Ok(())
}

fn install_generation(allocation_root: &Path, generation: &OwnerGeneration) -> PocResult<()> {
    write_immutable_json(
        &generation_path(allocation_root, generation.owner_epoch),
        generation,
    )
}

fn load_generation(allocation_root: &Path, owner_epoch: u64) -> PocResult<OwnerGeneration> {
    read_json(&generation_path(allocation_root, owner_epoch))
}

fn read_selector(allocation_root: &Path) -> PocResult<Option<OwnerSelector>> {
    let path = selector_path(allocation_root);
    match read_json(&path) {
        Ok(selector) => Ok(Some(selector)),
        Err(PocError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn truncate_journal(path: &Path, valid_bytes: u64) -> PocResult<()> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| PocError::io("open torn owner journal", path, source))?;
    file.set_len(valid_bytes)
        .map_err(|source| PocError::io("truncate torn owner journal", path, source))?;
    file.sync_data()
        .map_err(|source| PocError::io("fdatasync repaired owner journal", path, source))
}

fn allocation_id_at_root(allocation_root: &Path) -> PocResult<AllocationId> {
    let descriptor: AllocationDescriptor = read_json(&allocation_root.join("ALLOCATION.json"))?;
    if descriptor.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(
            "allocation descriptor schema mismatch".to_owned(),
        ));
    }
    Ok(descriptor.allocation_id)
}

fn validate_path_component(value: &str, label: &str) -> PocResult<()> {
    let bytes = value.as_bytes();
    let valid = (1..=128).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(PocError::Integrity(format!(
            "{label} is not a safe path component"
        )))
    }
}

fn owner_dir(allocation_root: &Path) -> PathBuf {
    allocation_root.join("owner")
}

fn journal_path(allocation_root: &Path) -> PathBuf {
    owner_dir(allocation_root).join("journal.bin")
}

fn selector_path(allocation_root: &Path) -> PathBuf {
    owner_dir(allocation_root).join("CURRENT")
}

fn generation_path(allocation_root: &Path, owner_epoch: u64) -> PathBuf {
    owner_dir(allocation_root)
        .join("generations")
        .join(format!("{owner_epoch}.json"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;

        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut checksum = u32::MAX;
    for byte in bytes {
        checksum ^= u32::from(*byte);
        for _ in 0..8 {
            checksum = (checksum >> 1) ^ (0x82f6_3b78 & (0_u32.wrapping_sub(checksum & 1)));
        }
    }
    !checksum
}
