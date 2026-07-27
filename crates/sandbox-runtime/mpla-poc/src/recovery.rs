use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::durable::{fsync_dir, read_json, replace_json, write_immutable_json, FileLock};
use crate::locator::{LocatorDelta, LocatorStore};
use crate::occ::{
    BranchOcc, ChangedPathSet, ConflictAllocation, OccPublication, OccPublishOutcome,
    RebasedCanonical, RetainedOverlapConflict,
};
use crate::owner::current_owner;
use crate::ref_store::{PairedRefStore, RefCommitReceipt};
use crate::{
    unix_time_ms, AllocationId, CanonicalDurabilityReceipt, LocatorGeneration, LocatorRefCandidate,
    NamedFaultInjector, NamedFaultPoint, OperationId, OwnerSubject, PairedRefValue, PocError,
    PocResult, PublicationId, RefSequence, SCHEMA_VERSION,
};

const RECOVERY_FORMAT: &str = "mpla-poc-recovery-v1";
const CRASH_SWEEP_FORMAT: &str = "mpla-poc-crash-sweep-v1";

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashExecutionMode {
    HostInjection,
    ProcessSigkill,
    ContainerKill,
}

impl CrashExecutionMode {
    #[must_use]
    pub const fn is_physical(self) -> bool {
        matches!(self, Self::ProcessSigkill | Self::ContainerKill)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashProtocolPhase {
    CommandFencing,
    DurableSealing,
    HolderQuiescence,
    StrictUnmount,
    AllocationFlush,
    StableInventory,
    OwnershipTransition,
    CanonicalDurability,
    LocatorSelection,
    RefReplacement,
    ResponseDelivery,
    SuccessorActivation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectedVisibility {
    Old,
    CompleteNew,
    PartialNew,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultRecoveryExpectation {
    pub fault_point: NamedFaultPoint,
    pub protocol_phase: CrashProtocolPhase,
    pub durable_sealing_required: bool,
    pub terminal_session_required: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DurableCrashWitness {
    pub schema_version: u32,
    pub protocol_phase: CrashProtocolPhase,
    pub recovery_phase: Option<DurableRecoveryPhase>,
    pub owner_count: u32,
    pub owner_allocation_id: Option<AllocationId>,
    pub owner_epoch: Option<u64>,
    pub locator_generation: Option<LocatorGeneration>,
    pub ref_sequence: Option<RefSequence>,
    pub session_terminal: bool,
    pub state_parent_synced: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CrashRecoveryObservation {
    pub schema_version: u32,
    pub fault_point: NamedFaultPoint,
    pub attempt: u32,
    pub execution_mode: CrashExecutionMode,
    pub operation_id: OperationId,
    pub retry_operation_id: OperationId,
    pub before: DurableCrashWitness,
    pub after: DurableCrashWitness,
    pub selected_visibility: SelectedVisibility,
    pub idempotent_retry_same_result: bool,
    pub post_sealing_session_resumed: bool,
    pub failed_span_retained: bool,
    pub cancelled_span_retained: bool,
    pub observed_debt_bytes: u64,
    pub temporary_debt_bytes: u64,
    pub retirement_debt_bytes: u64,
    pub unclassified_debt_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CrashAttemptRecord {
    pub schema_version: u32,
    pub format: String,
    pub recorded_unix_ms: u64,
    pub observation: CrashRecoveryObservation,
    pub passed: bool,
    pub failures: Vec<String>,
    pub record_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CrashSweepSummary {
    pub schema_version: u32,
    pub required_fault_points: u64,
    pub recorded_attempts: u64,
    pub passing_fault_points: u64,
    pub physical_passing_fault_points: u64,
    pub failed_attempts: u64,
    pub missing_fault_points: Vec<NamedFaultPoint>,
    pub physical_missing_fault_points: Vec<NamedFaultPoint>,
    pub complete_for_requested_mode: bool,
}

#[derive(Clone, Debug)]
pub struct CrashSweepLedger {
    root: PathBuf,
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

impl CrashSweepLedger {
    pub fn open(root: impl Into<PathBuf>) -> PocResult<Self> {
        let ledger = Self { root: root.into() };
        std::fs::create_dir_all(ledger.root.join("attempts")).map_err(|source| {
            PocError::io(
                "create crash sweep attempts directory",
                ledger.root.join("attempts"),
                source,
            )
        })?;
        fsync_dir(&ledger.root)?;
        Ok(ledger)
    }

    pub fn record(&self, observation: CrashRecoveryObservation) -> PocResult<CrashAttemptRecord> {
        let failures = crash_observation_failures(&observation);
        let fault_dir = self
            .root
            .join("attempts")
            .join(observation.fault_point.as_str());
        std::fs::create_dir_all(&fault_dir).map_err(|source| {
            PocError::io("create crash faultpoint directory", &fault_dir, source)
        })?;
        fsync_dir(&self.root.join("attempts"))?;
        let mut record = CrashAttemptRecord {
            schema_version: SCHEMA_VERSION,
            format: CRASH_SWEEP_FORMAT.to_owned(),
            recorded_unix_ms: unix_time_ms()?,
            observation,
            passed: failures.is_empty(),
            failures,
            record_sha256: String::new(),
        };
        record.record_sha256 = crash_record_digest(&record)?;
        let path = fault_dir.join(format!("{:08}.json", record.observation.attempt));
        write_immutable_json(&path, &record)?;
        Ok(record)
    }

    pub fn summary(&self, require_physical: bool) -> PocResult<CrashSweepSummary> {
        let mut passing = BTreeSet::new();
        let mut physical_passing = BTreeSet::new();
        let mut recorded_attempts = 0_u64;
        let mut failed_attempts = 0_u64;
        for point in NamedFaultPoint::ALL {
            let fault_dir = self.root.join("attempts").join(point.as_str());
            let Ok(entries) = std::fs::read_dir(&fault_dir) else {
                continue;
            };
            for entry in entries {
                let entry = entry.map_err(|source| {
                    PocError::io("read crash attempt directory entry", &fault_dir, source)
                })?;
                if !entry
                    .file_type()
                    .map_err(|source| {
                        PocError::io("stat crash attempt directory entry", entry.path(), source)
                    })?
                    .is_file()
                {
                    continue;
                }
                let record: CrashAttemptRecord = read_json(&entry.path())?;
                validate_crash_record(&record)?;
                if record.observation.fault_point != *point {
                    return Err(PocError::RecoveryRequired(format!(
                        "crash attempt under {} records {}",
                        point.as_str(),
                        record.observation.fault_point.as_str()
                    )));
                }
                recorded_attempts = recorded_attempts.checked_add(1).ok_or_else(|| {
                    PocError::Integrity("crash attempt count overflow".to_owned())
                })?;
                if record.passed {
                    passing.insert(*point);
                    if record.observation.execution_mode.is_physical() {
                        physical_passing.insert(*point);
                    }
                } else {
                    failed_attempts = failed_attempts.checked_add(1).ok_or_else(|| {
                        PocError::Integrity("failed crash attempt count overflow".to_owned())
                    })?;
                }
            }
        }
        let missing_fault_points = NamedFaultPoint::ALL
            .iter()
            .copied()
            .filter(|point| !passing.contains(point))
            .collect::<Vec<_>>();
        let physical_missing_fault_points = NamedFaultPoint::ALL
            .iter()
            .copied()
            .filter(|point| !physical_passing.contains(point))
            .collect::<Vec<_>>();
        let complete_for_requested_mode = if require_physical {
            physical_missing_fault_points.is_empty()
        } else {
            missing_fault_points.is_empty()
        };
        Ok(CrashSweepSummary {
            schema_version: SCHEMA_VERSION,
            required_fault_points: usize_to_u64(NamedFaultPoint::ALL.len())?,
            recorded_attempts,
            passing_fault_points: usize_to_u64(passing.len())?,
            physical_passing_fault_points: usize_to_u64(physical_passing.len())?,
            failed_attempts,
            missing_fault_points,
            physical_missing_fault_points,
            complete_for_requested_mode,
        })
    }

    pub fn verify_complete(&self, require_physical: bool) -> PocResult<CrashSweepSummary> {
        let summary = self.summary(require_physical)?;
        if !summary.complete_for_requested_mode {
            let missing = if require_physical {
                &summary.physical_missing_fault_points
            } else {
                &summary.missing_fault_points
            };
            return Err(PocError::RecoveryRequired(format!(
                "crash sweep is missing {} passing {} faultpoints",
                missing.len(),
                if require_physical {
                    "physical"
                } else {
                    "developmental"
                }
            )));
        }
        Ok(summary)
    }
}

#[must_use]
pub fn hv07_fault_expectations() -> Vec<FaultRecoveryExpectation> {
    NamedFaultPoint::ALL
        .iter()
        .copied()
        .map(|fault_point| {
            let protocol_phase = crash_protocol_phase(fault_point);
            let durable_sealing_required = !matches!(
                fault_point,
                NamedFaultPoint::FenceBeforeClose
                    | NamedFaultPoint::FenceAfterClose
                    | NamedFaultPoint::FenceAfterDrain
                    | NamedFaultPoint::SealingBeforeWrite
                    | NamedFaultPoint::SealingAfterFileFsync
            );
            FaultRecoveryExpectation {
                fault_point,
                protocol_phase,
                durable_sealing_required,
                terminal_session_required: durable_sealing_required,
            }
        })
        .collect()
}

fn crash_observation_failures(observation: &CrashRecoveryObservation) -> Vec<String> {
    let mut failures = Vec::new();
    if observation.schema_version != SCHEMA_VERSION
        || observation.before.schema_version != SCHEMA_VERSION
        || observation.after.schema_version != SCHEMA_VERSION
    {
        failures.push("unsupported crash observation schema".to_owned());
    }
    if observation.attempt == 0 {
        failures.push("crash attempt must be non-zero".to_owned());
    }
    let expectation = hv07_fault_expectations()
        .into_iter()
        .find(|expectation| expectation.fault_point == observation.fault_point);
    match expectation {
        Some(expectation) => {
            if observation.before.protocol_phase != expectation.protocol_phase {
                failures.push("durable before witness is assigned to the wrong phase".to_owned());
            }
            if expectation.terminal_session_required
                && (!observation.after.session_terminal || observation.post_sealing_session_resumed)
            {
                failures.push("post-Sealing session resumed or was not terminal".to_owned());
            }
        }
        None => failures.push("faultpoint is absent from the frozen registry".to_owned()),
    }
    if !observation.before.state_parent_synced || !observation.after.state_parent_synced {
        failures.push("durable before/after witness lacks parent fsync".to_owned());
    }
    if observation.after.owner_count != 1
        || observation.after.owner_allocation_id.is_none()
        || observation.after.owner_epoch.is_none_or(|epoch| epoch == 0)
    {
        failures.push("recovery did not select exactly one durable owner".to_owned());
    }
    if observation.selected_visibility == SelectedVisibility::PartialNew {
        failures.push("recovery exposed a partially new selection".to_owned());
    }
    if observation.operation_id != observation.retry_operation_id
        || !observation.idempotent_retry_same_result
    {
        failures.push("retry did not preserve the operation ID and result".to_owned());
    }
    if !observation.failed_span_retained || !observation.cancelled_span_retained {
        failures.push("failed and cancelled spans were not both retained".to_owned());
    }
    let classified = observation
        .temporary_debt_bytes
        .checked_add(observation.retirement_debt_bytes);
    if classified != Some(observation.observed_debt_bytes)
        || observation.unclassified_debt_bytes != 0
    {
        failures.push("temporary or retirement debt is not completely classified".to_owned());
    }
    failures
}

fn validate_crash_record(record: &CrashAttemptRecord) -> PocResult<()> {
    if record.schema_version != SCHEMA_VERSION || record.format != CRASH_SWEEP_FORMAT {
        return Err(PocError::Integrity(
            "unsupported crash sweep record".to_owned(),
        ));
    }
    if crash_record_digest(record)? != record.record_sha256 {
        return Err(PocError::RecoveryRequired(
            "crash sweep record checksum mismatch".to_owned(),
        ));
    }
    let expected_failures = crash_observation_failures(&record.observation);
    if expected_failures != record.failures || record.passed != expected_failures.is_empty() {
        return Err(PocError::RecoveryRequired(
            "crash sweep verdict disagrees with its durable observation".to_owned(),
        ));
    }
    Ok(())
}

fn crash_record_digest(record: &CrashAttemptRecord) -> PocResult<String> {
    let mut expected = record.clone();
    expected.record_sha256.clear();
    digest_json(&expected)
}

const fn crash_protocol_phase(point: NamedFaultPoint) -> CrashProtocolPhase {
    match point {
        NamedFaultPoint::FenceBeforeClose
        | NamedFaultPoint::FenceAfterClose
        | NamedFaultPoint::FenceAfterDrain => CrashProtocolPhase::CommandFencing,
        NamedFaultPoint::SealingBeforeWrite
        | NamedFaultPoint::SealingAfterFileFsync
        | NamedFaultPoint::SealingAfterDirFsync => CrashProtocolPhase::DurableSealing,
        NamedFaultPoint::QuiesceBeforeStop
        | NamedFaultPoint::QuiesceAfterReap
        | NamedFaultPoint::QuiesceAfterFdAudit => CrashProtocolPhase::HolderQuiescence,
        NamedFaultPoint::UnmountBeforeStrict | NamedFaultPoint::UnmountAfterStrict => {
            CrashProtocolPhase::StrictUnmount
        }
        NamedFaultPoint::FlushBeforeSyncfs | NamedFaultPoint::FlushAfterSyncfs => {
            CrashProtocolPhase::AllocationFlush
        }
        NamedFaultPoint::InventoryAfterFirst | NamedFaultPoint::InventoryAfterStableSecond => {
            CrashProtocolPhase::StableInventory
        }
        NamedFaultPoint::OwnerBeforeIntent
        | NamedFaultPoint::OwnerAfterIntentFsync
        | NamedFaultPoint::OwnerBeforeCompare
        | NamedFaultPoint::OwnerAfterGenerationFsync
        | NamedFaultPoint::OwnerAfterJournalCommit
        | NamedFaultPoint::OwnerAfterSelectorRename
        | NamedFaultPoint::OwnerAfterSelectorDirFsync
        | NamedFaultPoint::OwnerBeforeReceipt
        | NamedFaultPoint::OwnerAfterReceiptDirFsync => CrashProtocolPhase::OwnershipTransition,
        NamedFaultPoint::CanonicalBeforeInstall
        | NamedFaultPoint::CanonicalAfterObjectFsync
        | NamedFaultPoint::CanonicalAfterObjectDirFsync
        | NamedFaultPoint::CanonicalAfterRootManifestFsync => {
            CrashProtocolPhase::CanonicalDurability
        }
        NamedFaultPoint::LocatorAfterForward
        | NamedFaultPoint::LocatorAfterReverse
        | NamedFaultPoint::LocatorAfterManifestFsync
        | NamedFaultPoint::LocatorAfterSelectorRename
        | NamedFaultPoint::LocatorAfterSelectorDirFsync => CrashProtocolPhase::LocatorSelection,
        NamedFaultPoint::RefBeforeTemp
        | NamedFaultPoint::RefAfterTempFsync
        | NamedFaultPoint::RefAfterReplace
        | NamedFaultPoint::RefAfterParentFsync => CrashProtocolPhase::RefReplacement,
        NamedFaultPoint::ResponseLossPublish
        | NamedFaultPoint::ResponseLossActivate
        | NamedFaultPoint::ResponseLossRollback => CrashProtocolPhase::ResponseDelivery,
        NamedFaultPoint::ActivateAfterRefSelect
        | NamedFaultPoint::ActivateAfterLocatorPin
        | NamedFaultPoint::ActivateAfterFreshOwner
        | NamedFaultPoint::ActivateAfterMount
        | NamedFaultPoint::ActivateAfterReady
        | NamedFaultPoint::ActivateAfterBindingFsync => CrashProtocolPhase::SuccessorActivation,
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

fn usize_to_u64(value: usize) -> PocResult<u64> {
    u64::try_from(value)
        .map_err(|_| PocError::Integrity("crash sweep count does not fit in u64".to_owned()))
}
