use std::path::{Path, PathBuf};

use sandbox_runtime_mpla_poc::allocation::{create_allocation, open_allocation};
use sandbox_runtime_mpla_poc::durable;
use sandbox_runtime_mpla_poc::inventory::capture_stable_pair;
use sandbox_runtime_mpla_poc::lease::issue_workspace_lease;
use sandbox_runtime_mpla_poc::owner::current_owner;
use sandbox_runtime_mpla_poc::storage_admin::{
    storage_admin_mount_plan_evidence, storage_admin_mountinfo_target_sha256,
    storage_admin_process_evidence_from_status, StorageAdminMountReceiptBinding,
    StorageAdminMountTableEvidence, StorageAdminObservedMount,
};
use sandbox_runtime_mpla_poc::{
    prepare_external_session, stationary_adopt_prepared, AllocationHandle, AllocationId,
    ExternalStationarySeal, FaultInjector, MutableLease, OperationId, OwnerSubject, PocError,
    PreparedExternalSession, PublicationId, PublicationPhase, RunId, SessionId, SessionPhase,
    SessionRecord, StationaryPublicationRequest, StorageAdminAction, StorageAdminOutcome,
    StorageAdminReceipt, StorageAdminScope, INTERFACE_VERSION, SCHEMA_VERSION,
    STORAGE_ADMIN_EFFECTIVE_CAPABILITIES, STORAGE_ADMIN_PRIVILEGED_SYSCALLS,
    STORAGE_ADMIN_PROFILE_ID, STORAGE_ADMIN_TRUSTED_EXECUTABLE,
};

#[test]
fn fresh_allocation_durability_batch_commits_a_reopenable_exact_session() {
    let temp = TempDirectory::new();
    let payload_root = temp.path().join("payload");
    let control_root = temp.path().join("control");
    let operation_id = OperationId::from_string("batched-fresh-allocation");
    let session_id = SessionId::from_string("batched-fresh-session");

    let batch = durable::begin_durability_batch();
    let allocation = create_allocation(&payload_root.join("allocations"), &operation_id)
        .expect("create batched allocation");
    let lease = issue_workspace_lease(&allocation, session_id.clone(), &operation_id)
        .expect("issue batched lease");
    let prepared = prepare_external_session(&control_root, &allocation, &lease)
        .expect("prepare batched external session");
    batch
        .commit(&[&allocation.allocation_root, prepared.session_dir()])
        .expect("commit batched object graph");

    let reopened = open_allocation(
        &payload_root.join("allocations"),
        &allocation.descriptor.allocation_id,
    )
    .expect("reopen committed allocation");
    let replay = issue_workspace_lease(&reopened, session_id, &operation_id)
        .expect("replay committed lease");
    assert_eq!(replay, lease);
    let record: SessionRecord = durable::read_json(&prepared.session_dir().join("SESSION.json"))
        .expect("read committed session");
    assert_eq!(record.phase, SessionPhase::Open);
    assert_eq!(record.session_id, replay.session_id);
    assert_eq!(record.allocation_id, reopened.descriptor.allocation_id);
    assert_eq!(record.workspace_root, prepared.workspace_root());
}

#[test]
fn external_stationary_adoption_commits_once_and_replays() {
    let fixture = Fixture::new();
    let first = fixture
        .publish(fixture.seal.clone())
        .expect("first adoption");
    assert!(!first.idempotent_replay);
    assert!(first.stale_writer_rejected);
    assert!(first.stale_deleter_rejected);
    assert_eq!(
        first.stable_inventory_sha256(),
        fixture.seal.first_inventory.inventory_sha256.as_str()
    );
    assert!(matches!(
        current_owner(&fixture.allocation.allocation_root)
            .expect("selected owner")
            .subject,
        OwnerSubject::PayloadOwned { publication_id }
            if publication_id == fixture.request.publication_id
    ));
    assert_eq!(fixture.session_record().phase, SessionPhase::Open);
    assert!(fixture
        .prepared
        .has_ratified_sealing(&fixture.allocation, &fixture.lease)
        .expect("ratified Sealing"));
    assert_eq!(
        fixture.operation_record().phase,
        PublicationPhase::PayloadOwned
    );

    let replay = fixture
        .publish(fixture.seal.clone())
        .expect("replay adoption");
    assert!(replay.idempotent_replay);
    assert_eq!(replay.adoption, first.adoption);
    assert_eq!(fixture.session_record().phase, SessionPhase::Open);
    assert!(fixture
        .prepared
        .has_ratified_sealing(&fixture.allocation, &fixture.lease)
        .expect("ratified Sealing replay"));
    assert_eq!(
        fixture.operation_record().phase,
        PublicationPhase::PayloadOwned
    );
}

#[test]
fn external_stationary_adoption_rejects_wrong_storage_action() {
    let fixture = Fixture::new();
    let mut seal = fixture.seal.clone();
    seal.quiesce.action = StorageAdminAction::StrictUnmount;
    persist_storage_receipt(&seal.quiesce);
    assert_pre_adoption_rejected(&fixture, seal);
}

#[test]
fn external_stationary_adoption_rejects_wrong_allocation_scope() {
    let fixture = Fixture::new();
    let mut seal = fixture.seal.clone();
    seal.quiesce.scope.allocation_id = AllocationId::from_string("other-allocation");
    persist_storage_receipt(&seal.quiesce);
    assert_pre_adoption_rejected(&fixture, seal);
}

#[test]
fn external_stationary_adoption_rejects_wrong_session_scope() {
    let fixture = Fixture::new();
    let mut seal = fixture.seal.clone();
    seal.quiesce.scope.session_id = SessionId::from_string("other-session");
    persist_storage_receipt(&seal.quiesce);
    assert_pre_adoption_rejected(&fixture, seal);
}

#[test]
fn external_stationary_adoption_rejects_unequal_inventories() {
    let fixture = Fixture::new();
    let mut seal = fixture.seal.clone();
    seal.second_inventory.inventory_sha256 = "b".repeat(64);
    assert_pre_adoption_rejected(&fixture, seal);
}

#[test]
fn external_stationary_adoption_rejects_mounted_strict_unmount_target() {
    let fixture = Fixture::new();
    let mut seal = fixture.seal.clone();
    let target = observed_mount(&seal.strict_unmount.scope);
    seal.strict_unmount.mount_plan_evidence.mountinfo_after = mounted_table(target);
    persist_storage_receipt(&seal.strict_unmount);
    assert_pre_adoption_rejected(&fixture, seal);
}

#[test]
fn ratified_external_sealing_fails_closed_for_mismatched_scope() {
    let fixture = Fixture::new();
    let sealing_path = fixture.prepared.session_dir().join("SEALING.json");
    let mut sealing: sandbox_runtime_mpla_poc::SealingRecord =
        durable::read_json(&sealing_path).expect("read Sealing");
    sealing.owner_epoch += 1;
    durable::replace_json(&sealing_path, &sealing).expect("replace mismatched Sealing");

    assert!(matches!(
        fixture
            .prepared
            .has_ratified_sealing(&fixture.allocation, &fixture.lease),
        Err(PocError::RecoveryRequired(_))
    ));
}

fn assert_pre_adoption_rejected(fixture: &Fixture, seal: ExternalStationarySeal) {
    assert!(matches!(
        fixture.publish(seal),
        Err(PocError::Integrity(_) | PocError::RecoveryRequired(_))
    ));
    assert!(matches!(
        current_owner(&fixture.allocation.allocation_root)
            .expect("workspace owner")
            .subject,
        OwnerSubject::WorkspaceOwned { .. }
    ));
    assert_eq!(
        fixture.session_record().phase,
        SessionPhase::RecoveryRequired
    );
}

struct Fixture {
    _temp: TempDirectory,
    allocation: AllocationHandle,
    lease: MutableLease,
    prepared: PreparedExternalSession,
    operations_root: PathBuf,
    request: StationaryPublicationRequest,
    seal: ExternalStationarySeal,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDirectory::new();
        let payload_root = temp.path().join("payload");
        let control_root = temp.path().join("control");
        let lower = temp.path().join("lower");
        std::fs::create_dir_all(&lower).expect("create lower");
        let allocation_operation = OperationId::from_string("create-allocation");
        let allocation =
            create_allocation(&payload_root.join("allocations"), &allocation_operation)
                .expect("create allocation");
        std::fs::write(allocation.upper_dir.join("changed.txt"), b"changed\n")
            .expect("write upper payload");
        let lease_operation = OperationId::from_string("issue-lease");
        let lease = issue_workspace_lease(
            &allocation,
            SessionId::from_string("session-external"),
            &lease_operation,
        )
        .expect("issue lease");
        let prepared = prepare_external_session(&control_root, &allocation, &lease)
            .expect("prepare external session");
        let request = StationaryPublicationRequest {
            schema_version: SCHEMA_VERSION,
            operation_id: OperationId::from_string("publish-external"),
            publication_id: PublicationId::from_string("publication-external"),
        };
        prepared
            .begin_sealing(
                &allocation,
                &lease,
                &request.operation_id,
                &mut FaultInjector::default(),
            )
            .expect("begin external Sealing");
        let (first_inventory, second_inventory) =
            capture_stable_pair(&allocation).expect("capture stable pair");
        let scope = StorageAdminScope {
            run_id: RunId::parse("external-publication-test").expect("run id"),
            sandbox_id: "sandbox-external".to_owned(),
            workspace_session_id: "workspace-external".to_owned(),
            session_id: lease.session_id.clone(),
            allocation_id: allocation.descriptor.allocation_id.clone(),
            lease_id: lease_operation.as_str().to_owned(),
            lease_epoch: lease.lease_epoch,
            mount_namespace_id: "mnt:[4026532999]".to_owned(),
            payload_root,
            control_root: control_root.clone(),
            lower_dirs_newest_first: vec![lower],
            allocation_root: allocation.allocation_root.clone(),
            workspace_root: prepared.workspace_root().to_path_buf(),
        };
        let binding = StorageAdminMountReceiptBinding {
            storage_operation_id: OperationId::from_string("mount-external"),
            attestation_sha256: "a".repeat(64),
        };
        let quiesce = storage_receipt(
            StorageAdminAction::Quiesce,
            "quiesce-external",
            scope.clone(),
            binding.clone(),
            true,
            1,
        );
        let strict_unmount = storage_receipt(
            StorageAdminAction::StrictUnmount,
            "strict-unmount-external",
            scope,
            binding,
            false,
            2,
        );
        persist_storage_receipt(&quiesce);
        persist_storage_receipt(&strict_unmount);
        Self {
            _temp: temp,
            allocation,
            lease,
            prepared,
            operations_root: control_root.join("operations"),
            request,
            seal: ExternalStationarySeal {
                quiesce,
                strict_unmount,
                first_inventory,
                second_inventory,
                workload_cgroup_empty: true,
            },
        }
    }

    fn publish(
        &self,
        seal: ExternalStationarySeal,
    ) -> sandbox_runtime_mpla_poc::PocResult<
        sandbox_runtime_mpla_poc::ExternalStationaryPublicationReceipt,
    > {
        stationary_adopt_prepared(
            &self.prepared,
            &self.allocation,
            &self.lease,
            &self.request,
            &self.operations_root,
            seal,
            &mut FaultInjector::default(),
        )
    }

    fn session_record(&self) -> SessionRecord {
        durable::read_json(&self.prepared.session_dir().join("SESSION.json"))
            .expect("read session record")
    }

    fn operation_record(&self) -> sandbox_runtime_mpla_poc::PublicationOperationRecord {
        durable::read_json(
            &self
                .operations_root
                .join("publication")
                .join(self.request.operation_id.as_str())
                .join("OPERATION.json"),
        )
        .expect("read publication operation record")
    }
}

fn storage_receipt(
    action: StorageAdminAction,
    operation_id: &str,
    scope: StorageAdminScope,
    binding: StorageAdminMountReceiptBinding,
    mounted_after: bool,
    completed_unix_ms: u64,
) -> StorageAdminReceipt {
    let operation_id = OperationId::from_string(operation_id);
    let process_evidence = storage_admin_process_evidence_from_status(
        PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
        "0".repeat(64),
        "CapInh:\t0000000000000000\nCapPrm:\t0000000000200000\nCapEff:\t0000000000200000\nCapBnd:\t0000000000200000\nCapAmb:\t0000000000000000\nNoNewPrivs:\t1\nSeccomp:\t2\nSeccomp_filters:\t1\n",
        scope.control_root.join("workload/cgroup.procs"),
        4242,
        scope.mount_namespace_id.clone(),
        4_026_532_999,
    )
    .expect("process evidence");
    let mut mount_plan = storage_admin_mount_plan_evidence(&scope).expect("mount-plan evidence");
    let target = observed_mount(&scope);
    mount_plan.mountinfo_before = mounted_table(target.clone());
    mount_plan.mountinfo_after = if mounted_after {
        mounted_table(target)
    } else {
        StorageAdminMountTableEvidence {
            sha256: storage_admin_mountinfo_target_sha256(None).expect("absent target digest"),
            target: None,
        }
    };
    let receipt_path = scope
        .control_root
        .join("storage-admin")
        .join(operation_id.as_str())
        .join("RECEIPT.json");
    StorageAdminReceipt {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        profile_id: STORAGE_ADMIN_PROFILE_ID.to_owned(),
        operation_id,
        action,
        request_sha256: "c".repeat(64),
        trusted_executable: PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
        effective_capabilities: STORAGE_ADMIN_EFFECTIVE_CAPABILITIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        allowed_privileged_syscalls: STORAGE_ADMIN_PRIVILEGED_SYSCALLS
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        process_evidence,
        mount_plan_evidence: mount_plan,
        mount_attestation: None,
        mount_receipt_binding: Some(binding),
        scope,
        outcome: StorageAdminOutcome::Succeeded,
        idempotent_replay: false,
        cleanup_complete: true,
        failure: None,
        started_unix_ms: completed_unix_ms,
        completed_unix_ms,
        receipt_path,
    }
}

fn observed_mount(scope: &StorageAdminScope) -> StorageAdminObservedMount {
    StorageAdminObservedMount {
        mount_id: 42,
        parent_mount_id: 1,
        root: PathBuf::from("/"),
        source: "overlay".to_owned(),
        filesystem_type: "overlay".to_owned(),
        target: scope.workspace_root.clone(),
        mount_options: vec!["rw".to_owned()],
        optional_fields: Vec::new(),
        super_options: vec!["rw".to_owned()],
        upper_dir: Some(scope.allocation_root.join("upper")),
        work_dir: Some(scope.allocation_root.join("work")),
    }
}

fn mounted_table(target: StorageAdminObservedMount) -> StorageAdminMountTableEvidence {
    StorageAdminMountTableEvidence {
        sha256: storage_admin_mountinfo_target_sha256(Some(&target))
            .expect("mounted target digest"),
        target: Some(target),
    }
}

fn persist_storage_receipt(receipt: &StorageAdminReceipt) {
    durable::replace_json(&receipt.receipt_path, receipt).expect("persist storage receipt");
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mpla-external-publication-{}-{}",
            std::process::id(),
            OperationId::new()
        ));
        std::fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
