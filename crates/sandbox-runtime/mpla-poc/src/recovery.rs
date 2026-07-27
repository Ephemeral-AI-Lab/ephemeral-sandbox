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
const REAL_OPERATION_WITNESS_FORMAT: &str = "mpla-poc-real-operation-witness-v1";
const PHYSICAL_POINT_ENV: &str = "MPLA_POC_PHYSICAL_FAULT_POINT";
const PHYSICAL_ARMED_PATH_ENV: &str = "MPLA_POC_PHYSICAL_FAULT_ARMED_PATH";

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
pub enum DurableOperationKind {
    CommandFence,
    SealingRecord,
    HolderQuiescence,
    StrictUnmount,
    AllocationFlush,
    StableInventory,
    OwnerIntent,
    OwnerCompare,
    OwnerGeneration,
    OwnerJournal,
    OwnerSelector,
    OwnerReceipt,
    CanonicalObjectInstall,
    CanonicalRootManifest,
    LocatorGeneration,
    LocatorSelector,
    PairedRefCommit,
    PublishResponse,
    ActivateResponse,
    RollbackResponse,
    RefSelection,
    LocatorPin,
    FreshWorkspaceOwner,
    SessionMount,
    ReadinessProbe,
    ActivationBinding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultOperationBinding {
    pub fault_point: NamedFaultPoint,
    pub protocol_phase: CrashProtocolPhase,
    pub operation: DurableOperationKind,
    pub durable_boundary: &'static str,
}

macro_rules! operation_bindings {
    ($(($point:ident, $phase:ident, $operation:ident, $boundary:literal)),+ $(,)?) => {
        pub const HV07_OPERATION_BINDINGS: &[FaultOperationBinding] = &[
            $(FaultOperationBinding {
                fault_point: NamedFaultPoint::$point,
                protocol_phase: CrashProtocolPhase::$phase,
                operation: DurableOperationKind::$operation,
                durable_boundary: $boundary,
            }),+
        ];
    };
}

operation_bindings! {
    (FenceBeforeClose, CommandFencing, CommandFence, "before_close"),
    (FenceAfterClose, CommandFencing, CommandFence, "closing_record_parent_fsynced"),
    (FenceAfterDrain, CommandFencing, CommandFence, "in_flight_commands_drained"),
    (SealingBeforeWrite, DurableSealing, SealingRecord, "before_sealing_record_write"),
    (SealingAfterFileFsync, DurableSealing, SealingRecord, "sealing_temporary_file_fsynced"),
    (SealingAfterDirFsync, DurableSealing, SealingRecord, "sealing_record_parent_fsynced"),
    (QuiesceBeforeStop, HolderQuiescence, HolderQuiescence, "before_stop_kill_reap"),
    (QuiesceAfterReap, HolderQuiescence, HolderQuiescence, "process_tree_reaped"),
    (QuiesceAfterFdAudit, HolderQuiescence, HolderQuiescence, "writable_fd_audit_clear"),
    (UnmountBeforeStrict, StrictUnmount, StrictUnmount, "before_strict_unmount"),
    (UnmountAfterStrict, StrictUnmount, StrictUnmount, "strict_unmount_complete"),
    (FlushBeforeSyncfs, AllocationFlush, AllocationFlush, "before_allocation_syncfs"),
    (FlushAfterSyncfs, AllocationFlush, AllocationFlush, "allocation_syncfs_complete"),
    (InventoryAfterFirst, StableInventory, StableInventory, "first_inventory_complete"),
    (InventoryAfterStableSecond, StableInventory, StableInventory, "stable_second_inventory_complete"),
    (OwnerBeforeIntent, OwnershipTransition, OwnerIntent, "before_owner_intent_append"),
    (OwnerAfterIntentFsync, OwnershipTransition, OwnerIntent, "owner_intent_journal_fsynced"),
    (OwnerBeforeCompare, OwnershipTransition, OwnerCompare, "before_owner_compare"),
    (OwnerAfterGenerationFsync, OwnershipTransition, OwnerGeneration, "owner_generation_fsynced"),
    (OwnerAfterJournalCommit, OwnershipTransition, OwnerJournal, "owner_commit_journal_fsynced"),
    (OwnerAfterSelectorRename, OwnershipTransition, OwnerSelector, "owner_selector_replaced"),
    (OwnerAfterSelectorDirFsync, OwnershipTransition, OwnerSelector, "owner_selector_parent_fsynced"),
    (OwnerBeforeReceipt, OwnershipTransition, OwnerReceipt, "before_owner_receipt_install"),
    (OwnerAfterReceiptDirFsync, OwnershipTransition, OwnerReceipt, "owner_receipt_parent_fsynced"),
    (CanonicalBeforeInstall, CanonicalDurability, CanonicalObjectInstall, "before_canonical_object_install"),
    (CanonicalAfterObjectFsync, CanonicalDurability, CanonicalObjectInstall, "canonical_objects_fsynced"),
    (CanonicalAfterObjectDirFsync, CanonicalDurability, CanonicalObjectInstall, "canonical_object_directory_fsynced"),
    (CanonicalAfterRootManifestFsync, CanonicalDurability, CanonicalRootManifest, "root_manifest_parent_fsynced"),
    (LocatorAfterForward, LocatorSelection, LocatorGeneration, "locator_forward_durable"),
    (LocatorAfterReverse, LocatorSelection, LocatorGeneration, "locator_reverse_durable"),
    (LocatorAfterManifestFsync, LocatorSelection, LocatorGeneration, "locator_manifest_parent_fsynced"),
    (LocatorAfterSelectorRename, LocatorSelection, LocatorSelector, "locator_selector_replaced"),
    (LocatorAfterSelectorDirFsync, LocatorSelection, LocatorSelector, "locator_selector_parent_fsynced"),
    (RefBeforeTemp, RefReplacement, PairedRefCommit, "before_ref_temporary"),
    (RefAfterTempFsync, RefReplacement, PairedRefCommit, "ref_temporary_fsynced"),
    (RefAfterReplace, RefReplacement, PairedRefCommit, "paired_ref_replaced"),
    (RefAfterParentFsync, RefReplacement, PairedRefCommit, "paired_ref_parent_fsynced"),
    (ResponseLossPublish, ResponseDelivery, PublishResponse, "publish_terminal_durable_before_response"),
    (ResponseLossActivate, ResponseDelivery, ActivateResponse, "activation_terminal_durable_before_response"),
    (ResponseLossRollback, ResponseDelivery, RollbackResponse, "rollback_terminal_durable_before_response"),
    (ActivateAfterRefSelect, SuccessorActivation, RefSelection, "paired_ref_selected"),
    (ActivateAfterLocatorPin, SuccessorActivation, LocatorPin, "locator_generation_pinned"),
    (ActivateAfterFreshOwner, SuccessorActivation, FreshWorkspaceOwner, "fresh_workspace_owner_durable"),
    (ActivateAfterMount, SuccessorActivation, SessionMount, "successor_mount_complete"),
    (ActivateAfterReady, SuccessorActivation, ReadinessProbe, "external_readiness_succeeded"),
    (ActivateAfterBindingFsync, SuccessorActivation, ActivationBinding, "activation_binding_parent_fsynced"),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RealOperationWitness {
    pub schema_version: u32,
    pub format: String,
    pub fault_point: NamedFaultPoint,
    pub protocol_phase: CrashProtocolPhase,
    pub operation: DurableOperationKind,
    pub durable_boundary: String,
    pub operation_id: OperationId,
    pub durable_state_paths: Vec<PathBuf>,
    pub operation_state_parent_synced: bool,
    pub stationary_payload_path_before: Option<PathBuf>,
    pub stationary_payload_path_after: Option<PathBuf>,
    pub payload_bytes_moved: u64,
    pub payload_bytes_copied: u64,
    pub recorded_unix_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PhysicalKillWitness {
    pub schema_version: u32,
    pub fault_point: NamedFaultPoint,
    pub operation_id: OperationId,
    pub process_id: u32,
    pub signal: i32,
    pub durable_marker_observed: bool,
    pub marker_parent_synced: bool,
    pub terminated_by_expected_signal: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryReplayWitness {
    pub schema_version: u32,
    pub fault_point: NamedFaultPoint,
    pub operation_id: OperationId,
    pub retry_operation_id: OperationId,
    pub recovery_invoked: bool,
    pub recovery_completed: bool,
    pub terminal_invariant_verified: bool,
    pub selected_visibility: SelectedVisibility,
    pub exact_owner_verified: bool,
    pub exact_locator_verified: bool,
    pub exact_ref_verified: bool,
    pub stationary_payload_verified: bool,
    pub failed_attempt_bundle_durable: bool,
    pub cancelled_attempt_bundle_durable: bool,
    pub idempotent_retry_verified: bool,
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
    pub real_operation_witness: Option<RealOperationWitness>,
    pub physical_kill_witness: Option<PhysicalKillWitness>,
    pub recovery_replay_witness: Option<RecoveryReplayWitness>,
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

#[must_use]
pub fn hv07_operation_bindings() -> &'static [FaultOperationBinding] {
    HV07_OPERATION_BINDINGS
}

pub fn reach_real_operation(
    faults: &mut NamedFaultInjector,
    point: NamedFaultPoint,
    operation_id: &OperationId,
    durable_state_paths: impl IntoIterator<Item = PathBuf>,
    stationary_payload_path: Option<&Path>,
    post_sealing: bool,
) -> PocResult<()> {
    let binding = operation_binding(point)?;
    let durable_state_paths = durable_state_paths.into_iter().collect::<Vec<_>>();
    if std::env::var_os(PHYSICAL_POINT_ENV).as_deref() == Some(std::ffi::OsStr::new(point.as_str()))
    {
        if durable_state_paths.is_empty() || durable_state_paths.iter().any(|path| !path.exists()) {
            return Err(PocError::Integrity(format!(
                "real operation {} reached {} without its durable state",
                binding.durable_boundary,
                point.as_str()
            )));
        }
        if stationary_payload_path.is_some_and(|path| !path.exists()) {
            return Err(PocError::Integrity(format!(
                "real operation {} lost its stationary payload before {}",
                binding.durable_boundary,
                point.as_str()
            )));
        }
        let operation_state_parent_synced = sync_operation_state_parents(&durable_state_paths)?;
        let marker_path = std::env::var_os(PHYSICAL_ARMED_PATH_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| {
                PocError::InvalidConfig(
                    "physical real-operation witness has no armed marker path".to_owned(),
                )
            })?;
        let witness_path = marker_path.with_file_name("real-operation.json");
        let stationary = stationary_payload_path.map(Path::to_path_buf);
        replace_json(
            &witness_path,
            &RealOperationWitness {
                schema_version: SCHEMA_VERSION,
                format: REAL_OPERATION_WITNESS_FORMAT.to_owned(),
                fault_point: point,
                protocol_phase: binding.protocol_phase,
                operation: binding.operation,
                durable_boundary: binding.durable_boundary.to_owned(),
                operation_id: operation_id.clone(),
                durable_state_paths,
                operation_state_parent_synced,
                stationary_payload_path_before: stationary.clone(),
                stationary_payload_path_after: stationary,
                payload_bytes_moved: 0,
                payload_bytes_copied: 0,
                recorded_unix_ms: unix_time_ms()?,
            },
        )?;
    }
    faults.reach(point, 1, post_sealing)
}

fn sync_operation_state_parents(paths: &[PathBuf]) -> PocResult<bool> {
    let mut parents = std::collections::BTreeSet::new();
    for path in paths {
        let parent = path.parent().ok_or_else(|| {
            PocError::Integrity(format!(
                "real-operation durable state has no parent: {}",
                path.display()
            ))
        })?;
        parents.insert(parent.to_path_buf());
    }
    for parent in parents {
        std::fs::File::open(&parent)
            .map_err(|error| PocError::io("open real-operation state parent", &parent, error))?
            .sync_all()
            .map_err(|error| PocError::io("fsync real-operation state parent", &parent, error))?;
    }
    Ok(true)
}

fn operation_binding(point: NamedFaultPoint) -> PocResult<FaultOperationBinding> {
    HV07_OPERATION_BINDINGS
        .iter()
        .copied()
        .find(|binding| binding.fault_point == point)
        .ok_or_else(|| {
            PocError::Integrity(format!(
                "faultpoint {} has no real-operation binding",
                point.as_str()
            ))
        })
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
        let path = fault_dir.join(format!("{:08}.json", observation.attempt));
        if path.exists() {
            let existing: CrashAttemptRecord = read_json(&path)?;
            validate_crash_record(&existing)?;
            if existing.observation == observation {
                return Ok(existing);
            }
            return Err(PocError::Integrity(format!(
                "crash attempt {} for {} was reused with different evidence",
                observation.attempt,
                observation.fault_point.as_str()
            )));
        }
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
    match observation.real_operation_witness.as_ref() {
        Some(witness) => match operation_binding(observation.fault_point) {
            Ok(binding) => {
                if witness.schema_version != SCHEMA_VERSION
                    || witness.format != REAL_OPERATION_WITNESS_FORMAT
                    || witness.fault_point != observation.fault_point
                    || witness.protocol_phase != binding.protocol_phase
                    || witness.operation != binding.operation
                    || witness.durable_boundary != binding.durable_boundary
                    || witness.operation_id != observation.operation_id
                    || witness.durable_state_paths.is_empty()
                    || !witness.operation_state_parent_synced
                {
                    failures.push(
                        "real-operation witness does not match the exact durable boundary"
                            .to_owned(),
                    );
                }
                if witness.stationary_payload_path_before != witness.stationary_payload_path_after
                    || witness.payload_bytes_moved != 0
                    || witness.payload_bytes_copied != 0
                {
                    failures.push(
                        "real operation moved or copied the stationary publication payload"
                            .to_owned(),
                    );
                }
            }
            Err(_) => failures.push("faultpoint has no real-operation binding".to_owned()),
        },
        None => failures.push("crash attempt lacks a real-operation witness".to_owned()),
    }
    if observation.execution_mode.is_physical() {
        match observation.physical_kill_witness.as_ref() {
            Some(witness)
                if witness.schema_version == SCHEMA_VERSION
                    && witness.fault_point == observation.fault_point
                    && witness.operation_id == observation.operation_id
                    && witness.process_id != 0
                    && witness.signal == libc::SIGKILL
                    && witness.durable_marker_observed
                    && witness.marker_parent_synced
                    && witness.terminated_by_expected_signal => {}
            Some(_) => {
                failures.push("physical kill witness is incomplete or mismatched".to_owned())
            }
            None => failures.push("physical crash attempt lacks a kill witness".to_owned()),
        }
    }
    match observation.recovery_replay_witness.as_ref() {
        Some(witness)
            if witness.schema_version == SCHEMA_VERSION
                && witness.fault_point == observation.fault_point
                && witness.operation_id == observation.operation_id
                && witness.retry_operation_id == observation.retry_operation_id
                && witness.recovery_invoked
                && witness.recovery_completed
                && witness.terminal_invariant_verified
                && witness.selected_visibility == observation.selected_visibility
                && witness.exact_owner_verified
                && witness.exact_locator_verified
                && witness.exact_ref_verified
                && witness.stationary_payload_verified
                && witness.failed_attempt_bundle_durable
                && witness.cancelled_attempt_bundle_durable
                && witness.idempotent_retry_verified => {}
        Some(_) => failures.push("recovery replay witness is incomplete or mismatched".to_owned()),
        None => failures.push("crash attempt lacks a recovery replay witness".to_owned()),
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
