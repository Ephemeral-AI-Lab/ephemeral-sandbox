use std::path::PathBuf;

use sandbox_runtime_mpla_poc::storage_admin::{
    storage_admin_mount_plan_evidence, storage_admin_process_evidence_from_status,
};
use sandbox_runtime_mpla_poc::{
    AllocationId, OperationId, PocConfig, RunId, SessionId, StorageAdminAction,
    StorageAdminAuthorization, StorageAdminOutcome, StorageAdminReceipt, StorageAdminRequest,
    StorageAdminScope, INTERFACE_VERSION, STORAGE_ADMIN_EFFECTIVE_CAPABILITIES,
    STORAGE_ADMIN_PRIVILEGED_SYSCALLS, STORAGE_ADMIN_PROFILE_ID, STORAGE_ADMIN_TRUSTED_EXECUTABLE,
};

#[test]
fn fixed_config_round_trips() {
    let config = PocConfig::default();
    config.validate().expect("fixed config must validate");
    let encoded = serde_json::to_vec(&config).expect("config must encode");
    let decoded: PocConfig = serde_json::from_slice(&encoded).expect("config must decode");
    assert_eq!(decoded, config);
    assert_eq!(INTERFACE_VERSION, "m2r-iface-v1");
}

#[test]
fn run_id_rejects_unsafe_targets() {
    for invalid in ["", "/", "~", "all/run", "*.json", "-leading"] {
        assert!(RunId::parse(invalid).is_err(), "{invalid}");
    }
    assert!(RunId::parse("m0-20260727T130703p0800").is_ok());
}

#[test]
fn corrective_storage_admin_contract_round_trips() {
    let scope = StorageAdminScope {
        run_id: RunId::parse("m2r-20260728T015724p0800").expect("run ID"),
        sandbox_id: "eos-corrective".to_owned(),
        workspace_session_id: "workspace-corrective".to_owned(),
        session_id: SessionId::from_string("session-1"),
        allocation_id: AllocationId::from_string("allocation-1"),
        lease_id: "m2r-20260728T015724p0800:lead:SECURITY".to_owned(),
        lease_epoch: 7,
        mount_namespace_id: "mnt:[4026532999]".to_owned(),
        payload_root: PathBuf::from("/mpla/payload"),
        control_root: PathBuf::from("/mpla/control"),
        lower_dirs_newest_first: vec![PathBuf::from("/mpla/lower")],
        allocation_root: PathBuf::from("/mpla/allocation"),
        workspace_root: PathBuf::from("/mpla/workspace"),
    };
    let request = StorageAdminRequest {
        schema_version: 1,
        interface_version: INTERFACE_VERSION.to_owned(),
        profile_id: STORAGE_ADMIN_PROFILE_ID.to_owned(),
        operation_id: OperationId::from_string("operation-1"),
        action: StorageAdminAction::Mount,
        scope: scope.clone(),
    };
    let authorization = StorageAdminAuthorization {
        authenticated: true,
        actor_id: "mpla-poc-candidate".to_owned(),
        operation_id: request.operation_id.clone(),
        run_id: scope.run_id.clone(),
        sandbox_id: scope.sandbox_id.clone(),
        workspace_session_id: scope.workspace_session_id.clone(),
        session_id: scope.session_id.clone(),
        allocation_id: scope.allocation_id.clone(),
        lease_id: scope.lease_id.clone(),
        lease_epoch: scope.lease_epoch,
        mount_namespace_id: scope.mount_namespace_id.clone(),
    };
    let receipt = StorageAdminReceipt {
        schema_version: 1,
        interface_version: INTERFACE_VERSION.to_owned(),
        profile_id: STORAGE_ADMIN_PROFILE_ID.to_owned(),
        operation_id: request.operation_id.clone(),
        action: request.action,
        request_sha256: "a".repeat(64),
        trusted_executable: PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
        effective_capabilities: STORAGE_ADMIN_EFFECTIVE_CAPABILITIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        allowed_privileged_syscalls: STORAGE_ADMIN_PRIVILEGED_SYSCALLS
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        process_evidence: storage_admin_process_evidence_from_status(
            PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
            "00".repeat(32),
            "CapInh:\t0000000000000000\nCapPrm:\t0000000000200000\nCapEff:\t0000000000200000\nCapBnd:\t0000000000200000\nCapAmb:\t0000000000000000\nNoNewPrivs:\t1\nSeccomp:\t2\nSeccomp_filters:\t1\n",
            PathBuf::from("/mpla-test/cgroup.procs"),
            4242,
            "mnt:[4026532999]".to_owned(),
            4_026_532_999,
        )
        .expect("process evidence"),
        mount_plan_evidence: storage_admin_mount_plan_evidence(&scope)
            .expect("mount-plan evidence"),
        mount_attestation: None,
        mount_receipt_binding: None,
        scope,
        outcome: StorageAdminOutcome::Succeeded,
        idempotent_replay: false,
        cleanup_complete: true,
        failure: None,
        started_unix_ms: 0,
        completed_unix_ms: 1,
        receipt_path: PathBuf::from("/mpla/control/receipts/operation-1.json"),
    };

    for value in [
        serde_json::to_value(&request).expect("request"),
        serde_json::to_value(&authorization).expect("authorization"),
        serde_json::to_value(&receipt).expect("receipt"),
    ] {
        let encoded = serde_json::to_vec(&value).expect("encode");
        let decoded: serde_json::Value = serde_json::from_slice(&encoded).expect("decode");
        assert_eq!(decoded, value);
    }
}
