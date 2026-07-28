use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sandbox_runtime_mpla_poc::storage_admin::{
    authorize_storage_admin, decode_invocation, run_storage_admin,
    storage_admin_mount_plan_evidence, storage_admin_process_evidence_from_status,
    validate_opened_mount_namespace, OrdinaryWorkloadPolicy, StorageAdminExecution,
    StorageAdminInvocation, StorageAdminLifecycle, StorageAdminPreparationStep,
    StorageAdminProcessProfile, STORAGE_ADMIN_SECCOMP_PROFILE_ID,
};
use sandbox_runtime_mpla_poc::{
    AllocationId, OperationId, RunId, SessionId, StorageAdminAction, StorageAdminAuthorization,
    StorageAdminOutcome, StorageAdminRequest, StorageAdminScope, INTERFACE_VERSION, SCHEMA_VERSION,
    STORAGE_ADMIN_EFFECTIVE_CAPABILITIES, STORAGE_ADMIN_PRIVILEGED_SYSCALLS,
    STORAGE_ADMIN_PROFILE_ID, STORAGE_ADMIN_TRUSTED_EXECUTABLE,
};
use uuid::Uuid;

const TRUSTED_ACTOR: &str = "mpla-corrective-lead";
const NAMESPACE_HOLDER_PID: u32 = 4_242;

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mpla-storage-admin-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create test root");
        Self(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct FakeLifecycle {
    execution: StorageAdminExecution,
    executions: usize,
    recoveries: usize,
    commits: usize,
}

impl FakeLifecycle {
    fn succeeding() -> Self {
        Self {
            execution: StorageAdminExecution::succeeded(),
            executions: 0,
            recoveries: 0,
            commits: 0,
        }
    }

    fn returning(execution: StorageAdminExecution) -> Self {
        Self {
            execution,
            executions: 0,
            recoveries: 0,
            commits: 0,
        }
    }
}

impl StorageAdminLifecycle for FakeLifecycle {
    fn execute(
        &mut self,
        _action: StorageAdminAction,
        _scope: &StorageAdminScope,
    ) -> StorageAdminExecution {
        self.executions += 1;
        self.execution.clone()
    }

    fn recover_incomplete(
        &mut self,
        _action: StorageAdminAction,
        _scope: &StorageAdminScope,
    ) -> StorageAdminExecution {
        self.recoveries += 1;
        StorageAdminExecution::failed("recovered incomplete test operation", true)
    }

    fn receipt_committed(&mut self, _action: StorageAdminAction, _scope: &StorageAdminScope) {
        self.commits += 1;
    }

    fn authority_evidence(
        &mut self,
        scope: &StorageAdminScope,
    ) -> sandbox_runtime_mpla_poc::PocResult<(
        sandbox_runtime_mpla_poc::storage_admin::StorageAdminProcessEvidence,
        sandbox_runtime_mpla_poc::storage_admin::StorageAdminMountPlanEvidence,
    )> {
        let status = "CapInh:\t0000000000000000\nCapPrm:\t0000000000200000\nCapEff:\t0000000000200000\nCapBnd:\t0000000000200000\nCapAmb:\t0000000000000000\nNoNewPrivs:\t1\nSeccomp:\t2\nSeccomp_filters:\t1\n";
        Ok((
            storage_admin_process_evidence_from_status(
                PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
                "00".repeat(32),
                status,
                PathBuf::from("/mpla-test/cgroup.procs"),
                NAMESPACE_HOLDER_PID,
                scope.mount_namespace_id.clone(),
                scope
                    .mount_namespace_id
                    .trim_start_matches("mnt:[")
                    .trim_end_matches(']')
                    .parse()
                    .expect("valid mount namespace inode"),
            )?,
            storage_admin_mount_plan_evidence(scope)?,
        ))
    }
}

fn invocation(root: &Path, operation_id: &str) -> StorageAdminInvocation {
    let scope = StorageAdminScope {
        run_id: RunId::parse("m2r-20260728T015724p0800").expect("run id"),
        sandbox_id: "sandbox-security".to_owned(),
        workspace_session_id: "workspace-security".to_owned(),
        session_id: SessionId::from_string("session-security"),
        allocation_id: AllocationId::from_string("allocation-security"),
        lease_id: "m2r-lease-security:7".to_owned(),
        lease_epoch: 7,
        mount_namespace_id: "mnt:[4026532999]".to_owned(),
        payload_root: root.join("payload"),
        control_root: root.join("control"),
        lower_dirs_newest_first: vec![root.join("payload/lower-1")],
        allocation_root: root.join("allocation"),
        workspace_root: root.join("workspace"),
    };
    let request = StorageAdminRequest {
        schema_version: SCHEMA_VERSION,
        interface_version: INTERFACE_VERSION.to_owned(),
        profile_id: STORAGE_ADMIN_PROFILE_ID.to_owned(),
        operation_id: OperationId::from_string(operation_id),
        action: StorageAdminAction::Mount,
        scope: scope.clone(),
    };
    let authorization = StorageAdminAuthorization {
        authenticated: true,
        actor_id: TRUSTED_ACTOR.to_owned(),
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
    StorageAdminInvocation {
        expected_request: request.clone(),
        request,
        authorization,
        trusted_actor_id: TRUSTED_ACTOR.to_owned(),
        trusted_executable_sha256: "00".repeat(32),
        workload_cgroup_procs: root.join("workload/cgroup.procs"),
        mount_namespace_holder_pid: NAMESPACE_HOLDER_PID,
    }
}

fn rejection(invocation: &StorageAdminInvocation) -> String {
    authorize_storage_admin(
        &invocation.expected_request,
        &invocation.request,
        &invocation.authorization,
        &invocation.trusted_actor_id,
    )
    .expect_err("substitution must fail closed")
    .to_string()
}

#[test]
fn only_exact_authenticated_lifecycle_request_selects_storage_profile() {
    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-exact");
    let selection = authorize_storage_admin(
        &invocation.expected_request,
        &invocation.request,
        &invocation.authorization,
        &invocation.trusted_actor_id,
    )
    .expect("exact request selects profile");

    assert_eq!(selection.request(), &invocation.request);
    assert_eq!(selection.profile_id(), STORAGE_ADMIN_PROFILE_ID);
    assert_eq!(
        selection.trusted_executable(),
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    );
    assert_eq!(selection.request_sha256().len(), 64);

    let mut unauthenticated = invocation.clone();
    unauthenticated.authorization.authenticated = false;
    assert!(rejection(&unauthenticated).contains("not authenticated"));

    let mut wrong_actor = invocation.clone();
    wrong_actor.authorization.actor_id = "ordinary-workload".to_owned();
    assert!(rejection(&wrong_actor).contains("actor id"));
}

#[test]
fn every_request_scope_substitution_is_rejected_before_execution() {
    let root = TestRoot::new();
    let original = invocation(&root.0, "operation-binding");
    let mut substitutions = Vec::new();

    let mut changed = original.clone();
    changed.request.operation_id = OperationId::from_string("operation-other");
    substitutions.push(("operation", changed));

    let mut changed = original.clone();
    changed.request.scope.run_id = RunId::parse("m2r-other").expect("run id");
    substitutions.push(("run", changed));

    let mut changed = original.clone();
    changed.request.scope.sandbox_id = "sandbox-other".to_owned();
    substitutions.push(("sandbox", changed));

    let mut changed = original.clone();
    changed.request.scope.workspace_session_id = "workspace-other".to_owned();
    substitutions.push(("workspace", changed));

    let mut changed = original.clone();
    changed.request.scope.session_id = SessionId::from_string("session-other");
    substitutions.push(("MPLA session", changed));

    let mut changed = original.clone();
    changed.request.scope.allocation_id = AllocationId::from_string("allocation-other");
    substitutions.push(("allocation", changed));

    let mut changed = original.clone();
    changed.request.scope.lease_id = "m2r-lease-other:8".to_owned();
    substitutions.push(("lease id", changed));

    let mut changed = original.clone();
    changed.request.scope.lease_epoch += 1;
    substitutions.push(("lease epoch", changed));

    let mut changed = original.clone();
    changed.request.scope.mount_namespace_id = "mnt:[4026533000]".to_owned();
    substitutions.push(("mount namespace", changed));

    let mut changed = original.clone();
    changed.request.scope.payload_root = root.0.join("payload-other");
    substitutions.push(("payload root", changed));

    let mut changed = original.clone();
    changed.request.scope.control_root = root.0.join("control-other");
    substitutions.push(("control root", changed));

    let mut changed = original.clone();
    changed.request.scope.lower_dirs_newest_first = vec![root.0.join("payload/lower-other")];
    substitutions.push(("lower directories", changed));

    let mut changed = original.clone();
    changed.request.scope.allocation_root = root.0.join("allocation-other");
    substitutions.push(("allocation root", changed));

    let mut changed = original.clone();
    changed.request.scope.workspace_root = root.0.join("workspace-other");
    substitutions.push(("workspace root", changed));

    let mut changed = original.clone();
    changed.request.action = StorageAdminAction::Cleanup;
    substitutions.push(("lifecycle action", changed));

    for (label, substituted) in substitutions {
        assert!(
            rejection(&substituted).contains(label),
            "{label} substitution did not identify its rejected binding"
        );
    }

    let mut lifecycle = FakeLifecycle::succeeding();
    let mut substituted = original.clone();
    substituted.request.scope.workspace_root = root.0.join("attacker-workspace");
    assert!(run_storage_admin(&substituted, &mut lifecycle).is_err());
    assert_eq!(lifecycle.executions, 0);
    assert_eq!(lifecycle.commits, 0);
}

#[test]
fn executable_command_shell_capability_and_extra_path_inputs_are_not_accepted() {
    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-wire");

    for (field, injected_value) in [
        (
            "executable",
            serde_json::json!("/tmp/attacker-storage-admin"),
        ),
        ("command", serde_json::json!(["/bin/sh", "-c", "mount"])),
        (
            "capabilities",
            serde_json::json!(["CAP_SYS_ADMIN", "CAP_SYS_PTRACE"]),
        ),
        ("namespace_path", serde_json::json!("/proc/1/ns/mnt")),
        ("allowed_path", serde_json::json!("/")),
    ] {
        let mut encoded = serde_json::to_value(&invocation).expect("encode invocation");
        encoded["request"]
            .as_object_mut()
            .expect("request object")
            .insert(field.to_owned(), injected_value);
        let bytes = serde_json::to_vec(&encoded).expect("encode substituted invocation");
        assert!(
            decode_invocation(&bytes).is_err(),
            "{field} must not be an accepted wire field"
        );
    }

    let profile = StorageAdminProcessProfile;
    assert_eq!(
        profile.trusted_executable(),
        Path::new(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    );
    assert!(!profile.allows_arbitrary_executable());

    let mut encoded = serde_json::to_value(&invocation).expect("encode invocation");
    encoded.as_object_mut().expect("invocation object").insert(
        "mount_namespace_path".to_owned(),
        serde_json::json!("/proc/1/ns/mnt"),
    );
    assert!(decode_invocation(
        &serde_json::to_vec(&encoded).expect("encode path-substituted invocation")
    )
    .is_err());
}

#[test]
fn exact_server_bound_namespace_holder_is_validated_before_setns_and_reverified() {
    let profile = StorageAdminProcessProfile;
    assert_eq!(
        profile
            .user_namespace_path(NAMESPACE_HOLDER_PID)
            .expect("trusted holder path"),
        PathBuf::from("/proc/4242/ns/user")
    );
    assert_eq!(
        profile
            .mount_namespace_path(NAMESPACE_HOLDER_PID)
            .expect("trusted holder path"),
        PathBuf::from("/proc/4242/ns/mnt")
    );
    assert!(profile.mount_namespace_path(0).is_err());

    validate_opened_mount_namespace("mnt:[4026532999]", "mnt:[4026532999]", 4_026_532_999)
        .expect("exact opened namespace");
    assert!(
        validate_opened_mount_namespace("mnt:[4026532999]", "mnt:[4026533000]", 4_026_533_000,)
            .is_err()
    );
    assert!(
        validate_opened_mount_namespace("mnt:[4026532999]", "mnt:[4026532999]", 4_026_533_000,)
            .is_err()
    );

    assert_eq!(
        profile.preparation_steps(),
        &[
            StorageAdminPreparationStep::OpenAndValidateBoundUserNamespace,
            StorageAdminPreparationStep::OpenAndValidateBoundMountNamespace,
            StorageAdminPreparationStep::EnterBoundUserNamespace,
            StorageAdminPreparationStep::VerifyEnteredUserNamespace,
            StorageAdminPreparationStep::EnterBoundMountNamespace,
            StorageAdminPreparationStep::VerifyEnteredMountNamespace,
            StorageAdminPreparationStep::NarrowCapabilityMasks,
            StorageAdminPreparationStep::SetNoNewPrivileges,
            StorageAdminPreparationStep::VerifyExecutableAndCapabilityIdentity,
        ]
    );

    let root = TestRoot::new();
    let mut invalid_holder = invocation(&root.0, "operation-invalid-holder");
    invalid_holder.mount_namespace_holder_pid = 0;
    let mut lifecycle = FakeLifecycle::succeeding();
    assert!(run_storage_admin(&invalid_holder, &mut lifecycle).is_err());
    assert_eq!(lifecycle.executions, 0);
}

#[test]
fn stale_and_fenced_authorizations_fail_closed() {
    let root = TestRoot::new();
    let original = invocation(&root.0, "operation-fence");

    let mut stale_epoch = original.clone();
    stale_epoch.authorization.lease_epoch -= 1;
    assert!(rejection(&stale_epoch).contains("lease epoch"));

    let mut stale_lease = original.clone();
    stale_lease.authorization.lease_id = "expired-lease:6".to_owned();
    assert!(rejection(&stale_lease).contains("lease id"));

    let mut fenced_session = original.clone();
    fenced_session.authorization.workspace_session_id = "fenced-workspace".to_owned();
    assert!(rejection(&fenced_session).contains("workspace session"));

    let mut wrong_namespace = original.clone();
    wrong_namespace.authorization.mount_namespace_id = "mnt:[1]".to_owned();
    assert!(rejection(&wrong_namespace).contains("mount namespace"));

    let mut replayed_for_other_operation = original.clone();
    replayed_for_other_operation.authorization.operation_id =
        OperationId::from_string("operation-prior");
    assert!(rejection(&replayed_for_other_operation).contains("operation id"));
}

#[test]
fn response_loss_retry_returns_one_stable_operation_and_receipt() {
    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-response-loss");
    let mut lifecycle = FakeLifecycle::succeeding();

    let first =
        run_storage_admin(&invocation, &mut lifecycle).expect("first execution must succeed");
    let durable_before = std::fs::read(&first.receipt_path).expect("read durable receipt");
    assert_eq!(lifecycle.executions, 1);
    assert_eq!(lifecycle.commits, 1);
    assert!(!first.idempotent_replay);

    let mut must_not_execute =
        FakeLifecycle::returning(StorageAdminExecution::failed("must not execute", false));
    let replay =
        run_storage_admin(&invocation, &mut must_not_execute).expect("retry returns receipt");
    let durable_after = std::fs::read(&replay.receipt_path).expect("reread durable receipt");

    assert_eq!(must_not_execute.executions, 0);
    assert_eq!(must_not_execute.recoveries, 0);
    assert_eq!(must_not_execute.commits, 0);
    assert!(replay.idempotent_replay);
    assert_eq!(replay.receipt_path, first.receipt_path);
    assert_eq!(replay.request_sha256, first.request_sha256);
    assert_eq!(replay.outcome, first.outcome);
    assert_eq!(replay.started_unix_ms, first.started_unix_ms);
    assert_eq!(replay.completed_unix_ms, first.completed_unix_ms);
    assert_eq!(durable_after, durable_before);

    let mut substituted_holder = invocation.clone();
    substituted_holder.mount_namespace_holder_pid += 1;
    let mut must_not_replay = FakeLifecycle::succeeding();
    assert!(run_storage_admin(&substituted_holder, &mut must_not_replay).is_err());
    assert_eq!(must_not_replay.executions, 0);
    assert_eq!(must_not_replay.recoveries, 0);
}

#[test]
fn failed_and_cancelled_operations_retain_durable_cleanup_evidence() {
    let root = TestRoot::new();
    let failed_invocation = invocation(&root.0, "operation-failed");
    let mut failed_lifecycle =
        FakeLifecycle::returning(StorageAdminExecution::failed("mount rejected", true));
    let failed = run_storage_admin(&failed_invocation, &mut failed_lifecycle)
        .expect("failed operation still returns durable receipt");
    let stored_failed: sandbox_runtime_mpla_poc::StorageAdminReceipt =
        serde_json::from_slice(&std::fs::read(&failed.receipt_path).expect("failed receipt"))
            .expect("decode failed receipt");
    assert_eq!(stored_failed.outcome, StorageAdminOutcome::Failed);
    assert_eq!(stored_failed.failure.as_deref(), Some("mount rejected"));
    assert!(stored_failed.cleanup_complete);

    let cancelled_invocation = invocation(&root.0, "operation-cancelled");
    let mut cancelled_lifecycle =
        FakeLifecycle::returning(StorageAdminExecution::cancelled("lease cancelled", false));
    let cancelled = run_storage_admin(&cancelled_invocation, &mut cancelled_lifecycle)
        .expect("cancelled operation still returns durable receipt");
    let stored_cancelled: sandbox_runtime_mpla_poc::StorageAdminReceipt =
        serde_json::from_slice(&std::fs::read(&cancelled.receipt_path).expect("cancelled receipt"))
            .expect("decode cancelled receipt");
    assert_eq!(stored_cancelled.outcome, StorageAdminOutcome::Cancelled);
    assert_eq!(stored_cancelled.failure.as_deref(), Some("lease cancelled"));
    assert!(!stored_cancelled.cleanup_complete);
}

#[test]
fn ordinary_command_and_workload_policy_has_no_mount_authority() {
    let ordinary = OrdinaryWorkloadPolicy;
    assert!(ordinary.effective_capabilities().is_empty());
    assert!(ordinary.allowed_privileged_syscalls().is_empty());
    assert!(ordinary.denies_syscall("mount"));
    assert!(ordinary.denies_syscall("umount2"));
    assert!(!ordinary.can_select_storage_admin_profile());

    let privileged = StorageAdminProcessProfile;
    assert_eq!(
        privileged.effective_capabilities(),
        STORAGE_ADMIN_EFFECTIVE_CAPABILITIES
    );
    assert_eq!(
        privileged.allowed_privileged_syscalls(),
        STORAGE_ADMIN_PRIVILEGED_SYSCALLS
    );
}

#[test]
fn helper_enters_the_bound_namespaces_before_narrowing_inherited_capabilities() {
    const CAP_NET_ADMIN_BIT: u64 = 1 << 12;
    const CAP_SYS_ADMIN_BIT: u64 = 1 << 21;

    let profile = StorageAdminProcessProfile;
    let daemon_inherited = CAP_SYS_ADMIN_BIT | CAP_NET_ADMIN_BIT;
    assert_ne!(daemon_inherited & CAP_NET_ADMIN_BIT, 0);
    assert_eq!(profile.effective_capability_mask(), CAP_SYS_ADMIN_BIT);
    assert_eq!(profile.permitted_capability_mask(), CAP_SYS_ADMIN_BIT);
    assert_eq!(profile.inheritable_capability_mask(), 0);
    assert_eq!(profile.ambient_capability_mask(), 0);
    assert_eq!(profile.effective_capability_mask() & CAP_NET_ADMIN_BIT, 0);
    assert_eq!(profile.permitted_capability_mask() & CAP_NET_ADMIN_BIT, 0);
    assert_eq!(
        profile.preparation_steps().get(6),
        Some(&StorageAdminPreparationStep::NarrowCapabilityMasks)
    );
    assert_eq!(
        profile.preparation_steps().get(7),
        Some(&StorageAdminPreparationStep::SetNoNewPrivileges)
    );
    assert_eq!(
        profile.preparation_steps().get(8),
        Some(&StorageAdminPreparationStep::VerifyExecutableAndCapabilityIdentity)
    );
}

#[test]
fn internal_security_evidence_captures_raw_process_and_exact_mount_plan() {
    let status = "\
CapInh:\t0000000000000000
CapPrm:\t0000000000200000
CapEff:\t0000000000200000
CapBnd:\t0000000000201000
CapAmb:\t0000000000000000
NoNewPrivs:\t1
Seccomp:\t2
Seccomp_filters:\t1
";
    let process = storage_admin_process_evidence_from_status(
        PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE),
        "00".repeat(32),
        status,
        PathBuf::from("/mpla-test/cgroup.procs"),
        NAMESPACE_HOLDER_PID,
        "mnt:[4026532999]".to_owned(),
        4_026_532_999,
    )
    .expect("parse raw process evidence");
    assert_eq!(process.capabilities.effective, 1 << 21);
    assert_eq!(process.capabilities.permitted, 1 << 21);
    assert_eq!(process.capabilities.inheritable, 0);
    assert_eq!(process.capabilities.bounding, (1 << 21) | (1 << 12));
    assert_eq!(process.capabilities.ambient, 0);
    assert_eq!(process.seccomp.profile_id, STORAGE_ADMIN_SECCOMP_PROFILE_ID);
    assert_eq!(process.seccomp.mode, 2);
    assert_eq!(process.seccomp.filter_count, 1);
    assert!(process.seccomp.no_new_privs);

    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-mount-evidence");
    let mount = storage_admin_mount_plan_evidence(&invocation.request.scope)
        .expect("capture exact mount plan");
    assert_eq!(mount.mount_namespace_id, "mnt:[4026532999]");
    assert_eq!(mount.source, "overlay");
    assert_eq!(mount.filesystem_type, "overlay");
    assert_eq!(mount.target, root.0.join("workspace"));
    assert_eq!(mount.flags, ["MS_NODEV", "MS_NOSUID"]);
    assert_eq!(
        mount.lower_dirs_newest_first,
        [root.0.join("payload/lower-1")]
    );
    assert_eq!(mount.upper_dir, root.0.join("allocation/upper"));
    assert_eq!(mount.work_dir, root.0.join("allocation/work"));
}

#[test]
fn storage_authority_cannot_become_or_spawn_the_workload() {
    let profile = StorageAdminProcessProfile;
    assert!(!profile.allows_workload_entry());
    assert!(!profile.allows_workload_descendants());
    assert!(!profile.allows_arbitrary_executable());
    assert_eq!(profile.effective_capabilities(), &["CAP_SYS_ADMIN"]);
    assert_eq!(
        profile.allowed_privileged_syscalls(),
        &["mount", "umount2", "setns", "syncfs"]
    );
}

#[test]
fn receipt_schema_captures_identity_security_scope_result_cleanup_and_time() {
    let root = TestRoot::new();
    let invocation = invocation(&root.0, "operation-receipt-schema");
    let mut lifecycle = FakeLifecycle::succeeding();
    let receipt = run_storage_admin(&invocation, &mut lifecycle).expect("receipt");
    let value = serde_json::to_value(&receipt).expect("encode receipt");
    let fields: BTreeSet<_> = value
        .as_object()
        .expect("receipt object")
        .keys()
        .map(String::as_str)
        .collect();
    let expected: BTreeSet<_> = [
        "schema_version",
        "interface_version",
        "profile_id",
        "operation_id",
        "action",
        "request_sha256",
        "trusted_executable",
        "effective_capabilities",
        "allowed_privileged_syscalls",
        "process_evidence",
        "mount_plan_evidence",
        "scope",
        "outcome",
        "idempotent_replay",
        "cleanup_complete",
        "failure",
        "started_unix_ms",
        "completed_unix_ms",
        "receipt_path",
    ]
    .into_iter()
    .collect();

    assert_eq!(fields, expected);
    assert_eq!(
        receipt.trusted_executable,
        PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    );
    assert_eq!(receipt.effective_capabilities, vec!["CAP_SYS_ADMIN"]);
    assert_eq!(
        receipt.allowed_privileged_syscalls,
        vec!["mount", "umount2", "setns", "syncfs"]
    );
    assert_eq!(receipt.scope, invocation.request.scope);
    assert_eq!(
        receipt.process_evidence.executable,
        PathBuf::from(STORAGE_ADMIN_TRUSTED_EXECUTABLE)
    );
    assert_eq!(
        receipt.mount_plan_evidence.target,
        invocation.request.scope.workspace_root
    );
    assert_eq!(receipt.outcome, StorageAdminOutcome::Succeeded);
    assert!(receipt.cleanup_complete);
    assert!(receipt.failure.is_none());
    assert!(receipt.started_unix_ms > 0);
    assert!(receipt.completed_unix_ms >= receipt.started_unix_ms);
    assert_eq!(receipt.request_sha256.len(), 64);
}
