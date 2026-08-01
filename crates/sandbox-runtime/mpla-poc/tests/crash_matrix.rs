use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use sandbox_runtime_mpla_poc::allocation::create_allocation;
use sandbox_runtime_mpla_poc::lease::issue_workspace_lease;
use sandbox_runtime_mpla_poc::owner::{compare_and_adopt, current_owner};
use sandbox_runtime_mpla_poc::recovery::{
    hv07_fault_expectations, hv07_operation_bindings, CrashExecutionMode, CrashProtocolPhase,
    CrashRecoveryObservation, CrashSweepLedger, DurableCrashWitness, DurableOperationKind,
    RealOperationWitness, RecoveryReplayWitness, SelectedVisibility,
};
use sandbox_runtime_mpla_poc::{
    AllocationId, NamedFaultInjector, NamedFaultPoint, OperationId, OwnerGeneration, OwnerSubject,
    OwnerTransitionRequest, PhysicalSnapshot, PocError, PublicationId, StableAllocationReceipt,
    SCHEMA_VERSION,
};
use uuid::Uuid;

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mpla-m2-crash-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create crash test root");
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn physical_fault_context_preserves_the_exact_stationary_payload_path() {
    let expected = PathBuf::from("/arena/aa/allocation/upper");
    let faults =
        NamedFaultInjector::default().with_physical_stationary_payload_path(expected.clone());

    assert_eq!(
        faults.physical_stationary_payload_path(),
        Some(expected.as_path())
    );
}

#[test]
fn developmental_sweep_accounts_for_every_frozen_faultpoint() {
    let root = TestRoot::new("complete-registry");
    let ledger = CrashSweepLedger::open(&root.path).expect("open crash ledger");
    let expectations = hv07_fault_expectations();
    assert_eq!(expectations.len(), NamedFaultPoint::ALL.len());
    let bindings = hv07_operation_bindings();
    assert_eq!(bindings.len(), NamedFaultPoint::ALL.len());
    assert_eq!(
        bindings
            .iter()
            .map(|binding| binding.fault_point)
            .collect::<Vec<_>>(),
        NamedFaultPoint::ALL
    );
    let unique = bindings
        .iter()
        .map(|binding| binding.fault_point)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), NamedFaultPoint::ALL.len());
    assert_eq!(
        expectations
            .iter()
            .map(|expectation| expectation.fault_point)
            .collect::<Vec<_>>(),
        NamedFaultPoint::ALL
    );

    for expectation in expectations {
        let observation = passing_observation(
            expectation.fault_point,
            expectation.protocol_phase,
            expectation.terminal_session_required,
            1,
            CrashExecutionMode::HostInjection,
        );
        let record = ledger
            .record(observation.clone())
            .expect("record developmental crash attempt");
        assert!(record.passed, "{:?}", record.failures);
        let replay = ledger
            .record(observation)
            .expect("idempotently replay crash attempt bundle");
        assert_eq!(record, replay);
    }

    let summary = ledger
        .verify_complete(false)
        .expect("complete developmental sweep");
    assert_eq!(summary.required_fault_points, 46);
    assert_eq!(summary.recorded_attempts, 46);
    assert_eq!(summary.passing_fault_points, 46);
    assert_eq!(summary.physical_passing_fault_points, 0);
    assert_eq!(summary.failed_attempts, 0);
    assert!(summary.missing_fault_points.is_empty());
    assert_eq!(summary.physical_missing_fault_points.len(), 46);
    assert!(matches!(
        ledger.verify_complete(true),
        Err(PocError::RecoveryRequired(message))
            if message.contains("missing 46 passing physical faultpoints")
    ));
}

#[test]
fn every_mapping_is_owned_by_its_real_operation_source() {
    let session = include_str!("../src/session.rs");
    let quiesce = include_str!("../src/quiesce.rs");
    let owner = include_str!("../src/owner.rs");
    let semantic = include_str!("../src/semantic/mod.rs");
    let locator = include_str!("../src/locator.rs");
    let ref_store = include_str!("../src/ref_store.rs");
    let activation = include_str!("../src/activation.rs");

    for binding in hv07_operation_bindings() {
        let source = match binding.operation {
            DurableOperationKind::CommandFence => session,
            DurableOperationKind::SealingRecord
            | DurableOperationKind::HolderQuiescence
            | DurableOperationKind::StrictUnmount
            | DurableOperationKind::AllocationFlush
            | DurableOperationKind::StableInventory => quiesce,
            DurableOperationKind::OwnerIntent
            | DurableOperationKind::OwnerCompare
            | DurableOperationKind::OwnerGeneration
            | DurableOperationKind::OwnerJournal
            | DurableOperationKind::OwnerSelector
            | DurableOperationKind::OwnerReceipt => owner,
            DurableOperationKind::CanonicalObjectInstall
            | DurableOperationKind::CanonicalRootManifest => semantic,
            DurableOperationKind::LocatorGeneration | DurableOperationKind::LocatorSelector => {
                locator
            }
            DurableOperationKind::PairedRefCommit
            | DurableOperationKind::PublishResponse
            | DurableOperationKind::RollbackResponse => ref_store,
            DurableOperationKind::ActivateResponse
            | DurableOperationKind::RefSelection
            | DurableOperationKind::LocatorPin
            | DurableOperationKind::FreshWorkspaceOwner
            | DurableOperationKind::SessionMount
            | DurableOperationKind::ReadinessProbe
            | DurableOperationKind::ActivationBinding => activation,
        };
        let needle = format!("NamedFaultPoint::{:?}", binding.fault_point);
        assert!(
            source.contains(&needle),
            "{} is not wired from its real {:?} operation",
            binding.fault_point.as_str(),
            binding.operation
        );
    }

    let heavy_child = include_str!("cases/heavy_lead.rs");
    assert!(
        !heavy_child.contains("physical_reach("),
        "the physical child must not invoke a named marker directly"
    );
    assert!(
        owner
            .find("NamedFaultPoint::OwnerAfterIntentFsync")
            .expect("intent durability hook")
            < owner
                .find("NamedFaultPoint::OwnerBeforeCompare")
                .expect("conditional compare hook")
    );
    assert!(
        owner
            .find("NamedFaultPoint::OwnerAfterGenerationFsync")
            .expect("generation durability hook")
            < owner
                .find("NamedFaultPoint::OwnerAfterJournalCommit")
                .expect("journal commit hook")
    );
}

#[test]
fn marker_only_forged_and_missing_witnesses_cannot_pass() {
    let root = TestRoot::new("anti-forgery");
    let ledger = CrashSweepLedger::open(&root.path).expect("open crash ledger");
    let point = NamedFaultPoint::LocatorAfterManifestFsync;
    let phase = CrashProtocolPhase::LocatorSelection;

    let mut marker_only =
        passing_observation(point, phase, true, 1, CrashExecutionMode::HostInjection);
    marker_only.real_operation_witness = None;
    assert!(
        !ledger
            .record(marker_only)
            .expect("marker-only attempt")
            .passed
    );

    let mut forged = passing_observation(point, phase, true, 2, CrashExecutionMode::HostInjection);
    forged
        .real_operation_witness
        .as_mut()
        .expect("real operation witness")
        .durable_boundary = "marker_only".to_owned();
    assert!(!ledger.record(forged).expect("forged attempt").passed);

    let mut missing_recovery =
        passing_observation(point, phase, true, 3, CrashExecutionMode::HostInjection);
    missing_recovery.recovery_replay_witness = None;
    assert!(
        !ledger
            .record(missing_recovery)
            .expect("missing recovery attempt")
            .passed
    );

    let physical = passing_observation(point, phase, true, 4, CrashExecutionMode::ProcessSigkill);
    let record = ledger.record(physical).expect("missing kill attempt");
    assert!(!record.passed);
    assert!(record
        .failures
        .iter()
        .any(|failure| failure.contains("kill witness")));

    let mut copied_payload =
        passing_observation(point, phase, true, 5, CrashExecutionMode::HostInjection);
    copied_payload
        .real_operation_witness
        .as_mut()
        .expect("real operation witness")
        .payload_bytes_copied = 1;
    let record = ledger
        .record(copied_payload)
        .expect("copied-payload attempt");
    assert!(!record.passed);
    assert!(record
        .failures
        .iter()
        .any(|failure| failure.contains("moved or copied")));

    let mut missing_stationary_payload =
        passing_observation(point, phase, true, 6, CrashExecutionMode::HostInjection);
    let witness = missing_stationary_payload
        .real_operation_witness
        .as_mut()
        .expect("real operation witness");
    witness.stationary_payload_path_before = None;
    witness.stationary_payload_path_after = None;
    let record = ledger
        .record(missing_stationary_payload)
        .expect("missing stationary-payload attempt");
    assert!(!record.passed);
    assert!(record
        .failures
        .iter()
        .any(|failure| failure.contains("moved or copied")));
}

#[test]
fn generation_edge_retry_reuses_one_exact_durable_owner() {
    let root = TestRoot::new("owner-generation-retry");
    let allocation =
        create_allocation(&root.path.join("arena"), &OperationId::new()).expect("allocation");
    let lease = issue_workspace_lease(
        &allocation,
        sandbox_runtime_mpla_poc::SessionId::new(),
        &OperationId::new(),
    )
    .expect("workspace lease");
    let operation_id = OperationId::from_string("owner-generation-retry");
    let publication_id = PublicationId::from_string("owner-generation-retry");
    let owner_epoch = lease.owner_epoch.checked_add(1).expect("next owner epoch");
    let generation = OwnerGeneration {
        schema_version: SCHEMA_VERSION,
        allocation_id: allocation.descriptor.allocation_id.clone(),
        owner_epoch,
        previous_owner_epoch: Some(lease.owner_epoch),
        subject: OwnerSubject::PayloadOwned {
            publication_id: publication_id.clone(),
        },
        operation_id: operation_id.clone(),
        written_unix_ms: 1,
    };
    let generation_path = allocation
        .owner_dir
        .join("generations")
        .join(format!("{owner_epoch}.json"));
    let mut generation_bytes = serde_json::to_vec(&generation).expect("encode owner generation");
    generation_bytes.push(b'\n');
    let mut generation_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&generation_path)
        .expect("create durable owner generation");
    generation_file
        .write_all(&generation_bytes)
        .expect("write durable owner generation");
    generation_file
        .sync_all()
        .expect("fsync durable owner generation");
    drop(generation_file);
    File::open(generation_path.parent().expect("generation directory"))
        .expect("open generation directory")
        .sync_all()
        .expect("fsync generation directory");

    let metadata = allocation
        .allocation_root
        .metadata()
        .expect("allocation metadata");
    let snapshot = PhysicalSnapshot {
        allocation_id: allocation.descriptor.allocation_id.clone(),
        allocation_path: allocation.allocation_root.clone(),
        device: metadata.dev(),
        representative_inodes: Vec::new(),
        logical_bytes: 0,
        allocated_bytes: 0,
        inode_count: 0,
        file_count: 0,
        directory_count: 0,
    };
    let stable = StableAllocationReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        allocation: allocation.descriptor.clone(),
        expected_owner_epoch: lease.owner_epoch,
        before: snapshot.clone(),
        after: snapshot,
        sync_completed: true,
    };
    let request = OwnerTransitionRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id,
        session_id: lease.session_id,
        allocation_id: allocation.descriptor.allocation_id.clone(),
        expected_lease_epoch: lease.lease_epoch,
        expected_owner_epoch: lease.owner_epoch,
    };
    let first = compare_and_adopt(&allocation.allocation_root, &stable, &request)
        .expect("resume after generation durability");
    let replay = compare_and_adopt(&allocation.allocation_root, &stable, &request)
        .expect("idempotent owner replay");
    let selected = current_owner(&allocation.allocation_root).expect("selected owner");

    assert_eq!(first.new_owner, generation);
    assert_eq!(replay.new_owner, generation);
    assert!(replay.idempotent_replay);
    assert_eq!(selected, generation);
}

#[test]
fn failed_attempt_is_retained_when_a_later_attempt_passes() {
    let root = TestRoot::new("retained-failure");
    let ledger = CrashSweepLedger::open(&root.path).expect("open crash ledger");
    let point = NamedFaultPoint::SealingAfterDirFsync;
    let mut failed = passing_observation(
        point,
        CrashProtocolPhase::DurableSealing,
        true,
        1,
        CrashExecutionMode::HostInjection,
    );
    failed.after.owner_count = 0;
    failed.after.owner_allocation_id = None;
    failed.selected_visibility = SelectedVisibility::PartialNew;
    failed.post_sealing_session_resumed = true;
    failed.unclassified_debt_bytes = 1;
    let failed_record = ledger.record(failed).expect("record failed attempt");
    assert!(!failed_record.passed);
    assert!(failed_record.failures.len() >= 4);

    let passing_record = ledger
        .record(passing_observation(
            point,
            CrashProtocolPhase::DurableSealing,
            true,
            2,
            CrashExecutionMode::HostInjection,
        ))
        .expect("record passing retry");
    assert!(passing_record.passed);

    let summary = ledger.summary(false).expect("summarize retained attempts");
    assert_eq!(summary.recorded_attempts, 2);
    assert_eq!(summary.failed_attempts, 1);
    assert_eq!(summary.passing_fault_points, 1);
    assert_eq!(summary.physical_passing_fault_points, 0);
    assert_eq!(summary.missing_fault_points.len(), 45);
    assert!(root
        .path
        .join("attempts")
        .join(point.as_str())
        .join("00000001.json")
        .is_file());
    assert!(root
        .path
        .join("attempts")
        .join(point.as_str())
        .join("00000002.json")
        .is_file());
}

fn passing_observation(
    fault_point: NamedFaultPoint,
    protocol_phase: CrashProtocolPhase,
    session_terminal: bool,
    attempt: u32,
    execution_mode: CrashExecutionMode,
) -> CrashRecoveryObservation {
    let operation_id = OperationId::from_string(format!("operation-{}", fault_point.as_str()));
    let binding = hv07_operation_bindings()
        .iter()
        .find(|binding| binding.fault_point == fault_point)
        .expect("frozen point has a real-operation binding");
    let selected_visibility = if session_terminal {
        SelectedVisibility::CompleteNew
    } else {
        SelectedVisibility::Old
    };
    CrashRecoveryObservation {
        schema_version: SCHEMA_VERSION,
        fault_point,
        attempt,
        execution_mode,
        operation_id: operation_id.clone(),
        retry_operation_id: operation_id.clone(),
        before: DurableCrashWitness {
            schema_version: SCHEMA_VERSION,
            protocol_phase,
            recovery_phase: None,
            owner_count: 1,
            owner_allocation_id: Some(AllocationId::from_string("allocation-old")),
            owner_epoch: Some(1),
            locator_generation: None,
            ref_sequence: None,
            session_terminal,
            state_parent_synced: true,
        },
        after: DurableCrashWitness {
            schema_version: SCHEMA_VERSION,
            protocol_phase,
            recovery_phase: None,
            owner_count: 1,
            owner_allocation_id: Some(AllocationId::from_string("allocation-selected")),
            owner_epoch: Some(2),
            locator_generation: None,
            ref_sequence: None,
            session_terminal,
            state_parent_synced: true,
        },
        real_operation_witness: Some(RealOperationWitness {
            schema_version: SCHEMA_VERSION,
            format: "mpla-poc-real-operation-witness-v1".to_owned(),
            fault_point,
            protocol_phase: binding.protocol_phase,
            operation: binding.operation,
            durable_boundary: binding.durable_boundary.to_owned(),
            operation_id: operation_id.clone(),
            durable_state_paths: vec![PathBuf::from("durable-operation-state")],
            operation_state_parent_synced: false,
            stationary_payload_path_before: Some(PathBuf::from("allocation/upper")),
            stationary_payload_path_after: Some(PathBuf::from("allocation/upper")),
            payload_bytes_moved: 0,
            payload_bytes_copied: 0,
            recorded_unix_ms: 1,
        }),
        physical_kill_witness: None,
        recovery_replay_witness: Some(RecoveryReplayWitness {
            schema_version: SCHEMA_VERSION,
            fault_point,
            operation_id: operation_id.clone(),
            retry_operation_id: operation_id,
            recovery_invoked: true,
            recovery_completed: true,
            terminal_invariant_verified: true,
            selected_visibility,
            exact_owner_verified: true,
            exact_locator_verified: true,
            exact_ref_verified: true,
            stationary_payload_verified: true,
            failed_attempt_bundle_durable: true,
            cancelled_attempt_bundle_durable: true,
            idempotent_retry_verified: true,
        }),
        selected_visibility,
        idempotent_retry_same_result: true,
        post_sealing_session_resumed: false,
        failed_span_retained: true,
        cancelled_span_retained: true,
        observed_debt_bytes: 12_288,
        temporary_debt_bytes: 8_192,
        retirement_debt_bytes: 4_096,
        unclassified_debt_bytes: 0,
    }
}
