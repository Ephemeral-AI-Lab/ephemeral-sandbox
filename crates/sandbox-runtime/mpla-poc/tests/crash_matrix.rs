use std::path::PathBuf;

use sandbox_runtime_mpla_poc::recovery::{
    hv07_fault_expectations, CrashExecutionMode, CrashProtocolPhase, CrashRecoveryObservation,
    CrashSweepLedger, DurableCrashWitness, SelectedVisibility,
};
use sandbox_runtime_mpla_poc::{
    AllocationId, NamedFaultPoint, OperationId, PocError, SCHEMA_VERSION,
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
fn developmental_sweep_accounts_for_every_frozen_faultpoint() {
    let root = TestRoot::new("complete-registry");
    let ledger = CrashSweepLedger::open(&root.path).expect("open crash ledger");
    let expectations = hv07_fault_expectations();
    assert_eq!(expectations.len(), NamedFaultPoint::ALL.len());
    assert_eq!(
        expectations
            .iter()
            .map(|expectation| expectation.fault_point)
            .collect::<Vec<_>>(),
        NamedFaultPoint::ALL
    );

    for expectation in expectations {
        let record = ledger
            .record(passing_observation(
                expectation.fault_point,
                expectation.protocol_phase,
                expectation.terminal_session_required,
                1,
                CrashExecutionMode::HostInjection,
            ))
            .expect("record developmental crash attempt");
        assert!(record.passed, "{:?}", record.failures);
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
    CrashRecoveryObservation {
        schema_version: SCHEMA_VERSION,
        fault_point,
        attempt,
        execution_mode,
        operation_id: operation_id.clone(),
        retry_operation_id: operation_id,
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
        selected_visibility: if session_terminal {
            SelectedVisibility::CompleteNew
        } else {
            SelectedVisibility::Old
        },
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
