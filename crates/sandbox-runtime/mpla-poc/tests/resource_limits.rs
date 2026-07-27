use sandbox_runtime_mpla_poc::config::MAX_PENDING_DESCRIPTOR_BYTES;
use sandbox_runtime_mpla_poc::{AdmissionController, AdmissionTier, PocError};

#[test]
fn fixed_admission_owns_physical_resources_only_for_four_active_jobs() {
    let controller = AdmissionController::new();
    let guards = (1..=32)
        .map(|_| controller.submit(MAX_PENDING_DESCRIPTOR_BYTES / 16))
        .collect::<Result<Vec<_>, _>>()
        .expect("first 32 jobs fit the fixed tiers");

    for guard in &guards[..4] {
        assert_eq!(guard.receipt().tier, AdmissionTier::ActiveData);
        assert!(guard.receipt().owns_payload_allocation);
        assert!(guard.receipt().owns_workspace_mount);
    }
    for guard in &guards[4..] {
        assert!(!guard.receipt().owns_payload_allocation);
        assert!(!guard.receipt().owns_workspace_mount);
        assert!(!guard.receipt().owns_staging_allocation);
    }
    let snapshot = controller.snapshot().expect("snapshot");
    assert_eq!(snapshot.active_data_workers, 4);
    assert_eq!(snapshot.coordinators, 16);
    assert_eq!(snapshot.pending_descriptors, 16);
    assert_eq!(
        snapshot.pending_descriptor_bytes,
        MAX_PENDING_DESCRIPTOR_BYTES
    );

    let error = controller
        .submit(1)
        .expect_err("job 33 must reject before ownership");
    assert!(matches!(error, PocError::Overloaded(_)));
    let after = controller.snapshot().expect("snapshot after reject");
    assert_eq!(after, snapshot);
}

#[test]
fn pending_descriptor_bytes_fail_closed() {
    let controller = AdmissionController::new();
    let guards = (0..16)
        .map(|_| controller.submit(0))
        .collect::<Result<Vec<_>, _>>()
        .expect("active and coordinator tiers");
    let pending = controller
        .submit(MAX_PENDING_DESCRIPTOR_BYTES)
        .expect("one full aggregate descriptor");
    let error = controller.submit(1).expect_err("aggregate descriptor cap");
    assert!(matches!(error, PocError::Overloaded(_)));
    drop((guards, pending));
}

#[test]
fn admitted_jobs_promote_in_fifo_order_without_changing_identity() {
    let controller = AdmissionController::new();
    let mut guards = (1..=32)
        .map(|_| controller.submit(MAX_PENDING_DESCRIPTOR_BYTES / 16))
        .collect::<Result<Vec<_>, _>>()
        .expect("admit complete bounded queue");

    assert_eq!(guards[4].receipt().job_ordinal, 5);
    assert_eq!(guards[16].receipt().job_ordinal, 17);
    drop(guards.remove(0));

    assert!(guards[4]
        .try_promote()
        .expect("later coordinator promotion check")
        .is_none());
    let promoted = guards[3]
        .try_promote()
        .expect("promote oldest coordinator")
        .expect("active slot is available");
    assert_eq!(promoted.job_ordinal, 5);
    assert_eq!(promoted.tier, AdmissionTier::ActiveData);
    assert!(promoted.owns_payload_allocation);
    assert!(promoted.owns_workspace_mount);

    let descriptor = guards[15]
        .try_promote()
        .expect("promote oldest pending descriptor")
        .expect("coordinator slot is available");
    assert_eq!(descriptor.job_ordinal, 17);
    assert_eq!(descriptor.tier, AdmissionTier::Coordinator);
    assert!(!descriptor.owns_payload_allocation);
    assert!(guards[16]
        .try_promote()
        .expect("later pending promotion check")
        .is_none());

    let snapshot = controller.snapshot().expect("promoted snapshot");
    assert_eq!(snapshot.active_data_workers, 4);
    assert_eq!(snapshot.coordinators, 16);
    assert_eq!(snapshot.pending_descriptors, 15);
    assert_eq!(
        snapshot.pending_descriptor_bytes,
        15 * (MAX_PENDING_DESCRIPTOR_BYTES / 16)
    );
    assert_eq!(snapshot.private_allocations, 4);
    assert_eq!(snapshot.active_mounts, 4);
}
